//! Tap-gesture detection shared by the tap lints — `on_tap_on_control` (a
//! tap on a control that already activates) and `tappable_without_role` (a
//! tap on a view with no accessibility identity).

use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::{Ty, TyKind};

use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};

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

/// The control's type name when `ty`, refs peeled, is one of `CONTROLS`.
pub(crate) fn control_name(cx: &LateContext<'_>, ty: Ty<'_>) -> Option<&'static str> {
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

/// Whether `expr` is a method call attaching a single-tap gesture — one of
/// `TAP_METHODS`, or `ViewExt::gesture` with a `TapGesture` argument.
pub(crate) fn is_tap_call<'tcx>(cx: &LateContext<'tcx>, expr: &Expr<'tcx>) -> bool {
    let ExprKind::MethodCall(_, _, args, _) = expr.kind else {
        return false;
    };
    let Some(did) = call_def_id(cx.typeck_results(), expr) else {
        return false;
    };
    let callee = implemented_trait_item(cx.tcx, did);
    TAP_METHODS.iter().any(|path| def_path_eq(cx, callee, path))
        || (def_path_eq(cx, callee, GESTURE)
            && args
                .first()
                .is_some_and(|gesture| is_tap_gesture(cx, gesture)))
}
