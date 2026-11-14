use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;
use rustc_session::declare_lint_pass;

use crate::carriers::is_call_to;
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `scroll(..)`/`scroll_horizontal(..)` and
    /// `ScrollView::vertical(..)`/`ScrollView::horizontal(..)` calls whose
    /// content is a `List` — passed bare, under `Metadata` modifiers such as
    /// `.on_appear(..)`, or as the `ListBuilder` that `.editing(..)`/
    /// `.on_delete(..)`/`.on_move(..)`/`.scroll_controller(..)` produce.
    ///
    /// ### Why is this bad?
    ///
    /// `List` scrolls itself and realises only the rows in the visible
    /// window. Inside a `ScrollView` it is laid out with unbounded height, so
    /// every row is realised at once and the two scroll views fight for the
    /// gesture.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// scroll(List::for_each(items, |item| ListItem::new(text(item.name))))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// List::for_each(items, |item| ListItem::new(text(item.name)))
    /// ```
    pub LIST_IN_SCROLL,
    suspicious,
    "a `List` inside `scroll(..)` is given unbounded height"
}

declare_lint_pass!(ListInScroll => [LIST_IN_SCROLL]);

/// The calls that hand their single argument to a `ScrollView`: the `scroll`/
/// `scroll_horizontal` free functions and the `ScrollView::vertical`/
/// `ScrollView::horizontal` constructors they delegate to.
const SCROLL_CALLS: &[&[&str]] = &[
    &["waterui_layout", "collections", "scroll", "scroll"],
    &[
        "waterui_layout",
        "collections",
        "scroll",
        "scroll_horizontal",
    ],
    &[
        "waterui_layout",
        "collections",
        "scroll",
        "ScrollView",
        "vertical",
    ],
    &[
        "waterui_layout",
        "collections",
        "scroll",
        "ScrollView",
        "horizontal",
    ],
];

/// `waterui_core::components::metadata::Metadata` — layout-transparent, and
/// its content is an erased `AnyView`, so a `List` under metadata modifiers is
/// only visible on the receiver chain.
const METADATA: &[&str] = &["waterui_core", "components", "metadata", "Metadata"];

/// `waterui_internal::component::list::{List, ListBuilder}` — a `ListBuilder`
/// is the `List` `.editing(..)`/`.on_delete(..)`/`.on_move(..)`/
/// `.scroll_controller(..)` return.
const LIST_TYPES: &[&[&str]] = &[
    &["waterui_internal", "component", "list", "List"],
    &["waterui_internal", "component", "list", "ListBuilder"],
];

const MESSAGE: &str = "a `List` inside `scroll(..)` is given unbounded height";
const HELP: &str = "`List` scrolls itself and realises only the visible rows; drop the outer `scroll(..)`, or use `Lazy::for_each` inside the scroll view when a plain reactive sequence was wanted";

/// Whether `ty` — references peeled — is an ADT defined at one of `paths`.
fn is_ty(cx: &LateContext<'_>, ty: Ty<'_>, paths: &[&[&'static str]]) -> bool {
    ty.peel_refs()
        .ty_adt_def()
        .is_some_and(|adt| paths.iter().any(|path| def_path_eq(cx, adt.did(), path)))
}

impl<'tcx> LateLintPass<'tcx> for ListInScroll {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::Call(_, [content]) = expr.kind else {
            return;
        };
        let typeck = cx.typeck_results();
        if !is_call_to(cx, typeck, expr, SCROLL_CALLS) {
            return;
        }
        // Sizing wrappers (`.padding()`, `.frame(..)`) bound the list
        // themselves — `Metadata` layers are the only ones peeled.
        let mut wrapped = content;
        while let ExprKind::MethodCall(_, receiver, ..) = wrapped.kind
            && is_ty(cx, typeck.expr_ty(wrapped), &[METADATA])
        {
            wrapped = receiver;
        }
        if !is_ty(cx, typeck.expr_ty(wrapped), LIST_TYPES) {
            return;
        }
        span_lint_and_then(cx, LIST_IN_SCROLL, expr.span, MESSAGE, |diag| {
            diag.span_label(content.span, "this `List` scrolls on its own");
            diag.help(HELP);
        });
    }
}
