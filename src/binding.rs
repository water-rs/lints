//! `nami`'s `Binding` def paths and method-applicability checks, shared by
//! lints that match binding reads and writes.

use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::ty::{implements_trait, make_normalized_projection};
use rustc_hir::{BinOpKind, BorrowKind, Expr, ExprKind, LangItem, Mutability};
use rustc_lint::LateContext;
use rustc_middle::ty::adjustment::{Adjust, AutoBorrow, AutoBorrowMutability};
use rustc_middle::ty::{Ty, TyKind, TypeVisitableExt, TypeckResults};
use rustc_span::{Pos, sym};

/// `nami`'s `Binding<T>` — the writable signal handle.
pub(crate) const BINDING: &[&str] = &["nami", "reactive_core", "binding", "Binding"];

/// `Binding::set` — the inherent write that publishes a new value. Method
/// resolution prefers it over the `CustomBinding`/`BindingImpl` trait
/// methods, so `b.set(..)` on a `Binding` resolves here.
pub(crate) const BINDING_SET: &[&str] = &["nami", "reactive_core", "binding", "Binding", "set"];

/// `Binding::get_mut` — the inherent live-guard accessor; `*b.get_mut() = x`
/// and `*b.get_mut() op= x` are hand-written `set`/`<op>_assign`.
pub(crate) const BINDING_GET_MUT: &[&str] =
    &["nami", "reactive_core", "binding", "Binding", "get_mut"];

/// `Binding::with_mut` — the inherent closure-scoped mutation.
pub(crate) const BINDING_WITH_MUT: &[&str] =
    &["nami", "reactive_core", "binding", "Binding", "with_mut"];

/// The `T` in `Binding<T>` for a receiver expression under `typeck` —
/// `None` for receivers that are not a `Binding` (a `CustomBinding` impl has
/// none of the inherent methods the binding lints' suggestions name).
pub(crate) fn binding_value_ty<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    receiver: &Expr<'tcx>,
) -> Option<Ty<'tcx>> {
    let TyKind::Adt(adt, args) = *typeck.expr_ty_adjusted(receiver).peel_refs().kind() else {
        return None;
    };
    crate::def_path::def_path_eq(cx, adt.did(), BINDING).then(|| args.type_at(0))
}

/// `rhs` spelled so that a generic parameter sees the type the operator
/// saw: the source itself when no coercion applied, `&*x` when the operand
/// was reborrowed through `n` derefs (`&String` → `&str`), and `None` for
/// any other adjustment, which the named methods cannot reproduce.
pub(crate) fn coerced_operand(
    typeck: &TypeckResults<'_>,
    rhs: &Expr<'_>,
    src: &str,
) -> Option<String> {
    let adjustments = typeck.expr_adjustments(rhs);
    // No adjustment, or a reborrow that lands on the same type (`&str` from
    // a `&str` literal): the source already has the operator's type.
    if adjustments.is_empty() || typeck.expr_ty(rhs) == typeck.expr_ty_adjusted(rhs) {
        return Some(src.to_owned());
    }
    let [derefs @ .., borrow] = adjustments else {
        return None;
    };
    if !matches!(
        borrow.kind,
        Adjust::Borrow(AutoBorrow::Ref(AutoBorrowMutability::Not))
    ) || !derefs
        .iter()
        .all(|adjustment| matches!(adjustment.kind, Adjust::Deref(_)))
    {
        return None;
    }
    // `&*(&x)` is `&x` with one deref fewer.
    let (stars, inner) = match rhs.kind {
        ExprKind::AddrOf(BorrowKind::Ref, Mutability::Not, inner) => (
            derefs.len().checked_sub(1)?,
            snippet_opt_expr(inner, src, rhs)?,
        ),
        _ => (derefs.len(), src.to_owned()),
    };
    let inner = if matches!(
        rhs.kind,
        ExprKind::Path(_) | ExprKind::Field(..) | ExprKind::AddrOf(..)
    ) {
        inner
    } else {
        format!("({inner})")
    };
    Some(format!("&{}{inner}", "*".repeat(stars)))
}

