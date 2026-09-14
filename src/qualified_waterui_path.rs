use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::source::{snippet_indent, snippet_opt};
use rustc_data_structures::fx::FxHashSet;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{
    AmbigArg, Arm, Block, CRATE_HIR_ID, Expr, ExprKind, ForeignItem, HirId, ImplItem, ImplItemKind,
    Item, ItemId, ItemKind, Mod, Node, Pat, Path, Stmt, StmtKind, TraitItem, TraitItemKind, Ty,
    UseKind, Variant,
};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::hir::nested_filter::All;
use rustc_middle::ty::{GenericParamDefKind, TyCtxt};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::{Symbol, kw};
use rustc_span::{BytePos, ExpnId, ExpnKind, MacroKind, Span};

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

/// The `DefId` a bare `use`-bound name resolves to, normalized so a
/// constructor and its parent ADT/variant compare equal.
fn target_did<Id>(tcx: TyCtxt<'_>, res: Res<Id>) -> Option<DefId> {
    let did = res.opt_def_id()?;
    if matches!(tcx.def_kind(did), DefKind::Ctor(..)) {
        Some(tcx.parent(did))
    } else {
        Some(did)
    }
}

/// Whether `pat` binds `name` (`vstack`, `(a, vstack)`, `Some(vstack)`, …).
fn pat_binds(pat: &Pat<'_>, name: Symbol) -> bool {
    let mut found = false;
    pat.each_binding(|_, _, _, ident| found |= ident.name == name);
    found
}

/// Pattern bindings only shadow value-namespace names (`let vstack = ..`,
/// `|vstack| ..`, `Some(vstack)`).
fn value_pat_binds(pat: &Pat<'_>, name: Symbol, ns: Namespace) -> bool {
    ns == Namespace::ValueNS && pat_binds(pat, name)
}

/// Whether any generic parameter of `owner` is named `name` in namespace
/// `ns` — a `fn f<VStack>(..)` makes a later bare `VStack` mean the type
/// parameter, not the import.
fn generics_shadow(tcx: TyCtxt<'_>, owner: LocalDefId, name: Symbol, ns: Namespace) -> bool {
    tcx.generics_of(owner).own_params.iter().any(|param| {
        param.name == name
            && match param.kind {
                GenericParamDefKind::Type { .. } => ns == Namespace::TypeNS,
                GenericParamDefKind::Const { .. } => ns == Namespace::ValueNS,
                GenericParamDefKind::Lifetime => false,
            }
    })
}

/// Whether the parameters of `body` bind `name` — only meaningful for
/// value-namespace lookups.
fn params_bind(tcx: TyCtxt<'_>, body: rustc_hir::BodyId, name: Symbol, ns: Namespace) -> bool {
    ns == Namespace::ValueNS
        && tcx
            .hir_body(body)
            .params
            .iter()
            .any(|param| pat_binds(param.pat, name))
}

/// What a bare name would resolve to at the flagged position — decides the
/// shape and applicability of the fix.
enum Bare {
    /// Nothing binds the name — `use` the full path and strip the qualifier.
    Free,
    /// The name already resolves to the very same item — strip only, no
    /// import is emitted.
    Same,
    /// The name resolves to something else — suggest an `as` alias.
    Conflict,
    /// A function-local `use ..::*` could resolve the name either way —
    /// suggest the safe alias form, marked `MaybeIncorrect`.
    Unknown,
}

/// An item-level name binding found while walking lexical scopes.
enum ItemCheck {
    Free,
    Glob,
    Same,
    Conflict,
}

