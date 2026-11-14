//! Value carriers — wrappers and calls that rewrap a value without
//! inspecting it (`&`, `*`, `.clone()`, `.to_owned()`, `.to_string()`,
//! `format!(..)`), shared by lints that trace where a value flows.

use clippy_utils::macros::{root_macro_call, root_macro_call_first_node};
use rustc_hir::{Block, Expr, ExprKind, HirId, Node, UnOp};
use rustc_lint::LateContext;
use rustc_middle::ty::{Ty, TyKind, TypeckResults};
use rustc_span::{ExpnId, sym};

use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};

/// `ToOwned::to_owned` — `text!` wraps every captured slot value in
/// `(<expr>).to_owned()`, which is also how a `text!` alias binding is
/// recognized.
pub(crate) const TO_OWNED: &[&str] = &["alloc", "borrow", "ToOwned", "to_owned"];

/// `ToString::to_string` — `|v| v.to_string()`.
pub(crate) const TO_STRING: &[&str] = &["alloc", "string", "ToString", "to_string"];

/// `Clone::clone` — `|v| v.clone()` over a `Str`/`String` parameter, and
/// `.clone()` receivers/arguments the analysis strips.
pub(crate) const CLONE: &[&str] = &["core", "clone", "Clone", "clone"];

/// Calls that carry a value into a new shape without reading it — the
/// carrier calls `watch_for_reactive_value` climbs through on the way to a
/// signal-taking parameter.
pub(crate) const CARRIER_CALLS: &[&[&str]] = &[CLONE, TO_OWNED, TO_STRING];

/// `From::from` — `String::from(..)` / `Str::from(..)` wrappers.
pub(crate) const FROM: &[&str] = &["core", "convert", "From", "from"];

/// `Into::into` — `x.into()` / `Into::<T>::into(x)` conversions.
pub(crate) const INTO: &[&str] = &["core", "convert", "Into", "into"];

/// `Text::verbatim` — marks the text as never translated.
pub(crate) const TEXT_VERBATIM: &[&str] = &["waterui_text", "text", "Text", "verbatim"];

/// String-shaped types a `From`/`Into` conversion may produce.
pub(crate) const STRING_TYS: &[&[&str]] = &[&["alloc", "string", "String"], &["suiteki", "Str"]];

/// `String` / `Str` / `&str` — the string shape a `From`/`Into` conversion
/// or a `.to_string()`/`.clone()` result must have to count as text.
pub(crate) fn is_string_ty(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    match ty.peel_refs().kind() {
        TyKind::Str => true,
        TyKind::Adt(def, _) => STRING_TYS
            .iter()
            .any(|p| crate::def_path::def_path_eq(cx, def.did(), p)),
        _ => false,
    }
}

