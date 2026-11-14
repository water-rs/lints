//! `manual_signal_combinator` — `signal.map(|v| !v)`-style closures that
//! restate a named `SignalExt` combinator.

use clippy_utils::res::{MaybeDef, MaybeQPath, MaybeResPath};
use clippy_utils::source::snippet_with_applicability;
use clippy_utils::sugg::Sugg;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_ast::LitKind;
use rustc_data_structures::packed::Pu128;
use rustc_errors::Applicability;
use rustc_hir::def_id::DefId;
use rustc_hir::{BinOpKind, CaptureBy, Expr, ExprKind, HirId, LangItem, PatKind, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{
    AssocContainer, FloatTy, GenericArgKind, Ty, TyKind, TypeVisitableExt, TypeckResults,
    UpvarCapture,
};
use rustc_session::declare_lint_pass;
use rustc_span::{Symbol, sym};
use std::ops::ControlFlow;

use crate::carriers::{CLONE, FROM, INTO, body_expr, is_string_ty, resolves_to};
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_sugg;
use crate::param_bounds::implemented_trait_item;
use crate::receiver::{deref_depth, owned_spelling};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `signal.map(|v| <shape>)` calls whose single-parameter closure
    /// restates a named `SignalExt` combinator:
    ///
    /// - `!v` on a `bool` output → `.not()`; `v != e` → `.equal_to(e).not()`
    /// - `v == e`/`v > e`/`v < e`/`v >= e`/`v <= e` with `e` typed as the
    ///   signal's `Output` → `.equal_to(e)`/`.gt(e)`/`.lt(e)`/`.ge(e)`/
    ///   `.le(e)`; `v == 0` on a numeric primitive → `.is_zero()`;
    ///   `v > 0`/`v < 0` on a signed numeric primitive → `.is_positive()`/
    ///   `.is_negative()`
    /// - `-v`/`v.abs()` on a signed numeric primitive output → `.negate()`/
    ///   `.abs()`
    /// - `v.is_some()`/`v.is_none()`/`v.unwrap_or(d)`/`v.unwrap_or_else(f)`/
    ///   `v.unwrap_or_default()`/`v == Some(e)`/`v == None`/`v != None`/
    ///   `v.flatten()`/`v.map(f)`/`v.and_then(f)` on an `Option` output →
    ///   the same-named combinator / `.some_equal_to(e)`/`.is_none()`/
    ///   `.is_some()`/`.map_some(f)`/`.and_then_some(f)`
    /// - `v.is_ok()`/`v.is_err()`/`v.unwrap_or(d)`/`v.unwrap_or_else(f)`/
    ///   `v.ok()`/`v.err()`/`v.map(f)`/`v.map_err(f)` on a `Result` output →
    ///   `.is_ok()`/`.is_err()`/`.unwrap_or_result(d)`/
    ///   `.unwrap_or_else_result(f)`/`.ok()`/`.err()`/`.map_ok(f)`/
    ///   `.map_err(f)`
    /// - `v.is_empty()`/`v.len()`/`v.contains(e)` on a `String`/`&str`/`Str`
    ///   output → `.str_is_empty()`/`.str_len()`/`.str_contains(e)`
    /// - `if v { a } else { b }` on a `bool` output → `.select(a, b)`;
    ///   `v.then_some(a)`/`if v { Some(a) } else { None }` → `.then_some(a)`
    /// - `v.into()`/`Into::into(v)`/`From::from(v)`/`U::from(v)` →
    ///   `.map_into()`
    /// - `v`/`v.clone()` — a needless map → `s.clone()` on a place
    ///   receiver, the receiver verbatim on a temporary
    ///
    /// `e`/`d`/`f` never mention the closure parameter. An operand the
    /// combinator stores and evaluates once at construction — `select`/
    /// `then_some`/`unwrap_or`/`some_equal_to` values and comparison
    /// operands alike — must be a literal or a path; a stored callable `f`
    /// must be a closure literal or path whose type is
    /// `Clone + 'static + Fn(..)` — the bound nami puts on it — so a
    /// closure capturing a non-`Clone` or borrowed value stays silent, as
    /// do `Iterator::map` and `Option::map`.
    ///
    /// ### Why is this bad?
    ///
    /// `Map` derives its `SignalIdentity` from the `(F, Output)` type pair:
    /// the combinators map through `fn` items, one shared type per
    /// combinator, so `flag.not()` rebuilds the same derived signal wherever
    /// it is reconstructed. A hand-written closure is its own type and reads
    /// as a bespoke transform where the operation's documented name belongs.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let enabled = flag.map(|v| !v);
    /// let is_three = count.map(|v| v == 3);
    /// let text = opt.map(|v| v.unwrap_or_default());
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let enabled = flag.not();
    /// let is_three = count.equal_to(3);
    /// let text = opt.unwrap_or_default();
    /// ```
    pub MANUAL_SIGNAL_COMBINATOR,
    style,
    "a `map` closure that restates a named `SignalExt` combinator"
}

