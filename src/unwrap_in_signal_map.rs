//! `unwrap_in_signal_map` — an `Option`/`Result` `unwrap`-family call on the
//! parameter inside a `SignalExt::map` closure: the derived signal panics the
//! first time the source is `None`/`Err`.

use std::ops::ControlFlow;

use clippy_utils::is_in_cfg_test;
use clippy_utils::res::MaybeResPath;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::{Body, Expr, ExprKind, HirId, Pat};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::impl_lint_pass;

use crate::anyview::peel;
use crate::carriers::CLONE;
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::{call_def_id, implemented_trait_item};
use crate::thread_sleep::is_test_wrapper;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags an `Option`/`Result` `unwrap`, `expect`, or `unwrap_unchecked`
    /// inside a `SignalExt::map` closure (a `zip(..).map(..)` included) whose
    /// receiver — peeled of `as_ref`/`as_deref`/`clone`, field accesses,
    /// tuple indices, and `&` — is a closure parameter binding. The body is
    /// walked in order: a nested closure's parameter counts when the call it
    /// is passed to derives from the parameter — `v.map(|x| x.unwrap())` —
    /// and a nested `SignalExt::map` is reported on its own span. A `map` in
    /// a `#[cfg(test)]` item or a `#[waterui::test]` body is exempt.
    ///
    /// ### Why is this bad?
    ///
    /// The derived signal re-runs the closure on every value the source
    /// produces, and `None`/`Err` is a state every `Option`/`Result` signal
    /// reaches at least once — before a load, after a failure. The panic
    /// takes the UI thread down.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let label = user.map(|u| u.unwrap().name.clone());
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let label = user.map_some(|u| u.name.clone());
    /// ```
    pub UNWRAP_IN_SIGNAL_MAP,
    suspicious,
    "an `unwrap` on the parameter inside a `SignalExt::map` closure"
}

/// `SignalExt::map` — the blanket `impl<C: Signal> SignalExt for C` puts it on
/// every signal, so a `zip(..).map(..)` resolves here through the same impl.
const SIGNAL_EXT_MAP: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "map"];

/// `Option`/`Result` calls that panic on the empty state, paired with the
/// source state the message names. All inherent methods, so no
/// `implemented_trait_item` normalization is needed.
const PANIC_CALLS: &[(&[&str], &str)] = &[
    (&["core", "option", "Option", "unwrap"], "None"),
    (&["core", "option", "Option", "expect"], "None"),
    (&["core", "option", "Option", "unwrap_unchecked"], "None"),
    (&["core", "result", "Result", "unwrap"], "Err"),
    (&["core", "result", "Result", "expect"], "Err"),
    (&["core", "result", "Result", "unwrap_unchecked"], "Err"),
];

/// Calls that re-wrap a value without inspecting it — `v.as_ref()`,
/// `v.as_deref()`, `v.clone()` — peeled on the way to the parameter. `AsRef`
/// and `Clone` are trait items, matched after `implemented_trait_item`
/// normalization; `Option`/`Result`'s `as_ref`/`as_deref` are inherent.
const PROJECTION_CALLS: &[&[&str]] = &[
    CLONE,
    &["core", "convert", "AsRef", "as_ref"],
    &["core", "option", "Option", "as_ref"],
    &["core", "option", "Option", "as_deref"],
    &["core", "result", "Result", "as_ref"],
    &["core", "result", "Result", "as_deref"],
];

const LABEL: &str = "the panic happens here";
const HELP: &str = "derive the value with `unwrap_or`/`unwrap_or_default`/`unwrap_or_else` (`unwrap_or_result`/`unwrap_or_else_result` for a `Result`), or keep the `Option`/`Result` with `map_some`/`map_ok` and decide at the view with `when`";

/// `did` is an `Option`/`Result` panic method; the `&str` is the source
/// state the call panics on.
fn panics_on(cx: &LateContext<'_>, did: DefId) -> Option<&'static str> {
    PANIC_CALLS
        .iter()
        .find_map(|(path, on)| def_path_eq(cx, did, path).then_some(*on))
}

/// `expr` peeled of `&`, field accesses, tuple indices, drop-temps, and the
/// `as_ref`/`as_deref`/`clone` re-wraps — the chain that still denotes the
/// value the closure received.
fn peel_projection<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::AddrOf(.., inner)
            | ExprKind::DropTemps(inner)
            | ExprKind::Field(inner, _) => inner,
            ExprKind::MethodCall(_, receiver, [], _)
                if call_def_id(typeck, expr).is_some_and(|did| {
                    let did = implemented_trait_item(cx.tcx, did);
                    PROJECTION_CALLS
                        .iter()
                        .any(|path| def_path_eq(cx, did, path))
                }) =>
            {
                receiver
            }
            _ => return expr,
        };
    }
}

