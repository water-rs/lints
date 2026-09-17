//! `nami`'s `data::collection::List` — def paths and call-shape helpers
//! shared by the lints that match its `snapshot()`/`replace()` calls.

use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::TypeckResults;

use crate::def_path::def_path_eq;

/// `nami::data::collection::List::snapshot` — the inherent `Vec<T>` clone.
const LIST_SNAPSHOT: &[&str] = &["nami", "data", "collection", "List", "snapshot"];

/// `nami::data::collection::List::replace` — the whole-vector republish that
/// returns the previous contents.
const LIST_REPLACE: &[&str] = &["nami", "data", "collection", "List", "replace"];

/// `expr` is `x.snapshot()` resolving to `List::snapshot` — returns `x`.
/// `typeck` must be the `TypeckResults` of the body containing `expr`.
pub(crate) fn snapshot_receiver<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    let ExprKind::MethodCall(segment, receiver, [], _) = expr.kind else {
        return None;
    };
    if expr.span.from_expansion() || segment.ident.name.as_str() != "snapshot" {
        return None;
    }
    let did = typeck.type_dependent_def_id(expr.hir_id)?;
    def_path_eq(cx, did, LIST_SNAPSHOT).then_some(receiver)
}

/// `expr` is `x.replace(arg)` resolving to `List::replace` — returns
/// `(x, arg)`. `typeck` must be the `TypeckResults` of the body containing
/// `expr`.
pub(crate) fn replace_call<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<(&'hir Expr<'hir>, &'hir Expr<'hir>)> {
    let ExprKind::MethodCall(segment, receiver, [arg], _) = expr.kind else {
        return None;
    };
    if expr.span.from_expansion() || segment.ident.name.as_str() != "replace" {
        return None;
    }
    let did = typeck.type_dependent_def_id(expr.hir_id)?;
    def_path_eq(cx, did, LIST_REPLACE).then_some((receiver, arg))
}
