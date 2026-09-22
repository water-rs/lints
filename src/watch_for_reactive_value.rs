use clippy_utils::res::MaybeResPath;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_hir::{Block, Expr, ExprKind, HirId, Pat};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::declare_lint_pass;
use std::ops::ControlFlow;

use crate::carriers::carried_arg;
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::{SIGNAL_PARAM_BOUNDS, arg_has_bound};
use crate::watch::watch_call;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `watch(signal, |value| ..)` and `Dynamic::watch(signal, |value| ..)`
    /// calls whose closure body is a single expression — a bare expression or
    /// a block with no statements and a tail — and in which every read of
    /// `value` flows through value carriers (`.clone()`, `.to_owned()`,
    /// `.to_string()`, `&`/`*`, a `format!(..)` it is an argument of) into a
    /// parameter bound by a signal-taking trait: `IntoSignal`,
    /// `IntoComputed`, `IntoSignalF32`, `IntoText`, `IntoLabel`.
    ///
    /// ### Why is this bad?
    ///
    /// `watch` tears down and rebuilds the subtree on every change, dropping
    /// state owned inside it. When the value only ever reaches a parameter
    /// that already tracks a signal, the callee could subscribe directly:
    /// `text!("{count}")` re-renders one text node; `.visible(signal)`
    /// toggles one flag. A read that escapes a carrier — a `match`/`if`
    /// scrutinee, `.len()`, arithmetic, a plain parameter — is a structural
    /// switch, which is the one job `watch` exists for and stays silent.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// watch(count.clone(), |v| text(v.to_string()))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text!("{count}")
    /// ```
    pub WATCH_FOR_REACTIVE_VALUE,
    suspicious,
    "a `watch` whose value only feeds signal-taking parameters rebuilds a subtree the signal could update"
}

declare_lint_pass!(WatchForReactiveValue => [WATCH_FOR_REACTIVE_VALUE]);

const MESSAGE: &str =
    "this `watch` rebuilds its subtree to update a value the callee already tracks as a signal";
const READ_LABEL: &str = "reaches a signal-taking parameter here";
const HELP: &str = "pass the signal itself — `text!(\"{count}\")` for text, or the signal to the `impl IntoComputed` / `IntoSignal` / `IntoText` parameter — so only that property updates and the subtree keeps its state";

/// The `HirId`s of every local the parameter pattern binds: one for
/// `|v|`/`|_v|`, several for a destructure like `|(a, b)|`.
fn bound_locals(pat: &Pat<'_>) -> Vec<HirId> {
    let mut bindings = Vec::new();
    pat.each_binding(|_, hir_id, _, _| bindings.push(hir_id));
    bindings
}

/// `call`'s parameter at `index` carries a signal-taking bound.
fn is_signal_position<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    call: &Expr<'tcx>,
    index: usize,
) -> bool {
    arg_has_bound(cx, typeck, call, &[SIGNAL_PARAM_BOUNDS], index)
}

impl<'tcx> LateLintPass<'tcx> for WatchForReactiveValue {
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
        // Only a single-expression body can "just move the value" — a bare
        // expression or a block with no statements and a tail. A body with a
        // `let`/statement may compute something non-reactive first.
        let value = match body.value.kind {
            ExprKind::Block(
                Block {
                    stmts: [],
                    expr: Some(tail),
                    ..
                },
                _,
            ) => *tail,
            ExprKind::Block(..) => return,
            _ => body.value,
        };
        let bindings = bound_locals(param.pat);
        let mut reads = Vec::new();
        for_each_expr(cx, value, |expr| -> ControlFlow<(), Descend> {
            if expr.res_local_id().is_some_and(|id| bindings.contains(&id)) {
                reads.push(expr);
            }
            ControlFlow::Continue(Descend::Yes)
        });
        // No reads is `watch_ignores_value`'s case.
        if reads.is_empty() {
            return;
        }
        for &read in &reads {
            let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(read.hir_id));
            let Some((call, index)) = carried_arg(cx, typeck, read) else {
                return;
            };
            if !is_signal_position(cx, typeck, call, index) {
                return;
            }
        }
        span_lint_and_then(cx, WATCH_FOR_REACTIVE_VALUE, expr.span, MESSAGE, |diag| {
            for read in &reads {
                diag.span_label(read.span, READ_LABEL);
            }
            diag.help(HELP);
        });
    }
}
