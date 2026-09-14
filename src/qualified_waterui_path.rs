use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::source::snippet_opt;
use rustc_data_structures::fx::FxHashSet;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{
    AmbigArg, Arm, Block, CRATE_HIR_ID, Expr, ForeignItem, HirId, ImplItem, Item, ItemKind, Mod,
    Node, Pat, Path, Stmt, StmtKind, TraitItem, Ty, UseKind, Variant,
};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::hir::nested_filter::All;
use rustc_middle::ty::TyCtxt;
use rustc_session::impl_lint_pass;
use rustc_span::symbol::{Symbol, kw};
use rustc_span::{BytePos, ExpnId, ExpnKind, MacroKind, Span};

use crate::imports::{Bare, bare_status, target_did, use_insertion};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags paths whose first segment resolves to the `waterui` crate root,
    /// a `waterui_*` member crate, or `nami` when the path is written out at
    /// the use site — in expressions, types, patterns, trait bounds, and
    /// `waterui::text!(..)`-style macro invocations — instead of imported
    /// with `use`. Paths inside `use` items are exempt.
    ///
    /// ### Why is this bad?
    ///
    /// WaterUI code is meant to read as bare names: the prelude and the
    /// facade modules exist so that `vstack`, `text!`, `Divider`, and
    /// friends are imported once, not requalified at every call.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// fn body() -> impl View {
    ///     waterui::layout::vstack((waterui::text!("hello"),))
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// use waterui::layout::vstack;
    /// use waterui::text;
    ///
    /// fn body() -> impl View {
    ///     vstack((text!("hello"),))
    /// }
    /// ```
    ///
    /// ### Known problems
    ///
    /// Every site of one path suggests the same `use` line. `cargo fix`
    /// applies the first and skips the rest of that pass as duplicates, so
    /// a module that repeats a path needs `cargo dylint --fix` run again —
    /// the second pass finds the name imported and only strips.
    pub QUALIFIED_WATERUI_PATH,
    style,
    "`waterui`/`nami` path written qualified instead of imported"
}

/// The lint pass. `expansions` deduplicates the `check_crate` sweep so each
/// macro invocation produces exactly one diagnostic.
#[derive(Default)]
pub struct QualifiedWateruiPath {
    expansions: FxHashSet<ExpnId>,
}

impl_lint_pass!(QualifiedWateruiPath => [QUALIFIED_WATERUI_PATH]);

/// `waterui`-family crate roots: the `waterui` facade, every `waterui_*`
/// member crate, and `nami`.
fn family_crate(name: Symbol) -> bool {
    name.as_str() == "waterui" || name.as_str() == "nami" || name.as_str().starts_with("waterui_")
}

/// Whether a `use` or `extern crate` item binds `name` directly to the crate
/// root `krate` — `use waterui as w;`, `extern crate nami as n;`.
fn item_aliases_crate(tcx: TyCtxt<'_>, item: &Item<'_>, name: Symbol, krate: DefId) -> bool {
    match item.kind {
        ItemKind::Use(path, UseKind::Single(ident)) => {
            let global = path
                .segments
                .first()
                .is_some_and(|seg| seg.ident.name == kw::PathRoot);
            ident.name == name
                && path.segments[usize::from(global)..].len() == 1
                && path.res.type_ns.and_then(|res| res.opt_def_id()) == Some(krate)
        }
        ItemKind::ExternCrate(_, ident) => {
            ident.name == name
                && tcx
                    .extern_mod_stmt_cnum(item.owner_id.def_id)
                    .is_some_and(|cnum| cnum.as_def_id() == krate)
        }
        _ => false,
    }
}

/// Whether a path head written as `name` and resolving to the crate root
/// `krate` spells the crate out: either `name` is the crate's own name, or a
/// `use`/`extern crate` in scope renames the crate to `name`. A crate root
/// that merely arrives under a module-like name through a re-export —
/// `locale` from the prelude, standing for `waterui_locale` — reads as a
/// module path and is left alone.
fn spells_crate(cx: &LateContext<'_>, hir_id: HirId, name: Symbol, krate: DefId) -> bool {
    let tcx = cx.tcx;
    if tcx.crate_name(krate.krate) == name {
        return true;
    }
    for (_, node) in tcx.hir_parent_iter(hir_id) {
        if let Node::Block(block) = node
            && block.stmts.iter().any(|stmt| {
                matches!(stmt.kind, StmtKind::Item(item_id)
                    if item_aliases_crate(tcx, tcx.hir_item(item_id), name, krate))
            })
        {
            return true;
        }
    }
    let module = tcx.parent_module(hir_id).to_local_def_id();
    let module: &Mod<'_> = match tcx.hir_node(tcx.local_def_id_to_hir_id(module)) {
        Node::Crate(module) => module,
        Node::Item(Item {
            kind: ItemKind::Mod(_, module),
            ..
        }) => module,
        _ => return false,
    };
    module
        .item_ids
        .iter()
        .any(|&item_id| item_aliases_crate(tcx, tcx.hir_item(item_id), name, krate))
}

