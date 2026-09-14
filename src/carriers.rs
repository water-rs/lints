//! Value carriers — wrappers and calls that rewrap a value without
//! inspecting it (`&`, `*`, `.clone()`, `.to_owned()`, `.to_string()`,
//! `format!(..)`), shared by lints that trace where a value flows.

use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::TypeckResults;

use crate::param_bounds::call_args;

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

/// `true` when `expr` is a `Call`/`MethodCall` resolving to one of `paths`
/// under `typeck`.
pub(crate) fn is_call_to<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &Expr<'hir>,
    paths: &[&[&'static str]],
) -> bool {
    crate::param_bounds::call_def_id(typeck, expr).is_some_and(|did| {
        paths
            .iter()
            .any(|p| crate::def_path::def_path_eq(cx, did, p))
    })
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
