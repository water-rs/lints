use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;
use rustc_session::declare_lint_pass;

use crate::anyview::peel;
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::{TUPLE_VIEWS_BOUNDS, call_arg_bounds, call_args};
use crate::row_builder::is_row_builder_call;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a collection view used as an element of a `vstack(..)`/
    /// `hstack(..)`/`zstack(..)` tuple argument: `ForEach` and its
    /// constructor family — `ForEach::new`, `Lazy::for_each`,
    /// `List::for_each`, `VStack`/`HStack`/`ZStack::for_each` — and `List`/
    /// `ListBuilder`.
    ///
    /// ### Why is this bad?
    ///
    /// A tuple element must be a single `View`; a collection is a `Views`
    /// sequence, so the call fails with an E0277 that points at the whole
    /// tuple rather than the element. The stack's own `for_each` form or a
    /// `scroll(..)` wrapper is the shape that compiles.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// vstack((header, VStack::for_each(items, |item| row(item))))
    /// ```
    ///
    /// The collection is the stack's own content — not one element of a
    /// tuple:
    ///
    /// ```rust,ignore
    /// VStack::for_each(items, |item| row(item))
    /// // or, alongside fixed siblings, inside a scroll view:
    /// vstack((header, scroll(Lazy::for_each(items, |item| row(item)))))
    /// ```
    pub COLLECTION_IN_TUPLE_STACK,
    suspicious,
    "a collection view is not a stack tuple element"
}

declare_lint_pass!(CollectionInTupleStack => [COLLECTION_IN_TUPLE_STACK]);

/// The collection-view types an element's resolved type may be: `ForEach`
/// (the `Views` sequence the `for_each` constructors produce bare) and
/// `List`/`ListBuilder` — which do implement `View`, but are the same
/// wrong-shape element the tuple cannot accept.
const COLLECTION_TYPES: &[&[&str]] = &[
    &["waterui_core", "ui", "views", "ForEach"],
    &["waterui_internal", "component", "list", "List"],
    &["waterui_internal", "component", "list", "ListBuilder"],
];

const MESSAGE: &str = "a collection view is not a stack element";
const HELP: &str = "make the collection the stack's own content — `Stack::for_each(..)`/`List` — or wrap it in `scroll(..)` alongside the fixed elements";

/// Whether `ty`, references peeled, is one of `COLLECTION_TYPES`.
fn is_collection_ty(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    ty.peel_refs().ty_adt_def().is_some_and(|adt| {
        COLLECTION_TYPES
            .iter()
            .any(|path| def_path_eq(cx, adt.did(), path))
    })
}

/// Whether `elem` — an element of a `TupleViews` tuple — is a collection
/// view: its type is one of `COLLECTION_TYPES`, or it is a `for_each`-family
/// constructor call whose result is a stack of `ForEach` (`VStack::
/// for_each(..)`) or an opaque `impl View` (`Lazy::for_each(..)`) that the
/// type check cannot name.
fn is_collection_element<'tcx>(cx: &LateContext<'tcx>, elem: &'tcx Expr<'tcx>) -> bool {
    let typeck = cx.typeck_results();
    is_collection_ty(cx, typeck.expr_ty(elem)) || is_row_builder_call(cx, typeck, peel(elem))
}

impl<'tcx> LateLintPass<'tcx> for CollectionInTupleStack {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        let Some((_, per_arg)) = call_arg_bounds(cx, expr, &[TUPLE_VIEWS_BOUNDS]) else {
            return;
        };
        for (arg, targets) in call_args(expr).into_iter().zip(per_arg) {
            let ExprKind::Tup(elements) = peel(arg).kind else {
                continue;
            };
            if targets.is_empty() {
                continue;
            }
            for element in elements {
                if !is_collection_element(cx, element) {
                    continue;
                }
                span_lint_and_then(
                    cx,
                    COLLECTION_IN_TUPLE_STACK,
                    element.span,
                    MESSAGE,
                    |diag| {
                        diag.span_label(
                            element.span,
                            "this collection cannot sit between tuple elements",
                        );
                        diag.help(HELP);
                    },
                );
            }
        }
    }
}
