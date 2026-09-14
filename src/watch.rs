//! Recognition of `watch` calls — the `waterui::dynamic::watch` free function
//! and the `Dynamic::watch` associated function — shared by the lints that
//! inspect them.

use clippy_utils::res::MaybeQPath;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Closure, Expr, ExprKind};
use rustc_lint::LateContext;

/// `waterui_core::components::dynamic::watch` — the free `watch(value, f)`.
const WATCH_FN: &[&str] = &["waterui_core", "components", "dynamic", "watch"];

/// `waterui_core::components::dynamic::Dynamic::watch` — the associated
/// `Dynamic::watch(value, f)`. It has no `self` parameter, so it only ever
/// arrives as `ExprKind::Call`.
const DYNAMIC_WATCH: &[&str] = &["waterui_core", "components", "dynamic", "Dynamic", "watch"];

/// A `watch(value, f)` call whose `f` is a closure literal.
pub(crate) struct WatchCall<'tcx> {
    /// The watched value — the first argument.
    pub signal: &'tcx Expr<'tcx>,
    /// The value-to-view closure literal — the second argument.
    pub closure: &'tcx Closure<'tcx>,
}

/// `Some(..)` when `expr` calls `watch`/`Dynamic::watch` with a closure
/// literal for `f`. A non-closure `f` — a named function, a variable — is not
/// recognized: what it does with the value is not visible at the call site.
pub(crate) fn watch_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<WatchCall<'tcx>> {
    let ExprKind::Call(func, [signal, closure]) = expr.kind else {
        return None;
    };
    let Res::Def(DefKind::Fn | DefKind::AssocFn, did) = func.res(cx) else {
        return None;
    };
    if !crate::def_path::def_path_eq(cx, did, WATCH_FN)
        && !crate::def_path::def_path_eq(cx, did, DYNAMIC_WATCH)
    {
        return None;
    }
    let ExprKind::Closure(closure) = closure.kind else {
        return None;
    };
    Some(WatchCall { signal, closure })
}