const MESSAGE: &str = "this `map` closure is a named `SignalExt` combinator";
const SUGGESTION_LABEL: &str = "use the combinator";

/// `SignalExt::map` — the blanket `impl<C: Signal> SignalExt for C` puts it
/// on every signal, so a `MethodCall` resolving here is a signal transform.
const SIGNAL_EXT_MAP: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "map"];

/// A rewrite the lint offers for the `map` call.
enum Rewrite<'hir> {
    /// `<receiver>.<name>(<args..>)`, with `.not()` chained when `not` is
    /// set — `v != e` is `.equal_to(e).not()`.
    Named {
        /// The `SignalExt` combinator name.
        name: &'static str,
        /// The operand expressions carried into the call, in argument order.
        args: Vec<&'hir Expr<'hir>>,
        /// Append `.not()` to the combinator call.
        not: bool,
    },
    /// `|v| v`/`|v| v.clone()` — the map is a no-op and goes away.
    Identity,
    /// `|v| v.into()`, `Into::into(v)`, `From::from(v)`, `U::from(v)` →
    /// `.map_into()`.
    MapInto,
}

impl<'hir> Rewrite<'hir> {
    fn named(name: &'static str) -> Self {
        Self::Named {
            name,
            args: Vec::new(),
            not: false,
        }
    }

    fn args(name: &'static str, args: Vec<&'hir Expr<'hir>>) -> Self {
        Self::Named {
            name,
            args,
            not: false,
        }
    }
}

/// `expr` is a bare read of the local `param` — the closure's parameter.
fn is_param(expr: &Expr<'_>, param: HirId) -> bool {
    expr.res_local_id() == Some(param)
}

/// `expr` never reads `param`, nested closures included — a value the fix
/// moves outside the closure cannot depend on its binding.
fn free_of_param<'hir>(cx: &LateContext<'hir>, expr: &'hir Expr<'hir>, param: HirId) -> bool {
    for_each_expr(cx, expr, |e| {
        if is_param(e, param) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(Descend::Yes)
        }
    })
    .is_none()
}

/// The self type of the inherent impl `did` is defined in — `Option<T>` for
/// `Option::is_some`, `str` for `str::is_empty`, `Str` for `Str::len`.
fn inherent_self_ty<'tcx>(cx: &LateContext<'tcx>, did: DefId) -> Option<Ty<'tcx>> {
    let assoc = cx.tcx.opt_associated_item(did)?;
    (assoc.container == AssocContainer::InherentImpl).then(|| {
        cx.tcx
            .type_of(cx.tcx.parent(did))
            .instantiate_identity()
            .skip_norm_wip()
    })
}

/// `ty` is the `Option`/`Result` ADT (by diagnostic item).
fn is_adt(cx: &LateContext<'_>, ty: Ty<'_>, item: Symbol) -> bool {
    matches!(ty.kind(), TyKind::Adt(def, _) if cx.tcx.is_diagnostic_item(item, def.did()))
}

