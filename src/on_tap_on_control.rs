//! `on_tap_on_control` — a tap gesture on a view that already activates on
//! tap competes with the control's own activation.

use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::tap_gesture::{control_name, is_tap_call};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `ViewExt` tap-gesture modifiers — `.on_tap(..)`,
    /// `.on_tap_gesture(..)`, `.on_tap_gesture_count(..)`,
    /// `.on_tap_haptic(..)`, `.on_tap_haptic_default(..)`, and
    /// `.gesture(..)` with a `TapGesture` argument — whose receiver is a
    /// control with its own activation: `Button`, `Toggle`, `Slider`,
    /// `Stepper`, `TextField`, `ListItem`, `Picker`, `NavigationLink`, or
    /// `Menu`.
    ///
    /// ### Why is this bad?
    ///
    /// The control's own action and the gesture compete for the same tap;
    /// which one wins differs per backend, and the accessibility tree ends
    /// up carrying two activations for one element.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// button("Save").on_tap(|| save())
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// button("Save").action(|| save())
    /// ```
    pub ON_TAP_ON_CONTROL,
    suspicious,
    "a tap gesture on a control competes with the control's own activation"
}

declare_lint_pass!(OnTapOnControl => [ON_TAP_ON_CONTROL]);

const HELP: &str = "the control already activates on tap and the accessibility tree would carry \
                    two activations for one element; use `.action(..)` (or the control's own \
                    change handler) instead";

impl<'tcx> LateLintPass<'tcx> for OnTapOnControl {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(segment, receiver, ..) = expr.kind else {
            return;
        };
        if !is_tap_call(cx, expr) {
            return;
        }
        let Some(name) = control_name(cx, cx.typeck_results().expr_ty_adjusted(receiver)) else {
            return;
        };
        span_lint_and_help(
            cx,
            ON_TAP_ON_CONTROL,
            expr.span.with_lo(segment.ident.span.lo()),
            format!("a tap gesture on a `{name}` competes with the control's own activation"),
            None,
            HELP,
        );
    }
}