/// Whether `use <path>` can legally import what `res` points at. Associated
/// items (`waterui::x::Type::method`) are not importable — the `use` must
/// stop at the owning item, so the qualifier is stripped only that far.
fn importable(res: Res) -> bool {
    matches!(
        res,
        Res::Def(
            DefKind::ExternCrate
                | DefKind::Mod
                | DefKind::Struct
                | DefKind::Union
                | DefKind::Enum
                | DefKind::Variant
                | DefKind::Trait
                | DefKind::TraitAlias
                | DefKind::TyAlias
                | DefKind::ForeignTy
                | DefKind::OpaqueTy
                | DefKind::Fn
                | DefKind::Const { .. }
                | DefKind::Static { .. }
                | DefKind::Macro(..)
                | DefKind::Ctor(..)
                | DefKind::Use,
            _,
        )
    )
}

/// A qualified WaterUI-family path ready to be diagnosed: where it is
/// written, what a `use` would import, and the span of qualifier to strip.
struct Flag<'a> {
    /// The span the diagnostic underlines.
    span: Span,
    /// `[path_lo, bare_lo)` — everything before the imported name.
    strip: Span,
    /// `[path_lo, bare_hi)` — qualifier plus the imported name; the alias
    /// fix replaces this whole span so `waterui::x::row` becomes `x_row`.
    cover: Span,
    /// Source text of the flagged path for the message.
    written: String,
    /// `use <this>;` — the written segment text up to and including `bare`.
    use_path: &'a str,
    /// The name the stripped path leaves behind.
    bare: Symbol,
    /// The namespace `bare` resolves in.
    ns: Namespace,
    /// The item `bare` must already resolve to for a strip-only fix.
    target: Option<DefId>,
    /// The segment before `bare`, used to seed an alias (`list` →
    /// `list_row`).
    penult: Symbol,
    /// A node at the flagged position — anchors module and scope lookup.
    hir_id: HirId,
}