/// `a` and `b` are the same type modulo arguments — `inherent_self_ty`
/// returns the impl's self type with its parameters unsubstituted, so a
/// concrete `Option<i32>` output compares equal to the impl's `Option<T>`.
fn same_nominal<'tcx>(a: Ty<'tcx>, b: Ty<'tcx>) -> bool {
    match (a.ty_adt_def(), b.ty_adt_def()) {
        (Some(x), Some(y)) => x.did() == y.did(),
        _ => a == b,
    }
}

/// `Option<T>` → `T`.
fn option_inner<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> Option<Ty<'tcx>> {
    let TyKind::Adt(def, args) = ty.kind() else {
        return None;
    };
    if !cx.tcx.is_diagnostic_item(sym::Option, def.did()) {
        return None;
    }
    args[0].as_type()
}

/// `Result<T, E>` → `(T, E)`.
fn result_args<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> Option<(Ty<'tcx>, Ty<'tcx>)> {
    let TyKind::Adt(def, args) = ty.kind() else {
        return None;
    };
    if !cx.tcx.is_diagnostic_item(sym::Result, def.did()) {
        return None;
    }
    Some((args[0].as_type()?, args[1].as_type()?))
}

/// `ty` is a numeric primitive — the `num_traits::Zero` implementors
/// `is_zero` accepts.
fn is_numeric(ty: Ty<'_>) -> bool {
    matches!(
        ty.kind(),
        TyKind::Int(_) | TyKind::Uint(_) | TyKind::Float(_)
    )
}

/// `ty` is a signed numeric primitive — `i8..i128`, `isize`, `f32`, `f64` —
/// the `num_traits::Signed` implementors `negate`/`abs`/`is_positive`/
/// `is_negative` accept.
fn is_signed(ty: Ty<'_>) -> bool {
    matches!(
        ty.kind(),
        TyKind::Int(_) | TyKind::Float(FloatTy::F32 | FloatTy::F64)
    )
}

/// `expr` is the literal `0`/`0.0` — the comparison operand `is_zero`,
/// `is_positive`, and `is_negative` restate.
fn zero_literal(expr: &Expr<'_>) -> bool {
    match expr.kind {
        ExprKind::Lit(lit) => match lit.node {
            LitKind::Int(Pu128(0), _) => true,
            LitKind::Float(sym, _) => sym.as_str().replace('_', "").parse::<f64>() == Ok(0.0),
            _ => false,
        },
        _ => false,
    }
}

/// `e` is a `&str`-typed expression (a string literal lands here too) or a
/// `String` — the `str_contains(pattern: impl Into<String>)` argument.
fn string_pattern<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> bool {
    if let ExprKind::Lit(lit) = expr.kind
        && matches!(lit.node, LitKind::Str(..))
    {
        return true;
    }
    match typeck.expr_ty(expr).kind() {
        TyKind::Ref(_, ty, _) => ty.is_str(),
        TyKind::Adt(def, _) => cx.tcx.is_diagnostic_item(sym::String, def.did()),
        _ => false,
    }
}

/// `ty` is `Clone` with no inference or generic parameters left — the
/// combinator stores and clones it.
fn cloneable_concrete<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    if ty.has_infer() || ty.has_non_region_param() {
        return false;
    }
    cx.tcx
        .lang_items()
        .clone_trait()
        .is_some_and(|clone| implements_trait(cx, ty, clone, &[]))
}

/// `ty` may be stored in a combinator and replayed — `Clone + 'static` —
/// with no inference or generic parameters left. `is_literal` for a
/// literal's type, which is `'static` by construction (`&'static str`,
/// numbers, …); otherwise every region must be `ReStatic` — typeck erases
/// local borrows to `ReErased`, which fails `is_static` and stays silent.
fn stored_ty<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>, is_literal: bool) -> bool {
    cloneable_concrete(cx, ty)
        && (is_literal
            || ty
                .walk()
                .all(|arg| !matches!(arg.kind(), GenericArgKind::Lifetime(re) if !re.is_static())))
}

