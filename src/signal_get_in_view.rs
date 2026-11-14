use clippy_utils::macros::FormatArgsStorage;
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use rustc_errors::Applicability;
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor, nested_filter};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeVisitableExt;
use rustc_session::impl_lint_pass;

use crate::diagnostics::{span_lint_and_help, span_lint_and_sugg, span_lint_and_then};
use crate::param_bounds::{
    BoundTarget, SIGNAL_PARAM_BOUNDS, VIEW_PARAM_BOUNDS, call_arg_bounds, call_args,
};
use crate::snapshot_get::{get_receiver, is_snapshot_get};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `Signal::get`/`Binding::get` snapshots passed into a call whose
    /// parameter is bound by a reactive or view trait — `IntoSignal`,
    /// `IntoComputed`, `IntoSignalF32`, `IntoText`, `IntoLabel`, `View`,
    /// `ViewBuilder` — whether the `.get()` is the argument itself or carried
    /// through `format!`, arithmetic, `.into()`, `as` casts, or struct-literal
    /// fields.
    ///
    /// ### Why is this bad?
    ///
    /// `.get()` reads the signal once and yields a plain value; the callee
    /// subscribes to nothing, so the view is frozen at the value it happened
    /// to read. `view.opacity(fade.get())` compiles and never animates.
    /// Handlers, `.map` closures, and tests are unaffected: the lint decides
    /// from the callee's signature, not from where the `.get()` textually
    /// sits.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("hello").opacity(fade.get())
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("hello").opacity(fade.clone())
    /// ```
    pub SIGNAL_GET_IN_VIEW,
    correctness,
    "a `.get()` snapshot flows into a reactive or view parameter"
}

/// `text` — the only callee for which `text(format!(..))` can be rewritten as
/// `text!(..)`, since `text!` produces a `Text` directly.
const TEXT_FN: &[&str] = &["waterui_text", "text", "text"];

const MESSAGE: &str = "`.get()` reads the signal once; the view will never update";

const PASS_SIGNAL_HELP: &str = "pass the signal itself so the view can subscribe to updates";

pub(crate) struct SignalGetInView {
    format_args: FormatArgsStorage,
}

impl SignalGetInView {
    pub(crate) fn new(format_args: FormatArgsStorage) -> Self {
        Self { format_args }
    }
}

impl_lint_pass!(SignalGetInView => [SIGNAL_GET_IN_VIEW]);

/// Collects `.get()` snapshots inside one argument expression. Everything is
/// walked except closures and const blocks (deferred/foreign bodies where a
/// `.get()` is a legitimate read), nested items (skipped by `NestedFilter`),
/// and arguments that already sit in a reactive-bound position of a nested
/// call — those are reported by that call's own `check_expr`.
struct SnapshotGet<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    gets: Vec<&'tcx Expr<'tcx>>,
}

impl<'tcx> Visitor<'tcx> for SnapshotGet<'_, 'tcx> {
    type NestedFilter = nested_filter::None;

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match expr.kind {
            ExprKind::Closure(_) | ExprKind::ConstBlock(_) => {}
            ExprKind::Call(func, args) => {
                if is_snapshot_get(self.cx, expr) {
                    self.gets.push(expr);
                }
                self.visit_expr(func);
                let mask = bound_arg_mask(self.cx, expr);
                for (index, arg) in args.iter().enumerate() {
                    if !mask.get(index).copied().unwrap_or_default() {
                        self.visit_expr(arg);
                    }
                }
            }
            ExprKind::MethodCall(_, receiver, args, _) => {
                if is_snapshot_get(self.cx, expr) {
                    self.gets.push(expr);
                }
                let mask = bound_arg_mask(self.cx, expr);
                for (index, arg) in std::iter::once(receiver).chain(args).enumerate() {
                    if !mask.get(index).copied().unwrap_or_default() {
                        self.visit_expr(arg);
                    }
                }
            }
            _ => intravisit::walk_expr(self, expr),
        }
    }
}