fn report(cx: &LateContext<'_>, flag: &Flag<'_>) {
    let status = bare_status(
        cx,
        flag.hir_id,
        flag.span.lo(),
        flag.bare,
        flag.ns,
        flag.target,
    );
    let msg = format!("`{}` can be imported and used unqualified", flag.written);
    match status {
        Bare::Same => {
            span_lint_and_then(cx, QUALIFIED_WATERUI_PATH, flag.span, msg.clone(), |diag| {
                diag.multipart_suggestion(
                    format!("`{}` is already in scope", flag.bare),
                    vec![(flag.strip, String::new())],
                    Applicability::MachineApplicable,
                );
            })
        }
        Bare::Free | Bare::Conflict | Bare::Unknown => {
            let Some((point, before, after)) = use_insertion(cx, flag.hir_id, flag.use_path) else {
                return;
            };
            let (label, use_text, strip_span, strip_text) = match status {
                Bare::Free => (
                    format!("import `{}`", flag.use_path),
                    format!("{before}use {};{after}", flag.use_path),
                    flag.strip,
                    String::new(),
                ),
                _ => {
                    let alias = format!("{}_{}", flag.penult, flag.bare);
                    (
                        format!(
                            "`{}` resolves to a different item here — import under an alias",
                            flag.bare
                        ),
                        format!("{before}use {} as {alias};{after}", flag.use_path),
                        flag.cover,
                        alias,
                    )
                }
            };
            span_lint_and_then(cx, QUALIFIED_WATERUI_PATH, flag.span, msg, |diag| {
                diag.multipart_suggestion(
                    label,
                    vec![(point, use_text), (strip_span, strip_text)],
                    match status {
                        Bare::Free => Applicability::MachineApplicable,
                        _ => Applicability::MaybeIncorrect,
                    },
                );
            });
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for QualifiedWateruiPath {
    /// Resolved HIR paths: expressions, types, patterns, trait bounds,
    /// generics — everything except macro invocation paths, which lowering
    /// consumes, and `use` paths, which are already imports.
    fn check_path(&mut self, cx: &LateContext<'tcx>, path: &Path<'tcx>, hir_id: HirId) {
        if let Node::Item(item) = cx.tcx.hir_node(hir_id)
            && matches!(item.kind, ItemKind::Use(..))
        {
            return;
        }
        if path.span.from_expansion() {
            return;
        }
        // `::waterui::x` lowers with a leading `{{root}}` segment.
        let base = usize::from(path.is_global());
        let segs = &path.segments[base..];
        if segs.len() < 2 {
            return;
        }
        let Some(did) = segs[0].res.opt_def_id() else {
            return;
        };
        if !did.is_crate_root() || did.is_local() {
            return;
        }
        if !family_crate(cx.tcx.crate_name(did.krate))
            || !spells_crate(cx, hir_id, segs[0].ident.name, did)
        {
            return;
        }
        // The `use` can only reach an importable item: for
        // `waterui::x::Type::assoc` it stops at `Type`, leaving `Type::assoc`.
        let Some(item) = segs[1..]
            .iter()
            .rposition(|seg| importable(seg.res))
            .map(|i| i + 1)
        else {
            return;
        };
        let use_path = segs[..=item]
            .iter()
            .map(|seg| seg.ident.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        report(
            cx,
            &Flag {
                span: path.span,
                strip: path.span.with_hi(segs[item].ident.span.lo()),
                cover: path.span.with_hi(segs[item].ident.span.hi()),
                written: snippet_opt(cx.sess(), path.span).unwrap_or_else(|| use_path.clone()),
                use_path: &use_path,
                bare: segs[item].ident.name,
                ns: segs[item].res.ns().unwrap_or(Namespace::TypeNS),
                target: target_did(cx.tcx, segs[item].res),
                penult: segs[item - 1].ident.name,
                hir_id,
            },
        );
    }

    /// Bang-macro invocations: `waterui::text!(..)` produces no resolvable
    /// HIR path — the call's tokens are consumed by expansion — so the sweep
    /// finds each expansion mark and recovers the source invocation.
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        let Node::Crate(module) = cx.tcx.hir_node(CRATE_HIR_ID) else {
            return;
        };
        intravisit::walk_mod(
            &mut ExpansionSweep {
                cx,
                seen: &mut self.expansions,
            },
            module,
        );
    }
}

/// Walks the whole crate and checks every node's outermost expansion mark:
/// a `MacroKind::Bang` mark whose call site is real source is a source-level
/// `a::b!` invocation. `seen` deduplicates — every node of one expansion
/// shares the mark.
struct ExpansionSweep<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    seen: &'a mut FxHashSet<ExpnId>,
}

impl<'tcx> ExpansionSweep<'_, 'tcx> {
    fn check_span(&mut self, hir_id: HirId, span: Span) {
        let expn = span.ctxt().outer_expn();
        if expn == ExpnId::root() || !self.seen.insert(expn) {
            return;
        }
        let data = expn.expn_data();
        if !matches!(data.kind, ExpnKind::Macro(MacroKind::Bang, _))
            || data.call_site.from_expansion()
            || data.call_site.is_dummy()
        {
            return;
        }
        let call_site = data.call_site;
        let Some(source) = snippet_opt(self.cx.sess(), call_site) else {
            return;
        };
        let Some(bang) = source.find('!') else {
            return;
        };
        // The path sub-span inside the call site: leading whitespace and the
        // `!(args)` tail are excluded.
        let path_start = source.len() - source.trim_start().len();
        let path_end = source[..bang].trim_end().len();
        if path_start >= path_end {
            return;
        }
        let raw = &source[path_start..path_end];
        let global = usize::from(raw.starts_with("::"));
        let raw = raw.strip_prefix("::").unwrap_or(raw);
        let segs: Vec<&str> = raw.split("::").collect();
        if segs.len() < 2 || segs.iter().any(|seg| !is_path_ident(seg)) {
            return;
        }
        let Some(did) = macro_seg0(self.cx, hir_id, segs[0]) else {
            return;
        };
        if !family_crate(self.cx.tcx.crate_name(did.krate))
            || !spells_crate(self.cx, hir_id, Symbol::intern(segs[0]), did)
        {
            return;
        }
        let lo = call_site.lo();
        let path_lo = lo + BytePos((path_start + 2 * global) as u32);
        let path_span = call_site
            .with_lo(path_lo)
            .with_hi(lo + BytePos(path_end as u32));
        // Strip through the final `::` — the macro name stays behind. The
        // strip starts at `path_start` so a leading `::` goes too.
        let last_sep = raw.rfind("::").unwrap_or(0);
        let strip = call_site
            .with_lo(lo + BytePos(path_start as u32))
            .with_hi(path_lo + BytePos(last_sep as u32 + 2));
        report(
            self.cx,
            &Flag {
                span: path_span,
                strip,
                cover: call_site
                    .with_lo(lo + BytePos(path_start as u32))
                    .with_hi(lo + BytePos(path_end as u32)),
                written: format!("{raw}!"),
                use_path: raw,
                bare: Symbol::intern(segs[segs.len() - 1]),
                ns: Namespace::MacroNS,
                target: data.macro_def_id,
                penult: Symbol::intern(segs[segs.len() - 2]),
                hir_id,
            },
        );
    }
}

impl<'tcx> Visitor<'tcx> for ExpansionSweep<'_, 'tcx> {
    type MaybeTyCtxt = TyCtxt<'tcx>;
    type NestedFilter = All;

    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.cx.tcx
    }

    fn visit_item(&mut self, item: &'tcx Item<'tcx>) {
        self.check_span(item.hir_id(), item.span);
        intravisit::walk_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'tcx ImplItem<'tcx>) {
        self.check_span(item.hir_id(), item.span);
        intravisit::walk_impl_item(self, item);
    }

    fn visit_trait_item(&mut self, item: &'tcx TraitItem<'tcx>) {
        self.check_span(item.hir_id(), item.span);
        intravisit::walk_trait_item(self, item);
    }

    fn visit_foreign_item(&mut self, item: &'tcx ForeignItem<'tcx>) {
        self.check_span(item.hir_id(), item.span);
        intravisit::walk_foreign_item(self, item);
    }

    fn visit_variant(&mut self, variant: &'tcx Variant<'tcx>) {
        self.check_span(variant.hir_id, variant.span);
        intravisit::walk_variant(self, variant);
    }

    fn visit_stmt(&mut self, stmt: &'tcx Stmt<'tcx>) {
        self.check_span(stmt.hir_id, stmt.span);
        intravisit::walk_stmt(self, stmt);
    }

    fn visit_block(&mut self, block: &'tcx Block<'tcx>) {
        self.check_span(block.hir_id, block.span);
        intravisit::walk_block(self, block);
    }

    fn visit_arm(&mut self, arm: &'tcx Arm<'tcx>) {
        self.check_span(arm.hir_id, arm.span);
        intravisit::walk_arm(self, arm);
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        self.check_span(expr.hir_id, expr.span);
        intravisit::walk_expr(self, expr);
    }

    fn visit_pat(&mut self, pat: &'tcx Pat<'tcx>) {
        self.check_span(pat.hir_id, pat.span);
        intravisit::walk_pat(self, pat);
    }

    fn visit_ty(&mut self, ty: &'tcx Ty<'tcx, AmbigArg>) {
        self.check_span(ty.hir_id, ty.span);
        intravisit::walk_ty(self, ty);
    }
}

/// Whether `s` is a plausible single path segment — `waterui`, `log`,
/// `debug` — so `a::b!` text can be trusted as a path.
fn is_path_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Resolves the first segment of a written macro path — `waterui` in
/// `waterui::log::debug!`, `w` in `w::text!`. Module-level `use`-aliases and
/// `extern crate` items come from the module's name table; otherwise the
/// name is matched against the dependency graph's crate names.
fn macro_seg0(cx: &LateContext<'_>, hir_id: HirId, name: &str) -> Option<DefId> {
    let tcx = cx.tcx;
    if matches!(name, "crate" | "self" | "Self" | "super") {
        return None;
    }
    let name = Symbol::intern(name);
    let module = tcx.parent_module(hir_id).to_local_def_id();
    if let Some(children) = tcx.resolutions(()).module_children.get(&module)
        && let Some(child) = children.iter().find(|child| child.ident.name == name)
    {
        return child.res.opt_def_id().filter(|did| did.is_crate_root());
    }
    tcx.crates(())
        .iter()
        .find(|&&krate| tcx.crate_name(krate) == name)
        .map(|krate| krate.as_def_id())
}
