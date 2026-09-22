use clippy_utils::res::MaybeResPath;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_hir::{Expr, HirId, Pat};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;
use std::ops::ControlFlow;

use crate::diagnostics::span_lint_and_then;
use crate::watch::watch_call;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `watch(signal, |value| ..)` and `Dynamic::watch(signal, |value| ..)`
    /// calls whose closure never reads `value`: the parameter is `_`, a
    /// binding nothing in the body resolves to, or a destructure whose
    /// bindings all go unused. A non-closure `f` — a named function, a
    /// variable — is not checked.
    ///
    /// ### Why is this bad?
    ///
    /// `watch` replaces its entire subtree every time the signal changes.
    /// When the closure never reads the value, every change rebuilds an
    /// identical subtree — pure churn that also discards any state owned
    /// inside it. The author almost always wanted `.visible(..)`, `when(..)`,
    /// or an `.on_change` side effect.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// watch(flag, |_| text("hi"))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("hi").visible(flag)
    /// ```
    pub WATCH_IGNORES_VALUE,
    suspicious,
    "a `watch` closure that never reads the watched value"
}

declare_lint_pass!(WatchIgnoresValue => [WATCH_IGNORES_VALUE]);

const MESSAGE: &str =
    "this `watch` rebuilds its subtree on every change of a value the closure never reads";
const HELP: &str = "hide or show with `.visible(signal)` / `when(signal, || ..)`, or react with `.on_change(&signal, |v| ..)` — `watch` is for a value that decides the view's shape";
const SIGNAL_LABEL: &str = "every change of this value rebuilds the subtree";

/// The `HirId`s of every local the parameter pattern binds: none for `_`,
/// one for `|v|`/`|_v|`, several for a destructure like `|(a, b)|`.
fn bound_locals(pat: &Pat<'_>) -> Vec<HirId> {
    let mut bindings = Vec::new();
    pat.each_binding(|_, hir_id, _, _| bindings.push(hir_id));
    bindings
}

impl<'tcx> LateLintPass<'tcx> for WatchIgnoresValue {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let Some(call) = watch_call(cx, expr) else {
            return;
        };
        let Some(closure) = call.closure() else {
            return;
        };
        let body = cx.tcx.hir_body(closure.body);
        // `Fn(T) -> V` arity is one; a closure of any other arity already
        // failed type checking, so there is nothing to flag.
        let [param] = body.params else {
            return;
        };
        let bindings = bound_locals(param.pat);
        let reads_value = !bindings.is_empty()
            && for_each_expr(cx, body.value, |expr| {
                if expr.res_local_id().is_some_and(|id| bindings.contains(&id)) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(Descend::Yes)
                }
            })
            .is_some();
        if reads_value {
            return;
        }
        span_lint_and_then(cx, WATCH_IGNORES_VALUE, param.pat.span, MESSAGE, |diag| {
            diag.span_label(call.signal.span, SIGNAL_LABEL);
            diag.help(HELP);
        });
    }
}