/// An operand a combinator stores and evaluates once at construction —
/// instead of per `map` call — must be a literal or a path that never reads
/// `param`, of a `stored_ty`.
fn stored_operand<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    param: HirId,
) -> bool {
    matches!(expr.kind, ExprKind::Lit(_) | ExprKind::Path(..))
        && free_of_param(cx, expr, param)
        && stored_ty(
            cx,
            typeck.expr_ty(expr),
            matches!(expr.kind, ExprKind::Lit(_)),
        )
}

/// `f` moves into the combinator as the stored callable — nami's bound is
/// `'static + Clone + Fn(inputs)`. A closure literal or a path qualifies:
/// a closure's captures must each move a `Clone + 'static` value in (a
/// `&`/`&mut` capture borrows the frame, which is never `'static`, and
/// `&mut` is never `Clone`), and the callable's own type must satisfy the
/// trait bounds.
fn movable_fn<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    f: &'hir Expr<'hir>,
    param: HirId,
    inputs: &[Ty<'hir>],
) -> bool {
    let mut f = f;
    while let ExprKind::DropTemps(inner) = f.kind {
        f = inner;
    }
    if !free_of_param(cx, f, param) {
        return false;
    }
    let f_ty = typeck.expr_ty(f);
    let (Some(fn_trait), Some(clone), Some(copy)) = (
        cx.tcx.lang_items().fn_trait(),
        cx.tcx.lang_items().clone_trait(),
        cx.tcx.lang_items().copy_trait(),
    ) else {
        return false;
    };
    match f.kind {
        ExprKind::Closure(closure) => {
            let TyKind::Closure(_, closure_args) = *f_ty.kind() else {
                return false;
            };
            // The callable's argument tuple must match what the combinator
            // passes in. Note `closure_args.kind()` is the kind inferred
            // for this use — `FnOnce` wherever the call site only requires
            // it — so `Fn` capability is derived from the captures below
            // instead.
            if *closure_args.as_closure().sig().skip_binder().inputs()
                != [Ty::new_tup(cx.tcx, inputs)]
            {
                return false;
            }
            let is_move = matches!(closure.capture_clause, CaptureBy::Value { .. });
            typeck
                .closure_min_captures_flattened(closure.def_id)
                .all(|captured| {
                    let ty = captured.place.ty();
                    match captured.info.capture_kind {
                        // A `&`/`&mut`/`use` capture borrows the frame —
                        // never `'static` (and `&mut` is never `Clone`).
                        UpvarCapture::ByRef(..) | UpvarCapture::ByUse => false,
                        // In a `move` closure every capture is by value;
                        // the stored callable stays `Fn` only when the
                        // body cannot move the value back out, which a
                        // `Copy` type guarantees. In a plain closure a
                        // by-value capture means the body consumed the
                        // place — `FnOnce` at best.
                        UpvarCapture::ByValue => {
                            is_move
                                && implements_trait(cx, ty, copy, &[])
                                && stored_ty(cx, ty, false)
                        }
                    }
                })
                && implements_trait(cx, f_ty, clone, &[])
        }
        // A path's own type must carry the bounds — the callable may be a
        // `fn` item, a `fn` pointer, or a stored `impl Fn` value.
        ExprKind::Path(..) => {
            !f_ty.has_infer()
                && !f_ty.has_non_region_param()
                && f_ty.walk().all(
                    |arg| !matches!(arg.kind(), GenericArgKind::Lifetime(re) if !re.is_static()),
                )
                && implements_trait(cx, f_ty, clone, &[])
                && implements_trait(cx, f_ty, fn_trait, &[Ty::new_tup(cx.tcx, inputs).into()])
        }
        _ => false,
    }
}

/// `expr` is `Some(inner)` — an `Option::Some` constructor call.
fn some_arg<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    let ExprKind::Call(func, [inner]) = expr.kind else {
        return None;
    };
    func.res(typeck)
        .ctor_parent(cx)
        .is_lang_item(cx, LangItem::OptionSome)
        .then_some(inner)
}

/// `expr` is `None`.
fn none_expr(cx: &LateContext<'_>, typeck: &TypeckResults<'_>, expr: &Expr<'_>) -> bool {
    expr.res(typeck)
        .ctor_parent(cx)
        .is_lang_item(cx, LangItem::OptionNone)
}

