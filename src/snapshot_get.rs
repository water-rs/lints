//! Detection of `Signal::get`/`Binding::get` snapshot reads — a `.get()`
//! that resolves to one of `SNAPSHOT_GETS` freezes the signal into a plain
//! value at the point of the call.

use clippy_utils::res::MaybeQPath;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::AssocContainer;

/// Def paths of the snapshot reads this lint tracks: the `Signal` trait's
/// `get` (reached through `Computed`, `Map`, `WithMetadata`, `SignalExt`
/// types, …) and `Binding`'s inherent `get`, which shadows the trait method
/// in method resolution.
const SNAPSHOT_GETS: &[&[&str]] = &[
    &["nami_core", "Signal", "get"],
    &["nami", "reactive_core", "binding", "Binding", "get"],
];

/// Whether `expr` is a `x.get()` resolving to `Signal::get` or `Binding::get`.
///
/// A `computed.get()` call resolves to the method inside `impl Signal for
/// Computed`, whose def path is `<impl Signal for Computed>::get` — not the
/// trait's. `AssocContainer::TraitImpl` points back at the implemented trait
/// item, which normalizes those calls onto `nami_core::Signal::get`. The
/// inherent `Binding::get` keeps its own `InherentImpl` def path.
pub(crate) fn is_snapshot_get(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let did = match expr.kind {
        ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
        ExprKind::Call(func, _) => match func.res(cx) {
            Res::Def(DefKind::AssocFn, did) => Some(did),
            _ => None,
        },
        _ => None,
    };
    let Some(did) = did else { return false };
    let did = match cx.tcx.associated_item(did).container {
        AssocContainer::TraitImpl(Ok(trait_item)) => trait_item,
        _ => did,
    };
    SNAPSHOT_GETS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, did, path))
}

/// The receiver (`x` in `x.get()` / `Signal::get(x)`).
pub(crate) fn get_receiver<'tcx>(expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    match expr.kind {
        ExprKind::MethodCall(_, receiver, [], _) => Some(receiver),
        ExprKind::Call(_, [receiver]) => Some(receiver),
        _ => None,
    }
}