/// The `HirId`s every local `pat` binds: none for `|_|`, one for `|v|`,
/// several for a destructure like `|(a, b)|`.
fn bound_locals(pat: &Pat<'_>) -> Vec<HirId> {
    let mut bindings = Vec::new();
    pat.each_binding(|_, hir_id, _, _| bindings.push(hir_id));
    bindings
}

/// Every panic call inside `body` whose receiver peels back to one of
/// `bindings` — each with the source state it panics on. `bindings` grows as
/// the walk runs: a closure argument of a call whose receiver peels to a
/// bound local takes its parameter values from it — `x` in `v.map(|x| ..)`
/// derives from `v` — and traversal is pre-order, so the derivation chains.
/// A nested `SignalExt::map` is not descended into: `check_expr` reports it
/// on its own span, and descending would double-report the same unwrap. A
/// panic call a macro wrote keeps its expansion span and is not reported;
/// an argument spliced into a macro call — `format!("{}", v.unwrap())` —
/// keeps its callsite span and is still reached.
fn panic_calls<'hir>(
    cx: &LateContext<'hir>,
    body: &Body<'hir>,
    bindings: &mut Vec<HirId>,
) -> Vec<(&'hir Expr<'hir>, &'static str)> {
    let mut calls = Vec::new();
    for_each_expr(cx, body.value, |expr| -> ControlFlow<(), Descend> {
        let ExprKind::MethodCall(_, receiver, args, _) = expr.kind else {
            return ControlFlow::Continue(Descend::Yes);
        };
        // `expr` may live in a nested body; resolve through the typeck of
        // the body that owns it.
        let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(expr.hir_id));
        let did = typeck.type_dependent_def_id(expr.hir_id);
        if did
            .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), SIGNAL_EXT_MAP))
        {
            return ControlFlow::Continue(Descend::No);
        }
        if !peel_projection(cx, typeck, receiver)
            .res_local_id()
            .is_some_and(|hir_id| bindings.contains(&hir_id))
        {
            return ControlFlow::Continue(Descend::Yes);
        }
        if !expr.span.from_expansion()
            && let Some(on) = did.and_then(|did| panics_on(cx, did))
        {
            calls.push((expr, on));
        }
        for arg in args {
            let ExprKind::Closure(closure) = peel(arg).kind else {
                continue;
            };
            for param in cx.tcx.hir_body(closure.body).params {
                bindings.extend(bound_locals(param.pat));
            }
        }
        ControlFlow::Continue(Descend::Yes)
    });
    calls
}

/// `is_test_wrapper` verdicts per enclosing fn `LocalDefId` — a body with
/// several `map`s resolves it once.
#[derive(Default)]
pub(crate) struct UnwrapInSignalMap {
    wrappers: FxHashMap<LocalDefId, bool>,
}

impl_lint_pass!(UnwrapInSignalMap => [UNWRAP_IN_SIGNAL_MAP]);

impl<'tcx> LateLintPass<'tcx> for UnwrapInSignalMap {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(_, _, [func], _) = expr.kind else {
            return;
        };
        if !cx
            .typeck_results()
            .type_dependent_def_id(expr.hir_id)
            .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), SIGNAL_EXT_MAP))
        {
            return;
        }
        let mut func = func;
        while let ExprKind::DropTemps(inner) = func.kind {
            func = inner;
        }
        let ExprKind::Closure(closure) = func.kind else {
            return;
        };
        // A `#[cfg(test)]` item or a `#[waterui::test]`/`#[waterui::bench]`
        // body asserts the value — the unwrap is the point.
        if is_in_cfg_test(cx.tcx, expr.hir_id) {
            return;
        }
        // The typeck root is the enclosing non-closure body owner — the `fn`
        // the `#[waterui::test]`/`#[waterui::bench]` attribute expanded.
        let owner = cx
            .tcx
            .typeck_root_def_id(cx.tcx.hir_enclosing_body_owner(expr.hir_id).into())
            .expect_local();
        if is_test_wrapper(cx, owner, &mut self.wrappers) {
            return;
        }
        let body = cx.tcx.hir_body(closure.body);
        let mut bindings: Vec<HirId> = body
            .params
            .iter()
            .flat_map(|param| bound_locals(param.pat))
            .collect();
        if bindings.is_empty() {
            return;
        }
        for (call, on) in panic_calls(cx, body, &mut bindings) {
            span_lint_and_then(
                cx,
                UNWRAP_IN_SIGNAL_MAP,
                expr.span,
                format!("this signal panics whenever its source is `{on}`"),
                |diag| {
                    diag.span_label(call.span, LABEL);
                    diag.help(HELP);
                },
            );
        }
    }
}
