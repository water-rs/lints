use clippy_utils::diagnostics::span_lint_and_sugg;
use clippy_utils::source::snippet_with_applicability;
use rustc_errors::Applicability;
use rustc_hir::Expr;
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::anyview::{erased_inner, is_anyview};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `AnyView::new(v)`, `v.anyview()`, and `ViewExt::anyview(v)` calls
    /// whose erased value `v` is already an `AnyView`.
    ///
    /// ### Why is this bad?
    ///
    /// The second wrapper buys nothing: `AnyView::new` unwraps an `AnyView`
    /// back out at a `TypeId` check, so the call is dead code that still
    /// reads as producing a new erased view. It shows up most often after
    /// refactoring `match` arms, when the values being erased were already
    /// erased upstream.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let view: AnyView = ..;
    /// AnyView::new(view)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// view
    /// ```
    pub REDUNDANT_ANYVIEW,
    style,
    "erasing a value that is already an `AnyView`"
}

declare_lint_pass!(RedundantAnyview => [REDUNDANT_ANYVIEW]);

impl<'tcx> LateLintPass<'tcx> for RedundantAnyview {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let typeck = cx.typeck_results();
        let Some(inner) = erased_inner(cx, typeck, expr) else {
            return;
        };
        if !is_anyview(cx, typeck.expr_ty(inner)) {
            return;
        }
        let mut applicability = Applicability::MachineApplicable;
        let snippet = snippet_with_applicability(cx, inner.span, "..", &mut applicability);
        span_lint_and_sugg(
            cx,
            REDUNDANT_ANYVIEW,
            expr.span,
            "this `AnyView` is erased a second time",
            "drop the second wrapper",
            snippet.into_owned(),
            applicability,
        );
    }
}
