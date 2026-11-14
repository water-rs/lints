//! Detection of `Signal::get`/`Binding::get` snapshot reads — a `.get()`
//! that resolves to one of `SNAPSHOT_GETS` freezes the signal into a plain
//! value at the point of the call.

use std::ops::ControlFlow;

use clippy_utils::eq_expr_value;
use clippy_utils::res::MaybeQPath;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::{AssocKind, Ty, TypeckResults, Unnormalized};
use rustc_span::{Symbol, SyntaxContext};

use crate::binding::BINDING_GET_MUT;
use crate::carriers::strip_wraps;
use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};

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
    is_snapshot_get_in(cx, cx.typeck_results(), expr)
}

/// [`is_snapshot_get`] against an explicit `TypeckResults` — for callers
/// examining expressions that live in a body other than the one `cx`
/// currently type-checks (e.g. a `let` use scanned while visiting the map
/// that initializes it).
pub(crate) fn is_snapshot_get_in(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'_>,
    expr: &Expr<'_>,
) -> bool {
    let did = match expr.kind {
        ExprKind::MethodCall(..) => typeck.type_dependent_def_id(expr.hir_id),
        ExprKind::Call(func, _) => match func.res(typeck) {
            Res::Def(DefKind::AssocFn, did) => Some(did),
            _ => None,
        },
        _ => None,
    };
    let Some(did) = did else { return false };
    let did = crate::param_bounds::implemented_trait_item(cx.tcx, did);
    SNAPSHOT_GETS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, did, path))
}

/// Whether `ty` is a live signal handle — `Binding`, `Computed`, a derived
/// signal, or any other `Signal` whose `watch` produces a real guard.
/// `nami`'s `impl_constant!` gives `bool`, the numbers, `String`, `Duration`,
/// and the standard collections a `Signal` impl whose `watch` is a no-op
/// (`type Guard = ()`), so the trait alone cannot tell a live handle from
/// data — a non-unit normalized `<ty as Signal>::Guard` is exactly what
/// makes the type live. An unresolvable projection counts as live: the type
/// cannot be proven a plain value.
pub(crate) fn is_live_signal<'tcx>(
    cx: &LateContext<'tcx>,
    signal_dids: &[DefId],
    ty: Ty<'tcx>,
) -> bool {
    signal_dids
        .iter()
        .filter(|trait_did| implements_trait(cx, ty, **trait_did, &[]))
        .any(|trait_did| {
            let Some(guard_did) = cx
                .tcx
                .associated_items(*trait_did)
                .filter_by_name_unhygienic(Symbol::intern("Guard"))
                .find(|item| matches!(item.kind, AssocKind::Type { .. }))
                .map(|item| item.def_id)
            else {
                return true;
            };
            let projection =
                Ty::new_projection_from_args(cx.tcx, guard_did, cx.tcx.mk_args(&[ty.into()]));
            match cx
                .tcx
                .try_normalize_erasing_regions(cx.typing_env(), Unnormalized::new_wip(projection))
            {
                Ok(guard_ty) => !guard_ty.is_unit(),
                Err(_) => true,
            }
        })
}

/// The receiver (`x` in `x.get()` / `Signal::get(x)`).
pub(crate) fn get_receiver<'tcx>(expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    match expr.kind {
        ExprKind::MethodCall(_, receiver, [], _) => Some(receiver),
        ExprKind::Call(_, [receiver]) => Some(receiver),
        _ => None,
    }
}

/// Whether `expr` reads `binding` back — a snapshot `.get()` or a
/// `b.get_mut()` guard — nested closures included, with carriers stripped on
/// the read's receiver so `&b`, `b.clone()`, `state.count` match. A read a
/// macro wrote (`text!`'s subscription plumbing) is not the user's.
pub(crate) fn reads_binding<'tcx>(
    cx: &LateContext<'tcx>,
    ctxt: SyntaxContext,
    binding: &'tcx Expr<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> bool {
    for_each_expr(cx, expr, |e| -> ControlFlow<(), Descend> {
        if e.span.from_expansion() {
            return ControlFlow::Continue(Descend::No);
        }
        // `e` may live in a nested body; resolve its reads through the
        // typeck of the body that owns it.
        let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(e.hir_id));
        let is_read = is_snapshot_get_in(cx, typeck, e)
            || call_def_id(typeck, e).is_some_and(|did| {
                def_path_eq(cx, implemented_trait_item(cx.tcx, did), BINDING_GET_MUT)
            });
        let receiver = if is_read { get_receiver(e) } else { None };
        match receiver {
            Some(receiver)
                if eq_expr_value(cx, ctxt, strip_wraps(cx, typeck, receiver), binding) =>
            {
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(Descend::Yes),
        }
    })
    .is_some()
}