/// The combinator `v <op> e` spells — `equal_to`/`gt`/`lt`/`ge`/`le`, the
/// `v != e` negation, and the zero/signed shortcuts `is_zero`,
/// `is_positive`, `is_negative`. The comparison operand is stored and
/// evaluated once at construction — the same `stored_operand` rule as
/// `select`/`unwrap_or` applies.
fn binary_combinator<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    op: BinOpKind,
    rhs: &'hir Expr<'hir>,
    param: HirId,
    output: Ty<'hir>,
) -> Option<Rewrite<'hir>> {
    Some(match op {
        BinOpKind::Eq if zero_literal(rhs) && is_numeric(output) => Rewrite::named("is_zero"),
        BinOpKind::Eq => {
            if is_adt(cx, output, sym::Option) {
                // `v == None` spells `.is_none()`.
                if none_expr(cx, typeck, rhs) {
                    return Some(Rewrite::named("is_none"));
                }
                // `v == Some(e)` is `some_equal_to(e)` — the stored operand
                // is the inner `e`; a `Some(_)` that cannot be stored stays
                // silent rather than degrade to `equal_to(Some(..))`.
                if let Some(inner) = some_arg(cx, typeck, rhs) {
                    return stored_operand(cx, typeck, inner, param)
                        .then(|| Rewrite::args("some_equal_to", vec![inner]));
                }
            }
            if stored_operand(cx, typeck, rhs, param) {
                Rewrite::args("equal_to", vec![rhs])
            } else {
                return None;
            }
        }
        BinOpKind::Ne => {
            // `v != None` spells `.is_some()`.
            if is_adt(cx, output, sym::Option) && none_expr(cx, typeck, rhs) {
                return Some(Rewrite::named("is_some"));
            }
            if stored_operand(cx, typeck, rhs, param) {
                Rewrite::Named {
                    name: "equal_to",
                    args: vec![rhs],
                    not: true,
                }
            } else {
                return None;
            }
        }
        BinOpKind::Gt if is_signed(output) && zero_literal(rhs) => Rewrite::named("is_positive"),
        BinOpKind::Lt if is_signed(output) && zero_literal(rhs) => Rewrite::named("is_negative"),
        BinOpKind::Gt if stored_operand(cx, typeck, rhs, param) => Rewrite::args("gt", vec![rhs]),
        BinOpKind::Lt if stored_operand(cx, typeck, rhs, param) => Rewrite::args("lt", vec![rhs]),
        BinOpKind::Ge if stored_operand(cx, typeck, rhs, param) => Rewrite::args("ge", vec![rhs]),
        BinOpKind::Le if stored_operand(cx, typeck, rhs, param) => Rewrite::args("le", vec![rhs]),
        _ => return None,
    })
}

/// The combinator `if v { then } else { els }` spells on a `bool` output —
/// `then_some(a)` for `Some(a)`/`None` arms, `select(a, b)` otherwise. Both
/// combinators evaluate their operands eagerly, so the same stored-operand
/// rules apply.
fn if_combinator<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    then: &'hir Expr<'hir>,
    els: &'hir Expr<'hir>,
    param: HirId,
) -> Option<Rewrite<'hir>> {
    let [then, els] = [then, els].map(body_expr);
    if let Some(a) = some_arg(cx, typeck, then)
        && none_expr(cx, typeck, els)
        && stored_operand(cx, typeck, a, param)
    {
        return Some(Rewrite::args("then_some", vec![a]));
    }
    (stored_operand(cx, typeck, then, param) && stored_operand(cx, typeck, els, param))
        .then(|| Rewrite::args("select", vec![then, els]))
}