/// The source of `inner`, an operand nested in `outer` whose source is
/// `outer_src` — sliced rather than re-read so a macro-mapped span cannot
/// point elsewhere.
fn snippet_opt_expr(inner: &Expr<'_>, outer_src: &str, outer: &Expr<'_>) -> Option<String> {
    let (lo, hi) = (
        (inner.span.lo() - outer.span.lo()).to_usize(),
        (inner.span.hi() - outer.span.lo()).to_usize(),
    );
    outer_src.get(lo..hi).map(str::to_owned)
}

/// The `Binding<T>` method that applies `op` in place — `add_assign` for
/// `+` — paired with the lang item of the operator trait `T` must implement
/// for it (`Binding::add_assign` needs `T: Add<Output = T> + Clone`).
pub(crate) fn op_assign_method(op: BinOpKind) -> Option<(&'static str, LangItem)> {
    Some(match op {
        BinOpKind::Add => ("add_assign", LangItem::Add),
        BinOpKind::Sub => ("sub_assign", LangItem::Sub),
        BinOpKind::Mul => ("mul_assign", LangItem::Mul),
        BinOpKind::Div => ("div_assign", LangItem::Div),
        BinOpKind::Rem => ("rem_assign", LangItem::Rem),
        BinOpKind::BitAnd => ("bitand_assign", LangItem::BitAnd),
        BinOpKind::BitOr => ("bitor_assign", LangItem::BitOr),
        BinOpKind::BitXor => ("bitxor_assign", LangItem::BitXor),
        BinOpKind::Shl => ("shl_assign", LangItem::Shl),
        BinOpKind::Shr => ("shr_assign", LangItem::Shr),
        _ => return None,
    })
}

/// The name of the `Binding<T>` method that applies `op` with `rhs_ty` in
/// place — when `rhs_ty` is `T` itself and `T: Op<T, Output = T> + Clone`,
/// which is what `Binding::<op>_assign(other: T)` requires. `None` for any
/// other operand type: `String + &str` has no `add_assign` on the binding.
pub(crate) fn named_op_assign<'tcx>(
    cx: &LateContext<'tcx>,
    value_ty: Ty<'tcx>,
    op: BinOpKind,
    rhs_ty: Ty<'tcx>,
) -> Option<&'static str> {
    let (method, item) = op_assign_method(op)?;
    if value_ty.has_infer() || rhs_ty != value_ty {
        return None;
    }
    let trait_did = cx.tcx.lang_items().get(item)?;
    let clone = cx.tcx.lang_items().clone_trait()?;
    (implements_trait(cx, value_ty, trait_did, &[value_ty.into()])
        && implements_trait(cx, value_ty, clone, &[])
        && make_normalized_projection(
            cx.tcx,
            cx.typing_env(),
            trait_did,
            sym::Output,
            [value_ty, value_ty],
        ) == Some(value_ty))
    .then_some(method)
}

/// Whether `Binding<T>::append(ele)` accepts an `ele_ty` element: the
/// method requires `T: Clone + Extend<E>` (`String: Extend<&str>`,
/// `Vec<T>: Extend<T>`).
pub(crate) fn extend_accepts<'tcx>(
    cx: &LateContext<'tcx>,
    value_ty: Ty<'tcx>,
    ele_ty: Ty<'tcx>,
) -> bool {
    if value_ty.has_infer() || ele_ty.has_infer() {
        return false;
    }
    let Some(&extend) = lookup_path_str(cx.tcx, PathNS::Type, "core::iter::Extend").first() else {
        return false;
    };
    let Some(clone) = cx.tcx.lang_items().clone_trait() else {
        return false;
    };
    implements_trait(cx, value_ty, extend, &[ele_ty.into()])
        && implements_trait(cx, value_ty, clone, &[])
}
