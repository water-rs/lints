//! `tappable_without_role` — a tap gesture on a view with no accessibility
//! role or label is invisible to assistive technology.

use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::get_parent_expr;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};
use crate::tap_gesture::{control_name, is_tap_call};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `ViewExt` tap-gesture modifiers — `.on_tap(..)`,
    /// `.on_tap_gesture(..)`, `.on_tap_gesture_count(..)`,
    /// `.on_tap_haptic(..)`, `.on_tap_haptic_default(..)`, and
    /// `.gesture(..)` with a `TapGesture` argument — on a view that is not a
    /// control and whose modifier chain carries no `.a11y_role(..)` or
    /// `.a11y_label(..)`.
    ///
    /// ### Why is this bad?
    ///
    /// A tappable region with no role or label is invisible to assistive
    /// technology and to `waterui-testing` queries alike — both address
    /// elements through the accessibility tree.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("a").on_tap(|| open())
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("a")
    ///     .a11y_role(AccessibilityRole::Button)
    ///     .a11y_label("Open")
    ///     .on_tap(|| open())
    /// // or: button("Open").action(|| open())
    /// ```
    pub TAPPABLE_WITHOUT_ROLE,
    a11y,
    "a tap gesture on a view with no accessibility role or label"
}

declare_lint_pass!(TappableWithoutRole => [TAPPABLE_WITHOUT_ROLE]);

/// `ViewExt` modifiers that give a view its accessibility identity — either
/// one in the chain makes a tappable region discoverable.
const A11Y_IDENTITY_METHODS: &[&[&str]] = &[
    &["waterui_internal", "view", "ViewExt", "a11y_role"],
    &["waterui_internal", "view", "ViewExt", "a11y_label"],
];

const HELP: &str = "assistive technology and `waterui-testing` queries cannot find this tappable \
                    region; add `.a11y_role(AccessibilityRole::Button)` and `.a11y_label(..)`, or \
                    use `button(..)`";

/// Whether `call` resolves to `ViewExt::a11y_role` or `ViewExt::a11y_label`.
fn is_a11y_identity_call<'tcx>(cx: &LateContext<'tcx>, call: &Expr<'tcx>) -> bool {
    let Some(did) = call_def_id(cx.typeck_results(), call) else {
        return false;
    };
    let callee = implemented_trait_item(cx.tcx, did);
    A11Y_IDENTITY_METHODS
        .iter()
        .any(|path| def_path_eq(cx, callee, path))
}

/// Whether any call in `tap`'s modifier chain resolves to `a11y_role` or
/// `a11y_label`. The chain is the receiver chain below the tap call
/// (`view.m().n().on_tap(..)` sees `m` and `n`) plus the method calls above
/// it (`view.on_tap(..).a11y_role(..)` sees `a11y_role`).
fn chain_has_a11y_identity<'tcx>(cx: &LateContext<'tcx>, tap: &Expr<'tcx>) -> bool {
    if let ExprKind::MethodCall(_, receiver, ..) = tap.kind {
        let mut current = receiver;
        while let ExprKind::MethodCall(_, receiver, ..) = current.kind {
            if is_a11y_identity_call(cx, current) {
                return true;
            }
            current = receiver;
        }
    }
    let mut current = tap;
    while let Some(parent) = get_parent_expr(cx, current)
        && let ExprKind::MethodCall(_, receiver, ..) = parent.kind
        && receiver.hir_id == current.hir_id
    {
        if is_a11y_identity_call(cx, parent) {
            return true;
        }
        current = parent;
    }
    false
}

impl<'tcx> LateLintPass<'tcx> for TappableWithoutRole {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(segment, receiver, ..) = expr.kind else {
            return;
        };
        if !is_tap_call(cx, expr)
            || control_name(cx, cx.typeck_results().expr_ty_adjusted(receiver)).is_some()
            || chain_has_a11y_identity(cx, expr)
        {
            return;
        }
        span_lint_and_help(
            cx,
            TAPPABLE_WITHOUT_ROLE,
            expr.span.with_lo(segment.ident.span.lo()),
            "a tap gesture on a view with no accessibility role or label",
            None,
            HELP,
        );
    }
}
