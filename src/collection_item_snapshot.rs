use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::MaybeResPath;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind, HirId};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{AssocKind, Ty, TypeckResults, Unnormalized};
use rustc_session::declare_lint_pass;
use rustc_span::Symbol;
use std::ops::ControlFlow;

use crate::carriers::{carried_arg, strip_wraps};
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::{SNAPSHOT_PARAM_BOUNDS, arg_has_bound};
use crate::row_builder::row_builder_closure;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a read of a non-signal field of a `for_each` item —
    /// `row.unread`, `row.alpha`, or a field bound by destructuring the
    /// generator's parameter — inside the generator closure of
    /// `ForEach::new`, `Lazy::for_each`, `List::for_each`, or
    /// `VStack`/`HStack`/`ZStack::for_each`, when the read flows through
    /// value carriers (`.clone()`, `&`, `format!(..)`, …) into a parameter
    /// bound by `IntoSignal`, `IntoComputed`, or `IntoSignalF32`:
    /// `.visible(..)`, `.opacity(..)`, `.disabled(..)`, or a `when`/`.or`
    /// condition. `IntoText`/`IntoLabel` positions are out of scope —
    /// rendering text from item fields is ordinary row content.
    ///
    /// ### Why is this bad?
    ///
    /// `for_each` diffs rows by id: mutating a field changes no id, so the
    /// row is never rebuilt and the snapshot taken while building it never
    /// updates. A "mark all read" that clears the model leaves every unread
    /// dot lit.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// Lazy::for_each(list, |row| text(row.title.clone()).visible(row.unread))
    /// ```
    ///
    /// Derive the property as a `Computed` from the owning collection, or
    /// keep a `Binding` on the item, so the row updates in place:
    ///
    /// ```rust,ignore
    /// Lazy::for_each(list, |row| {
    ///     text(row.title.clone()).visible(row.unread.clone())
    /// })
    /// ```
    pub COLLECTION_ITEM_SNAPSHOT,
    suspicious,
    "a non-signal field of a `for_each` item drives a reactive property"
}

declare_lint_pass!(CollectionItemSnapshot => [COLLECTION_ITEM_SNAPSHOT]);

/// `nami_core::Signal` — `Binding`, `Computed`, and every derived signal
/// implement it, but so do plain values: `impl_constant!` gives `bool`,
/// the numbers, `String`, `Duration`, and the standard collections a `Signal`
/// impl whose `watch` is a no-op (`type Guard = ()`). The trait alone cannot
/// tell a live handle from data — the guard type can.
const SIGNAL: &str = "nami_core::Signal";

const MESSAGE: &str = "this reactive property is a snapshot of an item field";
const HELP: &str = "derive it as a `Computed` from the owning collection, or keep a `Binding` on the item, so the row updates in place; `#[allow(collection_item_snapshot)]` on the closure if the field is immutable row data";

/// `expr` as an item field read — `Some((name, ty))` for the label and the
/// `Signal` check — when `expr` is a `Field` whose base (parens/`&`/
/// `.clone()` peeled) resolves to a binding of the generator's parameter
/// pattern, or a direct read of a field that pattern destructures
/// (`|Item { unread, .. }|` binds `unread` itself). `typeck` must be the
/// `TypeckResults` of the body containing `expr`.
fn item_field_read<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    item_pat: HirId,
    bindings: &[HirId],
    expr: &'tcx Expr<'tcx>,
) -> Option<(Symbol, Ty<'tcx>)> {
    match expr.kind {
        ExprKind::Field(base, field)
            if strip_wraps(cx, typeck, base)
                .res_local_id()
                .is_some_and(|id| bindings.contains(&id)) =>
        {
            Some((field.name, typeck.expr_ty(expr)))
        }
        // A binding of the parameter pattern other than the root — which
        // binds the whole item — is a destructured field.
        _ => expr
            .res_local_id_and_ident()
            .filter(|(id, _)| *id != item_pat && bindings.contains(id))
            .map(|(_, ident)| (ident.name, typeck.expr_ty(expr))),
    }
}

/// Whether `ty` is a live signal handle — `Binding`, `Computed`, a derived
/// signal, or any other `Signal` whose `watch` produces a real guard. The
/// constant impls set `type Guard = ()` because their `watch` is a no-op, so
/// a non-unit normalized `<ty as Signal>::Guard` is exactly what makes the
/// field live. An unresolvable projection stays silent: the field cannot be
/// proven a plain value.
fn is_live_signal<'tcx>(cx: &LateContext<'tcx>, signal_dids: &[DefId], ty: Ty<'tcx>) -> bool {
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

impl<'tcx> LateLintPass<'tcx> for CollectionItemSnapshot {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let Some(closure) = row_builder_closure(cx, expr) else {
            return;
        };
        let body = cx.tcx.hir_body(closure.body);
        // The generator is `Fn(C::Item) -> V`: arity one.
        let [param] = body.params else {
            return;
        };
        let mut bindings = Vec::new();
        param
            .pat
            .each_binding(|_, hir_id, _, _| bindings.push(hir_id));
        let signal_dids = lookup_path_str(cx.tcx, PathNS::Type, SIGNAL);
        for_each_expr(cx, body.value, |expr| {
            if expr.span.from_expansion() {
                return ControlFlow::<(), Descend>::Continue(Descend::Yes);
            }
            let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(expr.hir_id));
            let Some((field, ty)) = item_field_read(cx, typeck, param.pat.hir_id, &bindings, expr)
            else {
                return ControlFlow::Continue(Descend::Yes);
            };
            if is_live_signal(cx, &signal_dids, ty) {
                return ControlFlow::Continue(Descend::Yes);
            }
            let Some((call, index)) = carried_arg(cx, typeck, expr) else {
                return ControlFlow::Continue(Descend::Yes);
            };
            if !arg_has_bound(cx, typeck, call, &[SNAPSHOT_PARAM_BOUNDS], index) {
                return ControlFlow::Continue(Descend::Yes);
            }
            span_lint_and_then(cx, COLLECTION_ITEM_SNAPSHOT, call.span, MESSAGE, |diag| {
                diag.span_label(
                    expr.span,
                    format!(
                        "changes to `{field}` will not re-render the row unless the item's id changes"
                    ),
                );
                diag.help(HELP);
            });
            ControlFlow::Continue(Descend::Yes)
        });
    }
}
