//! Import-aware helpers for lints whose fix writes a `use` item:
//! `bare_status` resolves what a bare name would mean at the flagged
//! position, and `use_insertion` picks where in the enclosing module the new
//! `use` line lands.

use clippy_utils::source::{snippet_indent, snippet_opt};
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::{
    BodyId, ExprKind, HirId, ImplItemKind, ItemId, ItemKind, Mod, Node, Pat, StmtKind, TraitFn,
    TraitItemKind, UseKind,
};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{GenericParamDefKind, TyCtxt};
use rustc_span::symbol::Symbol;
use rustc_span::{BytePos, Span};

/// The `DefId` a bare `use`-bound name resolves to, normalized so a
/// constructor and its parent ADT/variant compare equal.
pub(crate) fn target_did<Id>(tcx: TyCtxt<'_>, res: Res<Id>) -> Option<DefId> {
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
fn params_bind(tcx: TyCtxt<'_>, body: BodyId, name: Symbol, ns: Namespace) -> bool {
    ns == Namespace::ValueNS
        && tcx
            .hir_body(body)
            .params
            .iter()
            .any(|param| pat_binds(param.pat, name))
}

/// What a bare name would resolve to at the flagged position — decides the
/// shape and applicability of the fix.
pub(crate) enum Bare {
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
pub(crate) fn bare_status(
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
                    && let TraitItemKind::Fn(_, TraitFn::Provided(body)) = item.kind
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
pub(crate) fn use_insertion(
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
