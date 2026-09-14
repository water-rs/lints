//! Recognition of `watch` calls — the `waterui::dynamic::watch` free function
//! and the `Dynamic::watch` associated function — shared by the lints that
//! inspect them.

use clippy_utils::res::MaybeQPath;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Closure, Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::Ty;

/// `waterui_core::components::dynamic::watch` — the free `watch(value, f)`.
const WATCH_FN: &[&str] = &["waterui_core", "components", "dynamic", "watch"];

/// `waterui_core::components::dynamic::Dynamic::watch` — the associated
/// `Dynamic::watch(value, f)`. It has no `self` parameter, so it only ever
/// arrives as `ExprKind::Call`.
const DYNAMIC_WATCH: &[&str] = &["waterui_core", "components", "dynamic", "Dynamic", "watch"];

/// A `watch(value, f)` call.
pub(crate) struct WatchCall<'tcx> {
    /// The watched value — the first argument.
    pub signal: &'tcx Expr<'tcx>,
    /// The value-to-view function — the second argument, as passed.
    pub func: &'tcx Expr<'tcx>,
    /// `T`, the watched value's type — the callee's generic argument at index
    /// 0 in both spellings: `watch<T, S, V>` and `Dynamic::watch<T, S, V>`
    /// (`Dynamic` has no type parameters, so no parent args shift the index).
    /// See `waterui-core-0.3.2/src/components/dynamic.rs:310` and `:132`.
    pub value_ty: Ty<'tcx>,
}

impl<'tcx> WatchCall<'tcx> {
    /// `f` as a closure literal, when it is one. A non-closure `f` — a named
    /// function, a variable — yields `None`: what it does with the value is
    /// not visible at the call site.
    pub(crate) fn closure(&self) -> Option<&'tcx Closure<'tcx>> {
        let ExprKind::Closure(closure) = self.func.kind else {
            return None;
        };
        Some(closure)
    }
}

/// `Some(..)` when `expr` calls `watch`/`Dynamic::watch`, whatever shape `f`
/// has — closure literal, named function, or variable.
pub(crate) fn watch_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<WatchCall<'tcx>> {
    let ExprKind::Call(callee, [signal, func]) = expr.kind else {
        return None;
    };
    let Res::Def(DefKind::Fn | DefKind::AssocFn, did) = callee.res(cx) else {
        return None;
    };
    if !crate::def_path::def_path_eq(cx, did, WATCH_FN)
        && !crate::def_path::def_path_eq(cx, did, DYNAMIC_WATCH)
    {
        return None;
    }
    let value_ty = cx
        .typeck_results()
        .node_args(callee.hir_id)
        .first()
        .and_then(|arg| arg.as_type())?;
    Some(WatchCall {
        signal,
        func,
        value_ty,
    })
}
