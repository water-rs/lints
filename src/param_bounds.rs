use clippy_utils::ty::all_predicates_of;
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty::{
    AssocContainer, ClauseKind, EarlyBinder, GenericArg, GenericArgsRef, PredicatePolarity, Ty,
    TyCtxt, TyKind, TypeckResults,
};

/// A trait bound a callee's parameter carries, matched against a lint's
/// target bound table.
#[derive(Clone, Copy)]
pub(crate) struct BoundTarget<'tcx> {
    /// The bound trait's `DefId`.
    pub trait_did: DefId,
    /// The bound's generic arguments minus `Self` (e.g. `[f32]` for
    /// `IntoComputed<f32>`), instantiated with the call site's substitutions.
    pub args: &'tcx [GenericArg<'tcx>],
}

/// Parameter bounds that take a live signal — a value passed to one of
/// these is subscribed to, not snapshotted.
pub(crate) const SIGNAL_PARAM_BOUNDS: &[&[&str]] = &[
    &["nami", "reactive_core", "signal", "IntoSignal"],
    &["nami", "reactive_core", "signal", "IntoComputed"],
    &["waterui_core", "state", "computed_f32", "IntoSignalF32"],
    &["waterui_text", "text", "IntoText"],
    &["waterui_controls", "label", "IntoLabel"],
];

/// Parameter bounds that subscribe to a signal into a *property* — the
/// `SIGNAL_PARAM_BOUNDS` subset at which a plain value is a frozen snapshot
/// of the data it was read from. `IntoText`/`IntoLabel` are absent:
/// rendering text from an item field is ordinary row content, not the bug
/// `collection_item_snapshot` hunts.
pub(crate) const SNAPSHOT_PARAM_BOUNDS: &[&[&str]] = &[
    &["nami", "reactive_core", "signal", "IntoSignal"],
    &["nami", "reactive_core", "signal", "IntoComputed"],
    &["waterui_core", "state", "computed_f32", "IntoSignalF32"],
];

/// `waterui_core::foundation::handler::ViewBuilder` — the `VIEW_PARAM_BOUNDS`
/// entry for a view-factory parameter: a closure argument to one is a builder
/// the callee invokes to produce a view each time it rebuilds.
pub(crate) const VIEW_BUILDER: &[&str] = &["waterui_core", "foundation", "handler", "ViewBuilder"];

/// Parameter bounds that take a view — a subtree, not a value.
pub(crate) const VIEW_PARAM_BOUNDS: &[&[&str]] =
    &[&["waterui_core", "ui", "view", "View"], VIEW_BUILDER];