/// `true` when `expr` is a `Call`/`MethodCall` resolving to one of `paths`
/// under `typeck`.
pub(crate) fn is_call_to<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &Expr<'hir>,
    paths: &[&[&'static str]],
) -> bool {
    call_def_id(typeck, expr).is_some_and(|did| {
        paths
            .iter()
            .any(|p| crate::def_path::def_path_eq(cx, did, p))
    })
}

/// [`is_call_to`] with trait-impl normalization: a `Str::from(..)`-style
/// path call resolves to the impl's method, whose def path is
/// `<impl From<..> for Str>::from` — [`implemented_trait_item`] maps it back
/// onto `core::convert::From::from` so the trait path matches.
pub(crate) fn resolves_to<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &Expr<'hir>,
    paths: &[&[&'static str]],
) -> bool {
    call_def_id(typeck, expr).is_some_and(|did| {
        let item = implemented_trait_item(cx.tcx, did);
        paths
            .iter()
            .any(|path| crate::def_path::def_path_eq(cx, item, path))
    })
}

/// The outermost expression of macro expansion `expn` containing `hir_id` —
/// the ancestor `root_macro_call_first_node` recognizes as the call's first
/// node. `None` when the expansion's root cannot be found.
fn expansion_root<'hir>(
    cx: &LateContext<'hir>,
    hir_id: HirId,
    expn: ExpnId,
) -> Option<&'hir Expr<'hir>> {
    cx.tcx.hir_parent_id_iter(hir_id).find_map(|id| {
        let Node::Expr(expr) = cx.tcx.hir_node(id) else {
            return None;
        };
        root_macro_call_first_node(cx, expr)
            .is_some_and(|call| call.expn == expn)
            .then_some(expr)
    })
}

/// `Some((call, index))` when `read`, after climbing through value carriers —
/// `.clone()`/`.to_owned()`/`.to_string()` at operand position 0, `&`/`*`,
/// drop-temps, and a `format!(..)` invocation it is an argument of — sits at
/// argument `index` of `call`. `None` when the value is consumed any other
/// way: a `match`/`if` scrutinee, a `let` init, a field, the callee, a
/// non-carrier call like `.len()`.
pub(crate) fn carried_arg<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    read: &'hir Expr<'hir>,
) -> Option<(&'hir Expr<'hir>, usize)> {
    let mut current = read;
    loop {
        let parent_id = cx.tcx.parent_hir_id(current.hir_id);
        // `current` inside a `format!(..)` expansion is a format argument;
        // the value carried onward is the expansion's outermost node.
        if let Some(call) = root_macro_call(cx.tcx.hir_span(parent_id))
            && cx.tcx.get_diagnostic_name(call.def_id) == Some(sym::format_macro)
        {
            current = expansion_root(cx, parent_id, call.expn)?;
            continue;
        }
        let Node::Expr(parent) = cx.tcx.hir_node(parent_id) else {
            return None;
        };
        match parent.kind {
            ExprKind::AddrOf(.., inner)
            | ExprKind::Unary(UnOp::Deref, inner)
            | ExprKind::DropTemps(inner)
                if inner.hir_id == current.hir_id =>
            {
                current = parent;
            }
            ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                let args = call_args(parent);
                let index = args.iter().position(|arg| arg.hir_id == current.hir_id)?;
                if index == 0 && is_call_to(cx, typeck, parent, CARRIER_CALLS) {
                    current = parent;
                } else {
                    return Some((parent, index));
                }
            }
            _ => return None,
        }
    }
}

/// `&x`, `x.clone()`, drop-temps — wrappers a map receiver or a `zip`
/// argument may carry; the analysis looks through them.
pub(crate) fn strip_wraps<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::AddrOf(.., inner) | ExprKind::DropTemps(inner) => inner,
            _ if is_call_to(cx, typeck, expr, &[CLONE]) => match call_args(expr).as_slice() {
                [first, ..] => *first,
                [] => return expr,
            },
            _ => return expr,
        };
    }
}

/// `format!(..).into()`, `String::from(format!(..))`, `{ format!(..) }` —
/// tail conversions/blocks around a produced string are transparent to the
/// lints that inspect how a `String`/`Str` value was built.
pub(crate) fn strip_tail<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(
                Block {
                    stmts: [],
                    expr: Some(inner),
                    ..
                },
                _,
            ) => inner,
            _ if resolves_to(cx, typeck, expr, &[INTO, FROM])
                && is_string_ty(cx, typeck.expr_ty(expr)) =>
            {
                match call_args(expr).as_slice() {
                    [first, ..] => *first,
                    [] => return expr,
                }
            }
            _ => return expr,
        };
    }
}

/// The expression a body actually evaluates — `expr` with drop-temps and
/// statement-free block tails removed: `{ x }` peels to `x`, while a block
/// with statements stays whole (its statements do work a rewrite would
/// drop).
pub(crate) fn body_expr<'hir>(mut expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(
                Block {
                    stmts: [],
                    expr: Some(tail),
                    ..
                },
                _,
            ) => tail,
            _ => return expr,
        };
    }
}
