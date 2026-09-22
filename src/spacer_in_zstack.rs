use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint_pass;

use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_help;
use crate::param_bounds::{call_args, call_def_id};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `Spacer` elements in the contents tuple of `zstack((..))` and
    /// `ZStack::new(.., (..))`.
    ///
    /// ### Why is this bad?
    ///
    /// A spacer expands along its stack's axis and a Z stack has none, so
    /// the child is inert. The author probably wanted `.alignment(..)` on
    /// the `zstack`, or `absolute((..))` for an edge-anchored overlay.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// zstack((text("a"), spacer()))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// zstack((text("a"), text("b"))).alignment(Alignment::Bottom)
    /// ```
    pub SPACER_IN_ZSTACK,
    suspicious,
    "`spacer()` has no axis inside a `zstack`"
}

declare_lint_pass!(SpacerInZstack => [SPACER_IN_ZSTACK]);

/// Z-stack constructors whose last parameter is the `TupleViews` contents:
/// the `zstack` free function and `ZStack::new`
/// (`waterui-layout-0.3.2/src/stack/zstack.rs`).
const ZSTACK_CTORS: &[&[&str]] = &[
    &["waterui_layout", "stack", "zstack", "zstack"],
    &["waterui_layout", "stack", "zstack", "ZStack", "new"],
];

/// `waterui_layout::containers::spacer::Spacer` — `spacer()` and
/// `spacer_min(..)` both produce it
/// (`waterui-layout-0.3.2/src/containers/spacer.rs`).
const SPACER: &[&str] = &["waterui_layout", "containers", "spacer", "Spacer"];

impl<'tcx> LateLintPass<'tcx> for SpacerInZstack {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() || !matches!(expr.kind, ExprKind::Call(..)) {
            return;
        }
        let Some(did) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if !ZSTACK_CTORS.iter().any(|path| def_path_eq(cx, did, path)) {
            return;
        }
        let Some(&contents) = call_args(expr).last() else {
            return;
        };
        // Only a tuple literal exposes its children at the call; a variable
        // or array holding them is out of scope.
        let ExprKind::Tup(children) = contents.kind else {
            return;
        };
        for child in children {
            if matches!(
                cx.typeck_results().expr_ty(child).peel_refs().kind(),
                TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), SPACER)
            ) {
                span_lint_and_help(
                    cx,
                    SPACER_IN_ZSTACK,
                    child.span,
                    "`spacer()` has no axis inside a `zstack`",
                    None,
                    "a spacer expands along the stack axis and a Z stack has none, so this child is inert; position the layer with `.alignment(..)` on the `zstack`, or use `absolute((..))` for an edge-anchored overlay",
                );
            }
        }
    }
}