/// `param index -> bounds on it that are in `bounds_tables``, for one callee.
/// Walks the `parent` chain so impl-level bounds (`impl<V: View>`) and trait
/// supertraits (`trait ViewExt: View`) are seen alongside the callee's own
/// `where`/APIT clauses.
fn param_trait_bounds<'tcx>(
    cx: &LateContext<'tcx>,
    callee: DefId,
    substs: GenericArgsRef<'tcx>,
    bounds_tables: &[&[&[&'static str]]],
) -> FxHashMap<u32, Vec<BoundTarget<'tcx>>> {
    let mut bounds: FxHashMap<u32, Vec<BoundTarget<'tcx>>> = FxHashMap::default();
    for &(clause, _) in all_predicates_of(cx.tcx, callee) {
        let ClauseKind::Trait(pred) = clause.kind().skip_binder() else {
            continue;
        };
        if pred.polarity != PredicatePolarity::Positive
            || !bounds_tables
                .iter()
                .flat_map(|table| table.iter().copied())
                .any(|path| crate::def_path::def_path_eq(cx, pred.trait_ref.def_id, path))
        {
            continue;
        }
        let TyKind::Param(param) = *pred.trait_ref.self_ty().kind() else {
            continue;
        };
        let instantiated = EarlyBinder::bind(clause).instantiate(cx.tcx, substs);
        let ClauseKind::Trait(inst_pred) = instantiated.kind().skip_norm_wip().skip_binder() else {
            continue;
        };
        bounds.entry(param.index).or_default().push(BoundTarget {
            trait_did: pred.trait_ref.def_id,
            args: &inst_pred.trait_ref.args[1..],
        });
    }
    bounds
}

/// The bounds on `input` — a callee's declared parameter type — if it is a
/// type parameter carrying any of the target bounds.
fn bounds_on_param<'a, 'tcx>(
    bounds: &'a FxHashMap<u32, Vec<BoundTarget<'tcx>>>,
    input: Ty<'tcx>,
) -> Option<&'a Vec<BoundTarget<'tcx>>> {
    let TyKind::Param(param) = *input.peel_refs().kind() else {
        return None;
    };
    bounds.get(&param.index)
}

/// The callee and its call-site substitutions for a `Call`/`MethodCall`.
/// Same contract as `clippy_utils::fn_def_id_with_node_args`, but resolves
/// through `typeck` so callers inside a nested body (a closure scanned while
/// visiting its parent) can pass the `TypeckResults` of the body containing
/// `call`.
fn call_target<'tcx>(
    typeck: &TypeckResults<'tcx>,
    expr: &Expr<'tcx>,
) -> Option<(DefId, GenericArgsRef<'tcx>)> {
    match expr.kind {
        ExprKind::MethodCall(..) => Some((
            typeck.type_dependent_def_id(expr.hir_id)?,
            typeck.node_args(expr.hir_id),
        )),
        ExprKind::Call(
            Expr {
                kind: ExprKind::Path(qpath),
                hir_id: path_hir_id,
                ..
            },
            ..,
        ) => {
            // Only return Fn-like DefIds, not the DefIds of statics/consts/etc that contain or
            // deref to fn pointers, dyn Fn, impl Fn - #8850
            if let Res::Def(DefKind::Fn | DefKind::Ctor(..) | DefKind::AssocFn, id) =
                typeck.qpath_res(qpath, *path_hir_id)
            {
                Some((id, typeck.node_args(*path_hir_id)))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The resolved callee `DefId` of a `Call`/`MethodCall` — for lints that
/// need what a call resolves to without its parameter bounds.
pub(crate) fn call_def_id<'hir>(typeck: &TypeckResults<'hir>, expr: &Expr<'hir>) -> Option<DefId> {
    call_target(typeck, expr).map(|(did, _)| did)
}

/// The trait item `did` implements — `impl Trait for T::f` points back at
/// the trait's `f` — or `did` itself for inherent items and free items. A
/// method call on a trait method resolves to the impl's method, whose def
/// path is `<impl Trait for T>::f`; normalizing onto the trait item lets
/// def-path matching hit `path::to::Trait::f`.
pub(crate) fn implemented_trait_item(tcx: TyCtxt<'_>, did: DefId) -> DefId {
    match tcx.opt_associated_item(did).map(|assoc| assoc.container) {
        Some(AssocContainer::TraitImpl(Ok(trait_item))) => trait_item,
        _ => did,
    }
}

/// The arguments of `call` in parameter order — the receiver counts as
/// position 0 for method calls.
pub(crate) fn call_args<'a, 'hir>(call: &'a Expr<'hir>) -> Vec<&'a Expr<'hir>> {
    match call.kind {
        ExprKind::Call(_, args) => args.iter().collect(),
        ExprKind::MethodCall(_, receiver, args, _) => {
            std::iter::once(receiver).chain(args.iter()).collect()
        }
        _ => Vec::new(),
    }
}

/// `(callee, per-argument bound targets)` for a `Call`/`MethodCall`: for each
/// argument position, the `bounds_tables` bounds the matching parameter
/// carries — an empty vec for positions without one. `typeck` must be the
/// `TypeckResults` of the body containing `call`.
pub(crate) fn call_arg_bounds_in<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    call: &Expr<'tcx>,
    bounds_tables: &[&[&[&'static str]]],
) -> Option<(DefId, Vec<Vec<BoundTarget<'tcx>>>)> {
    let (callee, substs) = call_target(typeck, call)?;
    let bounds = param_trait_bounds(cx, callee, substs, bounds_tables);
    if bounds.is_empty() {
        return None;
    }
    let per_arg = cx
        .tcx
        .fn_sig(callee)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder()
        .inputs()
        .iter()
        .map(|input| {
            bounds_on_param(&bounds, *input)
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    Some((callee, per_arg))
}

/// [`call_arg_bounds_in`] with the lint's current `TypeckResults`.
pub(crate) fn call_arg_bounds<'tcx>(
    cx: &LateContext<'tcx>,
    call: &Expr<'tcx>,
    bounds_tables: &[&[&[&'static str]]],
) -> Option<(DefId, Vec<Vec<BoundTarget<'tcx>>>)> {
    call_arg_bounds_in(cx, cx.typeck_results(), call, bounds_tables)
}

/// Whether `call`'s parameter at `index` carries any of `bounds_tables`'
/// bounds. `typeck` must be the `TypeckResults` of the body containing
/// `call`.
pub(crate) fn arg_has_bound<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    call: &Expr<'tcx>,
    bounds_tables: &[&[&[&'static str]]],
    index: usize,
) -> bool {
    call_arg_bounds_in(cx, typeck, call, bounds_tables)
        .is_some_and(|(_, bounds)| bounds.get(index).is_some_and(|targets| !targets.is_empty()))
}
