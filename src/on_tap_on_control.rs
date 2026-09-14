//! `on_tap_on_control` — a tap gesture on a view that already activates on
//! tap competes with the control's own activation.

use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyKind};
use rustc_session::declare_lint_pass;

use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};

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

/// `ViewExt` modifiers that attach a single-tap gesture, matched on the
/// callee normalized through [`implemented_trait_item`]. The `on_tap_haptic*`
/// pair is `std`-gated upstream; a linted crate without the feature simply
/// never resolves them.
const TAP_METHODS: &[&[&str]] = &[
    &["waterui_internal", "view", "ViewExt", "on_tap"],
    &["waterui_internal", "view", "ViewExt", "on_tap_gesture"],
    &[
        "waterui_internal",
        "view",
        "ViewExt",
        "on_tap_gesture_count",
    ],
    &["waterui_internal", "view", "ViewExt", "on_tap_haptic"],
    &[
        "waterui_internal",
        "view",
        "ViewExt",
        "on_tap_haptic_default",
    ],
];

/// `ViewExt::gesture` — a tap only when its gesture argument is a
/// `TapGesture`.
const GESTURE: &[&str] = &["waterui_internal", "view", "ViewExt", "gesture"];

/// `waterui_core::ui::gesture::TapGesture`.
const TAP_GESTURE: &[&str] = &["waterui_core", "ui", "gesture", "TapGesture"];

/// Receiver types with an activation of their own — a tap gesture on one
/// competes with it. The last segment is the name the diagnostic prints.
const CONTROLS: &[&[&str]] = &[
    &["waterui_controls", "button", "Button"],
    &["waterui_controls", "menu", "Menu"],
    &["waterui_controls", "slider", "Slider"],
    &["waterui_controls", "stepper", "Stepper"],
    &["waterui_controls", "text_field", "TextField"],
    &["waterui_controls", "toggle", "Toggle"],
    &["waterui_form", "picker", "Picker"],
    &["waterui_internal", "component", "list", "ListItem"],
    &["waterui_navigation", "NavigationLink"],
];

const HELP: &str = "the control already activates on tap and the accessibility tree would carry \
                    two activations for one element; use `.action(..)` (or the control's own \
                    change handler) instead";

/// The control's type name when `ty`, refs peeled, is one of `CONTROLS`.
fn control_name(cx: &LateContext<'_>, ty: Ty<'_>) -> Option<&'static str> {
    let TyKind::Adt(adt, _) = *ty.peel_refs().kind() else {
        return None;
    };
    CONTROLS
        .iter()
        .find(|path| def_path_eq(cx, adt.did(), path))
        .and_then(|path| path.last().copied())
}

/// Whether `expr` is a `TapGesture`-typed expression.
fn is_tap_gesture(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    matches!(
        cx.typeck_results().expr_ty(expr).peel_refs().kind(),
        TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), TAP_GESTURE)
    )
}

impl<'tcx> LateLintPass<'tcx> for OnTapOnControl {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(segment, receiver, args, _) = expr.kind else {
            return;
        };
        let Some(did) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        let callee = implemented_trait_item(cx.tcx, did);
        let is_tap = TAP_METHODS.iter().any(|path| def_path_eq(cx, callee, path))
            || (def_path_eq(cx, callee, GESTURE)
                && args
                    .first()
                    .is_some_and(|gesture| is_tap_gesture(cx, gesture)));
        if !is_tap {
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
