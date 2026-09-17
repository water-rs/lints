//! `Computed` erasure machinery shared by `manual_computed` (which prefers
//! the `.computed()` spelling) and `needless_computed` (which drops the
//! erasure where every consumer accepts the un-erased signal).

use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::ty::{implements_trait, make_normalized_projection};
use rustc_hir::Expr;
use rustc_lint::LateContext;
use rustc_middle::ty::{TyKind, TypeVisitableExt, TypeckResults};
use rustc_span::sym;

use crate::binding::COMPUTED;
use crate::def_path::def_path_eq;

/// `Computed::new` — the constructor spelling of the erasure.
/// `Computed::constant` is absent: there is no source signal to unwrap.
pub(crate) const COMPUTED_NEW: &[&str] = &[
    "nami",
    "reactive_core",
    "signal",
    "computed",
    "Computed",
    "new",
];

/// `IntoComputed::into_computed` — the blanket trait form of the erasure.
pub(crate) const INTO_COMPUTED: &[&str] = &[
    "nami",
    "reactive_core",
    "signal",
    "IntoComputed",
    "into_computed",
];

/// `nami_core::Signal` — the trait the erased value must implement.
/// `SignalExt::computed`/`Computed::new` guarantee it by signature;
/// `IntoComputed::into_computed` does not — an `impl IntoComputed` receiver
/// is not a signal, and the trait is free to convert the output (`Output:
/// From<C::Output>`) — so both are checked in [`erases_signal`].
pub(crate) const SIGNAL: &str = "nami_core::Signal";

/// `result` is `Computed<T>` where `signal` implements
/// `Signal<Output = T>` — the erasure `x.computed()` performs. The `Output`
/// equality rejects `IntoComputed::into_computed` calls that convert the
/// value, and the `Signal` check rejects receivers that are only `impl
/// IntoComputed` (an `impl IntoComputed` parameter is not a signal, and the
/// erasure is load-bearing there).
///
/// `expr_ty`, not `expr_ty_adjusted`: a `&self` receiver's adjusted type is
/// the autoref `&C`, which is not the signal; for the by-value receivers
/// and arguments every spelling takes, the two agree.
pub(crate) fn erases_signal<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    result: &Expr<'tcx>,
    signal: &Expr<'tcx>,
) -> bool {
    let TyKind::Adt(adt, args) = *typeck.expr_ty(result).kind() else {
        return false;
    };
    if !def_path_eq(cx, adt.did(), COMPUTED) {
        return false;
    }
    // `e.computed()` yields `Computed<C::Output>` — the rewrite is the same
    // erasure only when that `Output` is this `Computed`'s `T`.
    let output = args.type_at(0);
    let signal_ty = typeck.expr_ty(signal);
    if output.has_infer() || signal_ty.has_infer() {
        return false;
    }
    let Some(signal_trait) = lookup_path_str(cx.tcx, PathNS::Type, SIGNAL)
        .first()
        .copied()
    else {
        return false;
    };
    implements_trait(cx, signal_ty, signal_trait, &[])
        && make_normalized_projection(
            cx.tcx,
            cx.typing_env(),
            signal_trait,
            sym::Output,
            [signal_ty],
        ) == Some(output)
}