/// A `use` or ordinary item inside a block. `use a::*` is `Glob` (a
/// potential binding whose resolution cannot be enumerated here), an
/// explicit `use a::name` reports `Same`/`Conflict`, and any other item
/// named `name` in the right namespace is a `Conflict`.
fn check_block_item(
    tcx: TyCtxt<'_>,
    item_id: ItemId,
    name: Symbol,
    ns: Namespace,
    target: Option<DefId>,
) -> ItemCheck {
    let item = tcx.hir_item(item_id);
    if let ItemKind::Use(use_path, kind) = item.kind {
        return match kind {
            UseKind::Glob => ItemCheck::Glob,
            UseKind::Single(ident) if ident.name == name => {
                let res = match ns {
                    Namespace::TypeNS => use_path.res.type_ns,
                    Namespace::ValueNS => use_path.res.value_ns,
                    Namespace::MacroNS => use_path.res.macro_ns,
                };
                match res {
                    Some(res) if target_did(tcx, res) == target => ItemCheck::Same,
                    Some(_) => ItemCheck::Conflict,
                    None => ItemCheck::Free,
                }
            }
            _ => ItemCheck::Free,
        };
    }
    if tcx
        .opt_item_name(item.owner_id.to_def_id())
        .is_some_and(|item_name| item_name == name)
        && tcx.def_kind(item.owner_id.def_id).ns() == Some(ns)
    {
        return ItemCheck::Conflict;
    }
    ItemCheck::Free
}

/// Resolves `name` as it would resolve at `hir_id` — lexical bindings
/// (params, `let`s, block items, generic params) first, then the enclosing
/// module's name table, which already accounts for `use`s, glob imports,
/// and the prelude.
fn bare_status(
    cx: &LateContext<'_>,
    hir_id: HirId,
    pos: BytePos,
    name: Symbol,
    ns: Namespace,
    target: Option<DefId>,
) -> Bare {
    let tcx = cx.tcx;
    let mut glob = false;
    // Set by `Node::Block`: params/generics of the enclosing item scope over
    // the body's `waterui::x` use, but not over the signature itself.
    let mut in_body = false;
    // Set by `Node::Pat`: a match arm's bindings do not scope over their own
    // pattern, and an `if let` pattern's bindings do not scope over it.
    let mut in_pat = false;
    // Set by `ExprKind::Let`: the let-pat's bindings do not scope over the
    // pattern position itself.
    let mut inside_let_cond = false;
    for (_, node) in tcx.hir_parent_iter(hir_id) {
        match node {
            Node::Block(block) => {
                in_body = true;
                for stmt in block.stmts {
                    match stmt.kind {
                        StmtKind::Let(local) if local.span.hi() <= pos => {
                            if value_pat_binds(local.pat, name, ns) {
                                return Bare::Conflict;
                            }
                        }
                        StmtKind::Item(item_id) => {
                            match check_block_item(tcx, item_id, name, ns, target) {
                                ItemCheck::Free => {}
                                ItemCheck::Glob => glob = true,
                                ItemCheck::Same => return Bare::Same,
                                ItemCheck::Conflict => return Bare::Conflict,
                            }
                        }
                        _ => {}
                    }
                }
            }
            Node::Arm(arm) => {
                if !in_pat && value_pat_binds(arm.pat, name, ns) {
                    return Bare::Conflict;
                }
            }
            Node::Pat(_) => in_pat = true,
            Node::Expr(expr) => match expr.kind {
                ExprKind::Closure(closure) => {
                    if in_body && params_bind(tcx, closure.body, name, ns) {
                        return Bare::Conflict;
                    }
                }
                ExprKind::If(cond, ..) => {
                    if !inside_let_cond
                        && let ExprKind::Let(let_expr) = cond.kind
                        && value_pat_binds(let_expr.pat, name, ns)
                    {
                        return Bare::Conflict;
                    }
                }
                ExprKind::Let(..) => inside_let_cond = true,
                _ => {}
            },
            // A `let`'s pattern does not scope over its own type annotation
            // or initializer; a parameter's pattern does not scope over its
            // own type.
            Node::LetStmt(_) | Node::Param(_) => {}
            Node::Item(item) => {
                if generics_shadow(tcx, item.owner_id.def_id, name, ns) {
                    return Bare::Conflict;
                }
                if in_body
                    && let ItemKind::Fn { body, .. } = item.kind
                    && params_bind(tcx, body, name, ns)
                {
                    return Bare::Conflict;
                }
                break;
            }
            Node::ImplItem(item) => {
                if generics_shadow(tcx, item.owner_id.def_id, name, ns) {
                    return Bare::Conflict;
                }
                if in_body
                    && let ImplItemKind::Fn(_, body) = item.kind
                    && params_bind(tcx, body, name, ns)
                {
                    return Bare::Conflict;
                }
            }
            Node::TraitItem(item) => {
                if generics_shadow(tcx, item.owner_id.def_id, name, ns) {
                    return Bare::Conflict;
                }
                if in_body
                    && let TraitItemKind::Fn(_, rustc_hir::TraitFn::Provided(body)) = item.kind
                    && params_bind(tcx, body, name, ns)
                {
                    return Bare::Conflict;
                }
            }
            Node::ForeignItem(item) => {
                if generics_shadow(tcx, item.owner_id.def_id, name, ns) {
                    return Bare::Conflict;
                }
            }
            Node::Crate(_) => break,
            _ => {}
        }
    }

    let module = tcx.parent_module(hir_id).to_local_def_id();
    let resolutions = tcx.resolutions(());
    let mut conflict = false;
    if let Some(children) = resolutions.module_children.get(&module) {
        for child in children
            .iter()
            .filter(|child| child.ident.name == name && child.res.ns() == Some(ns))
        {
            if target_did(tcx, child.res) == target {
                return Bare::Same;
            }
            conflict = true;
        }
    }
    if let Some(ambig) = resolutions.ambig_module_children.get(&module)
        && ambig.iter().any(|a| {
            a.main.ident.name == name
                && (a.main.res.ns() == Some(ns) || a.second.res.ns() == Some(ns))
        })
    {
        return Bare::Conflict;
    }
    if conflict {
        Bare::Conflict
    } else if glob {
        Bare::Unknown
    } else {
        Bare::Free
    }
}

