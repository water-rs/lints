//! `nami`'s `Binding` def paths and method-applicability checks, shared by
//! lints that match binding reads and writes.

use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::MaybeQPath;
use clippy_utils::ty::{implements_trait, make_normalized_projection};
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{
    BinOpKind, BorrowKind, Expr, ExprKind, HirId, LangItem, Mutability, Node, Path, QPath,
};
use rustc_lint::LateContext;
use rustc_middle::ty::adjustment::{Adjust, AutoBorrow, AutoBorrowMutability};
use rustc_middle::ty::{FloatTy, IntTy, Ty, TyKind, TypeVisitableExt, TypeckResults, UintTy};
use rustc_span::symbol::Symbol;
use rustc_span::{BytePos, Pos, Span, sym};

use crate::imports::{Bare, bare_status, extern_nameable, use_insertion};

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

/// `nami`'s generic binding constructors — `binding(..)` and
/// `Binding::container(..)` — as `LateContext::get_def_path` segments.
pub(crate) const GENERIC_CTORS: &[&[&str]] = &[
    &["nami", "reactive_core", "binding", "binding"],
    &["nami", "reactive_core", "binding", "Binding", "container"],
];

/// `nami`'s `Computed<T>` — the boxed, read-only signal handle
/// `SignalExt::computed` produces. `nami::reactive_core::signal::computed`
/// is a private module, but `get_def_path` reports the defining path.
pub(crate) const COMPUTED: &[&str] = &["nami", "reactive_core", "signal", "computed", "Computed"];

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

/// The dedicated `Binding::<t>` constructor `nami` gives a primitive `T` —
/// `typed_binding_constructor`'s rewrite and the shape
/// `default_binding_constructor` leaves to it; `None` for a type `nami`
/// gives no such constructor.
pub(crate) fn dedicated_ctor(ty: Ty<'_>) -> Option<&'static str> {
    Some(match *ty.kind() {
        TyKind::Uint(UintTy::U32) => "u32",
        TyKind::Uint(UintTy::U64) => "u64",
        TyKind::Uint(UintTy::Usize) => "usize",
        TyKind::Int(IntTy::I32) => "i32",
        TyKind::Int(IntTy::I64) => "i64",
        TyKind::Int(IntTy::Isize) => "isize",
        TyKind::Float(FloatTy::F32) => "f32",
        TyKind::Float(FloatTy::F64) => "f64",
        TyKind::Bool => "bool",
        _ => return None,
    })
}

/// The crate root a suggestion can spell `Binding` through — `waterui` when
/// the linted crate can name it, `nami` when only it is a direct dependency,
/// `None` when neither is in the extern prelude (e.g. linting `nami` itself).
pub(crate) fn binding_krate(cx: &LateContext<'_>) -> Option<&'static str> {
    if extern_nameable(cx, "waterui") {
        Some("waterui")
    } else {
        extern_nameable(cx, "nami").then_some("nami")
    }
}

/// A one-argument call to a `GENERIC_CTORS` constructor whose result type is
/// `Binding<T>` with `T` fully inferred — the prologue the binding
/// constructor lints share.
pub(crate) struct GenericCtorCall<'tcx> {
    /// The callee expression (`binding` or `Binding::<T>::container`).
    pub func: &'tcx Expr<'tcx>,
    /// The constructor's single argument.
    pub arg: &'tcx Expr<'tcx>,
    /// The `Binding` ADT — the def the `Binding` spelling is checked
    /// against.
    pub adt_did: DefId,
    /// `T` in the result's `Binding<T>`.
    pub value_ty: Ty<'tcx>,
}

/// `expr` is `ctor(arg)` for `ctor` in `GENERIC_CTORS` returning
/// `Binding<T>` with no inference variables in `T` — `None` for any other
/// call shape or an expansion.
pub(crate) fn generic_ctor_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<GenericCtorCall<'tcx>> {
    if expr.span.from_expansion() {
        return None;
    }
    let ExprKind::Call(func, [arg]) = expr.kind else {
        return None;
    };
    let Res::Def(_, did) = func.res(cx) else {
        return None;
    };
    if !GENERIC_CTORS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, did, path))
    {
        return None;
    }
    let TyKind::Adt(adt, args) = *cx.typeck_results().expr_ty(expr).kind() else {
        return None;
    };
    if !crate::def_path::def_path_eq(cx, adt.did(), BINDING) {
        return None;
    }
    let value_ty = args.type_at(0);
    if value_ty.has_infer() {
        return None;
    }
    Some(GenericCtorCall {
        func,
        arg,
        adt_did: adt.did(),
        value_ty,
    })
}

