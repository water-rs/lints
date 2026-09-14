//! Shared machinery for the `AnyView` lints: recognizing the calls that
//! produce an `AnyView` and the value they erase.

use clippy_utils::res::MaybeQPath;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::{Ty, TypeckResults};

/// `waterui_core::components::anyview::AnyView` — the erasure target.
pub(crate) const ANYVIEW: &[&str] = &["waterui_core", "components", "anyview", "AnyView"];

/// `AnyView::new` — an inherent associated function on `AnyView`.
const ANYVIEW_NEW: &[&str] = &["waterui_core", "components", "anyview", "AnyView", "new"];

/// `ViewExt::anyview` — the extension method producing `AnyView`.
const ANYVIEW_EXT: &[&str] = &["waterui_internal", "view", "ViewExt", "anyview"];

/// The erasure-producing calls, by defining-crate path.
const ERASURE_CALLS: &[&[&str]] = &[ANYVIEW_NEW, ANYVIEW_EXT];

/// The view `expr` erases — the argument of `AnyView::new(v)` or the receiver
/// of `v.anyview()`/`ViewExt::anyview(v)` — when `expr` is exactly such a
/// call. `typeck` must be the `TypeckResults` of the body containing `expr`.
pub(crate) fn erased_inner<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<&'tcx Expr<'tcx>> {
    if expr.span.from_expansion() {
        return None;
    }
    let (def_id, inner) = match expr.kind {
        ExprKind::Call(func, [inner]) => match func.res(typeck) {
            Res::Def(DefKind::AssocFn, did) => (did, inner),
            _ => return None,
        },
        ExprKind::MethodCall(_, receiver, [], _) => {
            (typeck.type_dependent_def_id(expr.hir_id)?, receiver)
        }
        _ => return None,
    };
    ERASURE_CALLS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, def_id, path))
        .then_some(inner)
}

/// `expr` with drop-temps and block wrappers removed — the value that
/// actually occupies the position.
pub(crate) fn peel<'tcx>(expr: &'tcx Expr<'tcx>) -> &'tcx Expr<'tcx> {
    let mut expr = expr;
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(block, _) => match block.expr {
                Some(tail) => tail,
                None => break,
            },
            _ => break,
        };
    }
    expr
}

/// Whether `ty`, references peeled, is `waterui_core`'s `AnyView` — i.e. the
/// value an erasure call wraps is already erased. Those calls belong to
/// `redundant_anyview`; `needless_anyview` steps aside for them.
pub(crate) fn is_anyview(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    ty.peel_refs()
        .ty_adt_def()
        .is_some_and(|adt| crate::def_path::def_path_eq(cx, adt.did(), ANYVIEW))
}