/// Where `use <use_path>;` goes in `hir_id`'s module — `(span, before,
/// after)` so the suggestion text is `format!("{before}use {path};{after}")`.
///
/// The new `use` lands in sorted position among the module's existing
/// `use` items: after the last one whose path sorts before `use_path`, or
/// before the first one. Distinct import suggestions therefore spread over
/// distinct insertion points, which matters for `cargo fix`: rustfix
/// rejects two different insertions at one byte offset, and cargo retries
/// the leftovers only a few times per run. A module without any `use` gets
/// the new one at its `inject_use_span`.
fn use_insertion(
    cx: &LateContext<'_>,
    hir_id: HirId,
    use_path: &str,
) -> Option<(Span, String, String)> {
    let tcx = cx.tcx;
    let module_did = tcx.parent_module(hir_id).to_local_def_id();
    let module_hir = tcx.local_def_id_to_hir_id(module_did);
    let module: &Mod<'_> = match tcx.hir_node(module_hir) {
        Node::Crate(module) => module,
        Node::Item(item) => match item.kind {
            ItemKind::Mod(_, module) => module,
            _ => return None,
        },
        _ => return None,
    };
    // `item_ids` is not guaranteed to be in source order.
    let mut uses: Vec<(Span, String)> = module
        .item_ids
        .iter()
        .map(|&item_id| tcx.hir_item(item_id))
        .filter(|item| matches!(item.kind, ItemKind::Use(..)) && !item.span.from_expansion())
        .filter_map(|item| {
            let text = snippet_opt(cx.sess(), item.span)?;
            // Compare on the path only: `pub use a::b;` sorts as `a::b`.
            let path = text
                .split_once("use ")
                .map_or(text.as_str(), |(_, rest)| rest);
            Some((item.span, path.trim_end_matches(';').trim().to_owned()))
        })
        .collect();
    uses.sort_by_key(|(span, _)| span.lo());
    if uses.is_empty() {
        let span = module.spans.inject_use_span;
        if span.from_expansion() || span.is_dummy() {
            return None;
        }
        // `indent_of` finds no non-blank character in the zero-width
        // `inject_use_span`'s line prefix; the prefix itself is the indent.
        let indent = snippet_indent(cx.sess(), span).unwrap_or_default();
        return Some((span.shrink_to_lo(), String::new(), format!("\n{indent}")));
    }
    let predecessor = uses
        .iter()
        .filter(|(_, path)| path.as_str() <= use_path)
        .max_by_key(|(span, _)| span.lo());
    Some(match predecessor {
        Some((span, _)) => {
            let indent = snippet_indent(cx.sess(), *span).unwrap_or_default();
            (span.shrink_to_hi(), format!("\n{indent}"), String::new())
        }
        None => {
            let (span, _) = &uses[0];
            let indent = snippet_indent(cx.sess(), *span).unwrap_or_default();
            (span.shrink_to_lo(), String::new(), format!("\n{indent}"))
        }
    })
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