/// `mask[i]` — whether the `i`-th argument position of `call` (the receiver
/// counts as position 0 for method calls) maps to a reactive-bound parameter.
/// The inner `check_expr` owns diagnostics for those positions, so
/// [`SnapshotGet`] skips them to keep one diagnostic per `.get()`.
fn bound_arg_mask<'tcx>(cx: &LateContext<'tcx>, call: &Expr<'tcx>) -> Vec<bool> {
    call_arg_bounds(cx, call, &[SIGNAL_PARAM_BOUNDS, VIEW_PARAM_BOUNDS])
        .map(|(_, bounds)| bounds.iter().map(|targets| !targets.is_empty()).collect())
        .unwrap_or_default()
}

/// `Some(())` when `arg`, stripping drop-temps, is exactly `get`.
fn arg_is_the_get(arg: &Expr<'_>, get: &Expr<'_>) -> bool {
    let mut expr = arg;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    expr.hir_id == get.hir_id
}

/// `x.clone()` when `x`'s type satisfies one of the parameter's bounds.
fn clone_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    get: &Expr<'tcx>,
    bounds: &[BoundTarget<'tcx>],
) -> Option<String> {
    let receiver = get_receiver(get)?;
    let receiver_ty = cx.typeck_results().expr_ty(receiver);
    let satisfies = bounds.iter().any(|target| {
        target.args.iter().all(|arg| !arg.has_infer())
            && implements_trait(cx, receiver_ty, target.trait_did, target.args)
    });
    if satisfies {
        Some(format!("{}.clone()", snippet_opt(cx, receiver.span)?))
    } else {
        None
    }
}

fn report_arg<'tcx>(
    cx: &LateContext<'tcx>,
    storage: &FormatArgsStorage,
    call: &'tcx Expr<'tcx>,
    callee: DefId,
    arg: &'tcx Expr<'tcx>,
    bounds: &[BoundTarget<'tcx>],
) {
    let mut finder = SnapshotGet {
        cx,
        gets: Vec::new(),
    };
    finder.visit_expr(arg);
    for (index, get) in finder.gets.iter().copied().enumerate() {
        // A `.get()` a macro wrote (`text!`'s own subscription plumbing) is
        // not the user's; `format!` arguments keep their call-site spans, so
        // the `text(format!(.., x.get()))` case is unaffected.
        if get.span.from_expansion() {
            continue;
        }
        if arg_is_the_get(arg, get) {
            match clone_suggestion(cx, get, bounds) {
                Some(suggestion) => span_lint_and_sugg(
                    cx,
                    SIGNAL_GET_IN_VIEW,
                    get.span,
                    MESSAGE,
                    PASS_SIGNAL_HELP,
                    suggestion,
                    Applicability::MachineApplicable,
                ),
                None => {
                    span_lint_and_help(
                        cx,
                        SIGNAL_GET_IN_VIEW,
                        get.span,
                        MESSAGE,
                        None,
                        PASS_SIGNAL_HELP,
                    );
                }
            }
        } else if index == 0 && crate::def_path::def_path_eq(cx, callee, TEXT_FN) {
            match crate::format_args::text_macro_suggestion(storage, cx, arg) {
                Some(suggestion) => {
                    span_lint_and_then(cx, SIGNAL_GET_IN_VIEW, get.span, MESSAGE, |diag| {
                        diag.span_suggestion(
                            call.span,
                            "use `text!` so the placeholders subscribe to the signals",
                            suggestion,
                            Applicability::MaybeIncorrect,
                        );
                    })
                }
                None => {
                    span_lint_and_help(
                        cx,
                        SIGNAL_GET_IN_VIEW,
                        get.span,
                        MESSAGE,
                        None,
                        PASS_SIGNAL_HELP,
                    );
                }
            }
        } else {
            span_lint_and_help(
                cx,
                SIGNAL_GET_IN_VIEW,
                get.span,
                MESSAGE,
                None,
                PASS_SIGNAL_HELP,
            );
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for SignalGetInView {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let Some((callee, arg_bounds)) =
            call_arg_bounds(cx, expr, &[SIGNAL_PARAM_BOUNDS, VIEW_PARAM_BOUNDS])
        else {
            return;
        };
        for (arg, targets) in call_args(expr).into_iter().zip(arg_bounds) {
            if !targets.is_empty() {
                report_arg(cx, &self.format_args, expr, callee, arg, &targets);
            }
        }
    }
}