/// The combinator `body` spells as a free-function call — `Into::into(v)`,
/// `From::from(v)`, `U::from(v)` are `.map_into()`; `Clone::clone(&v)` is
/// the needless map.
fn call_combinator<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    body: &'hir Expr<'hir>,
    param: HirId,
) -> Option<Rewrite<'hir>> {
    let ExprKind::Call(_, [arg]) = body.kind else {
        return None;
    };
    if resolves_to(cx, typeck, body, &[INTO, FROM]) {
        return is_param(arg, param).then_some(Rewrite::MapInto);
    }
    if resolves_to(cx, typeck, body, &[CLONE]) {
        // `Clone::clone` takes `&self` — the spelled argument carries the
        // autoref.
        let mut arg = arg;
        while let ExprKind::DropTemps(inner) | ExprKind::AddrOf(.., inner) = arg.kind {
            arg = inner;
        }
        return is_param(arg, param).then_some(Rewrite::Identity);
    }
    None
}

/// The combinator `v.<method>(..)` spells — when the method is the matching
/// inherent method and the signal's `Output` has the required shape.
fn method_combinator<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    param: HirId,
    output: Ty<'hir>,
) -> Option<Rewrite<'hir>> {
    let ExprKind::MethodCall(segment, receiver, args, _) = expr.kind else {
        return None;
    };
    if !is_param(receiver, param) {
        return None;
    }
    let did = typeck.type_dependent_def_id(expr.hir_id)?;
    let name = segment.ident.name.as_str();
    // `clone`/`into` resolve to trait items — `inherent_self_ty` below
    // covers only inherent methods, so handle these first.
    match name {
        "clone" if args.is_empty() && typeck.expr_ty(expr) == output => {
            if def_path_eq(cx, implemented_trait_item(cx.tcx, did), CLONE)
                || inherent_self_ty(cx, did).is_some_and(|self_ty| same_nominal(self_ty, output))
            {
                return Some(Rewrite::Identity);
            }
        }
        "into" if args.is_empty() && def_path_eq(cx, implemented_trait_item(cx.tcx, did), INTO) => {
            return Some(Rewrite::MapInto);
        }
        _ => {}
    }
    let self_ty = inherent_self_ty(cx, did)?;
    Some(match name {
        "is_some" | "is_none"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Option)
                && is_adt(cx, output, sym::Option) =>
        {
            Rewrite::named(if name == "is_some" {
                "is_some"
            } else {
                "is_none"
            })
        }
        "is_ok" | "is_err"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Result)
                && is_adt(cx, output, sym::Result) =>
        {
            Rewrite::named(if name == "is_ok" { "is_ok" } else { "is_err" })
        }
        "is_empty" | "len"
            if args.is_empty() && is_string_ty(cx, self_ty) && is_string_ty(cx, output) =>
        {
            Rewrite::named(if name == "is_empty" {
                "str_is_empty"
            } else {
                "str_len"
            })
        }
        "contains"
            if let [arg] = args
                && is_string_ty(cx, self_ty)
                && is_string_ty(cx, output)
                && free_of_param(cx, arg, param)
                && string_pattern(cx, typeck, arg) =>
        {
            Rewrite::args("str_contains", vec![arg])
        }
        "abs" if args.is_empty() && is_signed(self_ty) && same_nominal(self_ty, output) => {
            Rewrite::named("abs")
        }
        "unwrap_or"
            if let [d] = args
                && is_adt(cx, self_ty, sym::Option)
                && is_adt(cx, output, sym::Option)
                && stored_operand(cx, typeck, d, param) =>
        {
            Rewrite::args("unwrap_or", vec![d])
        }
        "unwrap_or"
            if let [d] = args
                && is_adt(cx, self_ty, sym::Result)
                && is_adt(cx, output, sym::Result)
                && stored_operand(cx, typeck, d, param) =>
        {
            Rewrite::args("unwrap_or_result", vec![d])
        }
        "unwrap_or_else"
            if let [f] = args
                && is_adt(cx, self_ty, sym::Option)
                && is_adt(cx, output, sym::Option)
                && movable_fn(cx, typeck, f, param, &[]) =>
        {
            Rewrite::args("unwrap_or_else", vec![f])
        }
        "unwrap_or_else"
            if let [f] = args
                && is_adt(cx, self_ty, sym::Result)
                && let Some((_, e)) = result_args(cx, output)
                && movable_fn(cx, typeck, f, param, &[e]) =>
        {
            Rewrite::args("unwrap_or_else_result", vec![f])
        }
        // `Option::unwrap_or_default` compiling already proves `T:
        // Default`; the combinator adds only `T: 'static`, which the
        // `map` output carries.
        "unwrap_or_default"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Option)
                && option_inner(cx, output).is_some() =>
        {
            Rewrite::named("unwrap_or_default")
        }
        "flatten"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Option)
                && option_inner(cx, output).is_some_and(|inner| is_adt(cx, inner, sym::Option)) =>
        {
            Rewrite::named("flatten")
        }
        "ok" if args.is_empty()
            && is_adt(cx, self_ty, sym::Result)
            && is_adt(cx, output, sym::Result) =>
        {
            Rewrite::named("ok")
        }
        "err"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Result)
                && is_adt(cx, output, sym::Result) =>
        {
            Rewrite::named("err")
        }
        "map"
            if let [f] = args
                && is_adt(cx, self_ty, sym::Option)
                && let Some(t) = option_inner(cx, output)
                && movable_fn(cx, typeck, f, param, &[t]) =>
        {
            Rewrite::args("map_some", vec![f])
        }
        "map"
            if let [f] = args
                && is_adt(cx, self_ty, sym::Result)
                && let Some((t, _)) = result_args(cx, output)
                && movable_fn(cx, typeck, f, param, &[t]) =>
        {
            Rewrite::args("map_ok", vec![f])
        }
        "and_then"
            if let [f] = args
                && is_adt(cx, self_ty, sym::Option)
                && let Some(t) = option_inner(cx, output)
                && movable_fn(cx, typeck, f, param, &[t]) =>
        {
            Rewrite::args("and_then_some", vec![f])
        }
        "map_err"
            if let [f] = args
                && is_adt(cx, self_ty, sym::Result)
                && let Some((_, e)) = result_args(cx, output)
                && movable_fn(cx, typeck, f, param, &[e]) =>
        {
            Rewrite::args("map_err", vec![f])
        }
        "then_some"
            if let [a] = args
                && self_ty.is_bool()
                && output.is_bool()
                && stored_operand(cx, typeck, a, param) =>
        {
            Rewrite::args("then_some", vec![a])
        }
        _ => return None,
    })
}