/// A `let <pat>: Binding<T> = <expr>` annotation — `erase` is the
/// `: Binding<T>` span a suggestion removes once the call names `T`, and
/// `path` the annotation's `Binding` path for lints that read `T`'s spelling
/// from it.
pub(crate) struct BindingAnnotation<'tcx> {
    /// The `: Binding<T>` span to erase.
    pub erase: Span,
    /// The `Binding` path inside the annotation.
    pub path: &'tcx Path<'tcx>,
}

/// `expr` is the whole initializer of `let <pat>: Binding<T> = expr`.
/// `None` without an annotation, when the annotation is anything but a
/// `Binding` path (a type alias, a projection), or when the `let` sits in an
/// expansion — the call can sit at a macro call site while the `let` lives
/// in the `macro_rules!` body, where erasing the annotation would edit the
/// definition once per expansion.
pub(crate) fn binding_annotation<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<BindingAnnotation<'tcx>> {
    let mut hir_id = expr.hir_id;
    let local = loop {
        match cx.tcx.parent_hir_node(hir_id) {
            Node::Expr(parent) if matches!(parent.kind, ExprKind::DropTemps(_)) => {
                hir_id = parent.hir_id;
            }
            Node::LetStmt(local) => break local,
            _ => return None,
        }
    };
    if local.init.is_none_or(|init| init.hir_id != hir_id) {
        return None;
    }
    let ty = local.ty?;
    if local.span.from_expansion() || ty.span.from_expansion() {
        return None;
    }
    let rustc_hir::TyKind::Path(QPath::Resolved(_, path)) = ty.kind else {
        return None;
    };
    if !matches!(path.res, Res::Def(DefKind::Struct, did)
        if crate::def_path::def_path_eq(cx, did, BINDING))
    {
        return None;
    }
    Some(BindingAnnotation {
        erase: ty.span.with_lo(local.pat.span.hi()),
        path,
    })
}

/// How `Binding` is spelled at `at` — `Binding` when it already resolves to
/// the `Binding` ADT or can be `use`d there (pushing the `use` edit onto
/// `parts`), `<krate>::Binding` when the bare name is taken or the `use`
/// cannot be placed, downgrading `applicable` on those qualified fallbacks.
/// `None` when neither `waterui` nor `nami` is nameable. Each lint appends
/// its own suffix (`::<T>::default()`, `::<t>(..)`).
pub(crate) fn binding_name(
    cx: &LateContext<'_>,
    hir_id: HirId,
    at: BytePos,
    adt_did: DefId,
    parts: &mut Vec<(Span, String)>,
    applicable: &mut Applicability,
) -> Option<String> {
    let status = bare_status(
        cx,
        hir_id,
        at,
        Symbol::intern("Binding"),
        Namespace::TypeNS,
        Some(adt_did),
    );
    let krate = binding_krate(cx);
    match status {
        Bare::Same => Some("Binding".to_owned()),
        Bare::Free => krate.map(|krate| {
            match use_insertion(cx, hir_id, &format!("{krate}::Binding")) {
                Some((point, before, after)) => {
                    parts.push((point, format!("{before}use {krate}::Binding;{after}")));
                    "Binding".to_owned()
                }
                // The module's `use` position is in an expansion — spell
                // the qualified path instead.
                None => {
                    *applicable = Applicability::MaybeIncorrect;
                    format!("{krate}::Binding")
                }
            }
        }),
        // `Binding` is taken (or a local glob leaves it unclear) — qualify
        // instead of importing.
        Bare::Conflict | Bare::Unknown => krate.map(|krate| {
            *applicable = Applicability::MaybeIncorrect;
            format!("{krate}::Binding")
        }),
    }
}
