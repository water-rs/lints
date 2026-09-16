//! `image_without_label` — a `Photo`/`Image`/`Svg` whose modifier chain
//! carries no `.a11y_label(..)` and no `.a11y_hidden(true)` enters the
//! accessibility tree unannounced.

use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::get_parent_expr;
use clippy_utils::visitors::{Descend, for_each_expr_without_closures};
use rustc_ast::LitKind;
use rustc_hir::{Expr, ExprKind, Node, PatKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;
use rustc_session::declare_lint_pass;
use std::ops::ControlFlow;

use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `Photo`, `Image`, or `Svg` construction — `photo(..)`,
    /// `Photo::new(..)`, `Image::new(..)`, `image(..)`, `Svg::new(..)` —
    /// whose modifier chain carries neither `.a11y_label(..)` nor
    /// `.a11y_hidden(true)`.
    ///
    /// ### Why is this bad?
    ///
    /// Controls take a mandatory label at construction, but the image views
    /// have no label parameter, so they are the one place an unlabeled
    /// element enters the accessibility tree unnoticed. `.a11y_hidden(false)`
    /// is not a label — it keeps the image visible to assistive technology
    /// without naming it. A label set by an ancestor is the case to
    /// `#[allow]`; a value bound to a name with `let` can be labelled in a
    /// later statement, which is out of scope.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// photo(Url::parse("https://water-rs.dev/a.png").unwrap())
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// photo(Url::parse("https://water-rs.dev/a.png").unwrap())
    ///     .a11y_label("a river")
    /// // or, when the image is decorative: .a11y_hidden(true)
    /// ```
    pub IMAGE_WITHOUT_LABEL,
    a11y,
    "this image has no accessibility label"
}

declare_lint_pass!(ImageWithoutLabel => [IMAGE_WITHOUT_LABEL]);

/// The image views with no label parameter at construction — the defining
/// crates' paths, never the `waterui::media`/`waterui::svg` re-exports.
const IMAGE_TYPES: &[&[&str]] = &[
    &["waterui_media", "photo", "Photo"],
    &["waterui_image", "image", "Image"],
    &["waterui_svg", "Svg"],
];

/// `ViewExt::a11y_label` — a call to it anywhere in the chain names the
/// image.
const A11Y_LABEL: &[&str] = &["waterui_internal", "view", "ViewExt", "a11y_label"];

/// `ViewExt::a11y_hidden` — a label substitute only with the literal `true`;
/// `false` keeps the image visible to assistive technology without naming
/// it.
const A11Y_HIDDEN: &[&str] = &["waterui_internal", "view", "ViewExt", "a11y_hidden"];

const MESSAGE: &str = "this image has no accessibility label";
const HELP: &str = "add `.a11y_label(..)` naming what the image shows, or `.a11y_hidden(true)` \
                    when it is decorative; a label set by an ancestor is the case to `#[allow]`";

/// Whether `ty` — references peeled — is one of the image ADTs.
fn is_image_ty(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    ty.peel_refs().ty_adt_def().is_some_and(|adt| {
        IMAGE_TYPES
            .iter()
            .any(|path| def_path_eq(cx, adt.did(), path))
    })
}

/// Whether `call` resolves to `ViewExt::a11y_label` or to
/// `ViewExt::a11y_hidden` with the literal `true`.
fn is_label_call<'tcx>(cx: &LateContext<'tcx>, call: &Expr<'tcx>) -> bool {
    let Some(did) = call_def_id(cx.typeck_results(), call) else {
        return false;
    };
    let callee = implemented_trait_item(cx.tcx, did);
    if def_path_eq(cx, callee, A11Y_LABEL) {
        return true;
    }
    if !def_path_eq(cx, callee, A11Y_HIDDEN) {
        return false;
    }
    let ExprKind::MethodCall(_, _, [arg], _) = call.kind else {
        return false;
    };
    matches!(arg.kind, ExprKind::Lit(lit) if matches!(lit.node, LitKind::Bool(true)))
}

/// Whether `expr` wraps a nested image construction — `f(photo(..))`
/// returning an image. The nested call is itself a candidate; reporting the
/// wrapper too would count one construction twice. A non-call nested
/// expression (`f(p)` for a bound `p`) is a use, not a construction, and
/// does not yield the report.
fn contains_image_call(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    for_each_expr_without_closures(expr, |e| {
        if e.hir_id != expr.hir_id
            && matches!(e.kind, ExprKind::Call(..))
            && is_image_ty(cx, cx.typeck_results().expr_ty(e))
        {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(Descend::Yes)
        }
    })
    .is_some()
}

/// Whether `expr`'s value is bound to a name by a `let` — the visible chain
/// ends there, but the binding can be labelled in a later statement, which
/// is out of scope. `let _ =` binds no name and still reports.
fn bound_to_a_name(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    matches!(
        cx.tcx.parent_hir_node(expr.hir_id),
        Node::LetStmt(local)
            if local.init.is_some_and(|init| init.hir_id == expr.hir_id)
                && !matches!(local.pat.kind, PatKind::Wild)
    )
}

impl<'tcx> LateLintPass<'tcx> for ImageWithoutLabel {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        // A candidate is a construction — a call whose result is an image
        // ADT. A path read or a method call on an existing value is a use,
        // not a construction.
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..))
            || !is_image_ty(cx, cx.typeck_results().expr_ty(expr))
            || contains_image_call(cx, expr)
        {
            return;
        }
        // The chain is the modifier calls written on the image: walk outward
        // while the parent is a method call whose receiver is the current
        // expression; it ends at the first non-method parent.
        let mut top = expr;
        let mut labelled = false;
        while let Some(parent) = get_parent_expr(cx, top)
            && let ExprKind::MethodCall(_, receiver, ..) = parent.kind
            && receiver.hir_id == top.hir_id
        {
            labelled |= is_label_call(cx, parent);
            top = parent;
        }
        if labelled || bound_to_a_name(cx, top) {
            return;
        }
        span_lint_and_help(cx, IMAGE_WITHOUT_LABEL, top.span, MESSAGE, None, HELP);
    }
}