/// The combinator `body` spells for a `map` over parameter `param` with the
/// signal's `Output` type `output`, or `None` when the body is not one of
/// the named shapes.
fn combinator<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    body: &'hir Expr<'hir>,
    param: HirId,
    output: Ty<'hir>,
) -> Option<Rewrite<'hir>> {
    if is_param(body, param) {
        return Some(Rewrite::Identity);
    }
    match body.kind {
        ExprKind::Unary(UnOp::Not, inner) if is_param(inner, param) && output.is_bool() => {
            Some(Rewrite::named("not"))
        }
        ExprKind::Unary(UnOp::Neg, inner) if is_param(inner, param) && is_signed(output) => {
            Some(Rewrite::named("negate"))
        }
        ExprKind::Binary(op, lhs, rhs)
            if is_param(lhs, param)
                && free_of_param(cx, rhs, param)
                && typeck.expr_ty(rhs) == output
                && output_is_clone(cx, output) =>
        {
            binary_combinator(cx, typeck, op.node, rhs, param, output)
        }
        ExprKind::MethodCall(..) => method_combinator(cx, typeck, body, param, output),
        ExprKind::Call(..) => call_combinator(cx, typeck, body, param),
        ExprKind::If(cond, then, Some(els)) if is_param(cond, param) && output.is_bool() => {
            if_combinator(cx, typeck, then, els, param)
        }
        _ => None,
    }
}

/// `equal_to`/`gt`/`lt`/`ge`/`le` all require `Self::Output: Clone`; `v == e`
/// proves only `Output: PartialEq`.
fn output_is_clone<'tcx>(cx: &LateContext<'tcx>, output: Ty<'tcx>) -> bool {
    !output.has_infer()
        && cx
            .tcx
            .lang_items()
            .clone_trait()
            .is_some_and(|clone| implements_trait(cx, output, clone, &[]))
}

