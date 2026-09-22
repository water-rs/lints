use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::MaybeQPath;
use clippy_utils::sugg::Sugg;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::{Expr, ExprKind, HirId, ItemKind, Node};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TyCtxt;
use rustc_session::declare_lint_pass;
use rustc_span::Symbol;

use crate::anyview::{ANYVIEW_EXT, ANYVIEW_NEW, is_anyview};
use crate::def_path::def_path_eq;
use crate::diagnostics::{span_lint_and_help, span_lint_and_then};
use crate::imports::{Bare, bare_status, use_insertion};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `AnyView::new(v)` calls inside a free function — closures and
    /// nested `fn` items inside one included — where `v` is not already an
    /// `AnyView`.
    ///
    /// ### Why is this bad?
    ///
    /// In a free view function the postfix `v.anyview()` chains with the
    /// rest of the modifier pipeline, while the `AnyView::new(..)` prefix
    /// form splits the erased value from its modifiers. Methods inside an
    /// `impl` block are exempt: component internals that construct an
    /// `AnyView` for storage keep the constructor form.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// match state {
    ///     DisplayState::Empty => AnyView::new(vstack((..)).spacing(8.0)),
    ///     DisplayState::Loaded(media) => AnyView::new(media_view(media)),
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// match state {
    ///     DisplayState::Empty => vstack((..)).spacing(8.0).anyview(),
    ///     DisplayState::Loaded(media) => media_view(media).anyview(),
    /// }
    /// ```
    pub ANYVIEW_NEW_IN_FREE_FN,
    style,
    "`AnyView::new` in a free function instead of the postfix `.anyview()`"
}

declare_lint_pass!(AnyviewNewInFreeFn => [ANYVIEW_NEW_IN_FREE_FN]);

/// `waterui_internal::view::ViewExt` — the trait `.anyview()` comes from.
const VIEW_EXT: &str = "waterui_internal::view::ViewExt";

/// The `use` line the fix writes — spelled through the `waterui` facade,
/// which is how users import it.
const VIEW_EXT_USE: &str = "waterui::view::ViewExt";

const MSG: &str = "`AnyView::new` in a free function; use the postfix `.anyview()`";
const SUGGESTION: &str = "use the postfix `.anyview()`";
const SCOPE_HELP: &str =
    "bring `ViewExt` into scope (`use waterui::view::ViewExt;`) to write `.anyview()`";

/// Whether `hir_id` sits inside a free `fn`: the nearest enclosing
/// item-level node is an `ItemKind::Fn`. `impl` and trait methods,
/// `const`/`static` initialisers, and foreign items are not free
/// functions; a closure inside a free `fn` keeps the function's answer.
fn in_free_fn(tcx: TyCtxt<'_>, hir_id: HirId) -> bool {
    for (_, node) in tcx.hir_parent_iter(hir_id) {
        match node {
            Node::Item(item) => return matches!(item.kind, ItemKind::Fn { .. }),
            Node::ImplItem(_) | Node::TraitItem(_) | Node::ForeignItem(_) => return false,
            _ => {}
        }
    }
    false
}

/// Whether `expr` is the receiver of a `.anyview()` call — the outer
/// erasure is `redundant_anyview`'s, and rewriting the inner call would
/// produce `x.anyview().anyview()`.
fn anyview_receiver(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let Node::Expr(parent) = cx.tcx.parent_hir_node(expr.hir_id) else {
        return false;
    };
    let ExprKind::MethodCall(_, receiver, [], _) = parent.kind else {
        return false;
    };
    receiver.hir_id == expr.hir_id
        && cx
            .typeck_results()
            .type_dependent_def_id(parent.hir_id)
            .is_some_and(|did| def_path_eq(cx, did, ANYVIEW_EXT))
}

/// The lint with no suggestion — `.anyview()` needs `ViewExt` in scope and
/// the trait could not be resolved or its `use` could not be placed.
fn report_help(cx: &LateContext<'_>, expr: &Expr<'_>) {
    span_lint_and_help(cx, ANYVIEW_NEW_IN_FREE_FN, expr.span, MSG, None, SCOPE_HELP);
}

impl<'tcx> LateLintPass<'tcx> for AnyviewNewInFreeFn {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::Call(func, [inner]) = expr.kind else {
            return;
        };
        let Res::Def(DefKind::AssocFn, did) = func.res(cx) else {
            return;
        };
        if !def_path_eq(cx, did, ANYVIEW_NEW)
            // Erasing an `AnyView` again is `redundant_anyview`'s call.
            || is_anyview(cx, cx.typeck_results().expr_ty(inner))
            || anyview_receiver(cx, expr)
            || !in_free_fn(cx.tcx, expr.hir_id)
        {
            return;
        }
        let mut applicability = Applicability::MachineApplicable;
        let receiver =
            Sugg::hir_with_applicability(cx, inner, "..", &mut applicability).maybe_paren();
        let view_ext = lookup_path_str(cx.tcx, PathNS::Type, VIEW_EXT)
            .first()
            .copied();
        let status = bare_status(
            cx,
            expr.hir_id,
            expr.span.lo(),
            Symbol::intern("ViewExt"),
            Namespace::TypeNS,
            view_ext,
        );
        let mut parts = Vec::new();
        match status {
            Bare::Same => {}
            Bare::Free | Bare::Conflict | Bare::Unknown => {
                if view_ext.is_none() {
                    return report_help(cx, expr);
                }
                let Some((point, before, after)) = use_insertion(cx, expr.hir_id, VIEW_EXT_USE)
                else {
                    return report_help(cx, expr);
                };
                // `ViewExt` resolving to something else here — or a
                // function-local glob that could resolve it either way —
                // gets the anonymous-import form: the trait's methods
                // enter scope without taking the name.
                let alias = if matches!(status, Bare::Free) {
                    ""
                } else {
                    applicability = Applicability::MaybeIncorrect;
                    " as _"
                };
                parts.push((point, format!("{before}use {VIEW_EXT_USE}{alias};{after}")));
            }
        }
        parts.push((expr.span, format!("{receiver}.anyview()")));
        span_lint_and_then(cx, ANYVIEW_NEW_IN_FREE_FN, expr.span, MSG, |diag| {
            diag.multipart_suggestion(SUGGESTION, parts, applicability);
        });
    }
}