impl<'tcx> LateLintPass<'tcx> for ManualSignalCombinator {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(_, mut receiver, [func], _) = expr.kind else {
            return;
        };
        let typeck = cx.typeck_results();
        if !typeck
            .type_dependent_def_id(expr.hir_id)
            .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), SIGNAL_EXT_MAP))
        {
            return;
        }
        let mut func = func;
        while let ExprKind::DropTemps(inner) = func.kind {
            func = inner;
        }
        let ExprKind::Closure(closure) = func.kind else {
            return;
        };
        let body = cx.tcx.hir_body(closure.body);
        let [param] = body.params else {
            return;
        };
        let PatKind::Binding(_, v, _, None) = param.pat.kind else {
            return;
        };
        // The closure is a nested body; its expressions are typed through
        // `typeck_body`, not the enclosing body's table.
        let typeck = cx.tcx.typeck_body(closure.body);
        let output = typeck.pat_ty(param.pat);
        let Some(rewrite) = combinator(cx, typeck, body_expr(body.value), v, output) else {
            return;
        };
        while let ExprKind::DropTemps(inner) = receiver.kind {
            receiver = inner;
        }
        let mut applicability = Applicability::MachineApplicable;
        let receiver_sugg = Sugg::hir_with_applicability(cx, receiver, "..", &mut applicability);
        let sugg = match rewrite {
            Rewrite::Named { name, args, not } => {
                let args = args
                    .iter()
                    .map(|arg| snippet_with_applicability(cx, arg.span, "..", &mut applicability))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{}.{name}({args}){}",
                    receiver_sugg.maybe_paren(),
                    if not { ".not()" } else { "" }
                )
            }
            Rewrite::Identity => {
                // Deleting the `map` hands the receiver to the consumer by
                // value: a place clones, a temporary moves. A place whose
                // type is not `Clone` has no safe spelling — stay silent.
                let clones = deref_depth(cx.typeck_results(), receiver) > 0
                    || receiver.is_syntactic_place_expr();
                // The cloned value is the signal itself — the receiver type
                // after autoderef/autoref, minus the borrow.
                let owned_ty = cx.typeck_results().expr_ty_adjusted(receiver).peel_refs();
                let clone = cx.tcx.lang_items().clone_trait();
                if clones
                    && !clone.is_some_and(|clone| {
                        !owned_ty.has_infer() && implements_trait(cx, owned_ty, clone, &[])
                    })
                {
                    return;
                }
                owned_spelling(cx.typeck_results(), receiver, receiver_sugg)
            }
            Rewrite::MapInto => {
                // `expr`'s type is `Map<_, _, U>` with `U` the conversion
                // target already inferred — spelling it compiles in
                // positions where a bare `map_into()` leaves `U`
                // ambiguous. A non-primitive `U` may not be nameable at
                // the call site as displayed, so only a primitive spelling
                // is machine-applicable.
                let target = match cx.typeck_results().expr_ty(expr).kind() {
                    TyKind::Adt(_, args) => args.iter().filter_map(|arg| arg.as_type()).next_back(),
                    _ => None,
                };
                match target {
                    Some(u) if !u.has_infer() => {
                        if !u.is_primitive() {
                            applicability = Applicability::MaybeIncorrect;
                        }
                        format!("{}.map_into::<{u}>()", receiver_sugg.maybe_paren())
                    }
                    _ => {
                        applicability = Applicability::MaybeIncorrect;
                        format!("{}.map_into()", receiver_sugg.maybe_paren())
                    }
                }
            }
        };
        span_lint_and_sugg(
            cx,
            MANUAL_SIGNAL_COMBINATOR,
            expr.span,
            MESSAGE,
            SUGGESTION_LABEL,
            sugg,
            applicability,
        );
    }
}

declare_lint_pass!(ManualSignalCombinator => [MANUAL_SIGNAL_COMBINATOR]);
