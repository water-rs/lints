//! `manual_signal_combinator` — `signal.map(|v| !v)`-style closures that
//! restate a named `SignalExt` combinator.

use clippy_utils::diagnostics::span_lint_and_sugg;
use clippy_utils::res::MaybeResPath;
use clippy_utils::source::snippet_with_applicability;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_ast::LitKind;
use rustc_errors::Applicability;
use rustc_hir::def_id::DefId;
use rustc_hir::{BinOpKind, Block, Expr, ExprKind, HirId, PatKind, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{AssocContainer, FloatTy, Ty, TyKind, TypeVisitableExt, TypeckResults};
use rustc_session::declare_lint_pass;
use rustc_span::{Symbol, sym};
use std::ops::ControlFlow;

use crate::carriers::is_string_ty;
use crate::def_path::def_path_eq;
use crate::param_bounds::implemented_trait_item;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `signal.map(|v| <shape>)` calls whose single-parameter closure
    /// restates a named `SignalExt` combinator:
    ///
    /// - `!v` on a `bool` output → `.not()`
    /// - `v == e`/`v > e`/`v < e`/`v >= e`/`v <= e` with `e` typed as the
    ///   signal's `Output` → `.equal_to(e)`/`.gt(e)`/`.lt(e)`/`.ge(e)`/
    ///   `.le(e)`
    /// - `-v` on a signed numeric primitive output → `.negate()`
    /// - `v.is_some()`/`v.is_none()` on an `Option` output → `.is_some()`/
    ///   `.is_none()`; `v.is_ok()`/`v.is_err()` on a `Result` output →
    ///   `.is_ok()`/`.is_err()`
    /// - `v.is_empty()`/`v.len()`/`v.contains(e)` on a `String`/`&str`/`Str`
    ///   output → `.str_is_empty()`/`.str_len()`/`.str_contains(e)`
    /// - `if v { a } else { b }` on a `bool` output → `.select(a, b)`
    ///
    /// The free operand must not mention the closure parameter, and
    /// `select`'s arms are restricted to literals and paths because both are
    /// evaluated eagerly. There is no `not_equal_to`, so `v != e` stays
    /// silent, as do `Iterator::map` and `Option::map`.
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
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let enabled = flag.not();
    /// let is_three = count.equal_to(3);
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

/// A rewrite the lint offers: `<receiver>.<name>(<args..>)`.
struct Combinator<'hir> {
    /// The `SignalExt` combinator name.
    name: &'static str,
    /// The operand expressions carried into the call, in argument order.
    args: Vec<&'hir Expr<'hir>>,
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

/// The closure's body expression: a bare expression or a block with no
/// statements and a tail — `{ !v }` matches `!v`.
fn body_expr<'hir>(mut expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(
                Block {
                    stmts: [],
                    expr: Some(tail),
                    ..
                },
                _,
            ) => tail,
            _ => return expr,
        };
    }
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

/// `ty` is a signed numeric primitive — `i8..i128`, `isize`, `f32`, `f64` —
/// the `num_traits::Signed` implementors `negate` accepts.
fn is_signed(ty: Ty<'_>) -> bool {
    matches!(
        ty.kind(),
        TyKind::Int(_) | TyKind::Float(FloatTy::F32 | FloatTy::F64)
    )
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

/// The combinator `v.<method>(..)` spells — when the method is the matching
/// inherent method and the signal's `Output` has the required shape.
fn method_combinator<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    param: HirId,
    output: Ty<'hir>,
) -> Option<Combinator<'hir>> {
    let ExprKind::MethodCall(segment, receiver, args, _) = expr.kind else {
        return None;
    };
    if !is_param(receiver, param) {
        return None;
    }
    let did = typeck.type_dependent_def_id(expr.hir_id)?;
    let self_ty = inherent_self_ty(cx, did)?;
    let name = segment.ident.name.as_str();
    Some(match name {
        "is_some" | "is_none"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Option)
                && is_adt(cx, output, sym::Option) =>
        {
            Combinator {
                name: if name == "is_some" {
                    "is_some"
                } else {
                    "is_none"
                },
                args: Vec::new(),
            }
        }
        "is_ok" | "is_err"
            if args.is_empty()
                && is_adt(cx, self_ty, sym::Result)
                && is_adt(cx, output, sym::Result) =>
        {
            Combinator {
                name: if name == "is_ok" { "is_ok" } else { "is_err" },
                args: Vec::new(),
            }
        }
        "is_empty" | "len"
            if args.is_empty() && is_string_ty(cx, self_ty) && is_string_ty(cx, output) =>
        {
            Combinator {
                name: if name == "is_empty" {
                    "str_is_empty"
                } else {
                    "str_len"
                },
                args: Vec::new(),
            }
        }
        "contains"
            if let [arg] = args
                && is_string_ty(cx, self_ty)
                && is_string_ty(cx, output)
                && free_of_param(cx, arg, param)
                && string_pattern(cx, typeck, arg) =>
        {
            Combinator {
                name: "str_contains",
                args: vec![arg],
            }
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
) -> Option<Combinator<'hir>> {
    match body.kind {
        ExprKind::Unary(UnOp::Not, inner) if is_param(inner, param) && output.is_bool() => {
            Some(Combinator {
                name: "not",
                args: Vec::new(),
            })
        }
        ExprKind::Unary(UnOp::Neg, inner) if is_param(inner, param) && is_signed(output) => {
            Some(Combinator {
                name: "negate",
                args: Vec::new(),
            })
        }
        ExprKind::Binary(op, lhs, rhs)
            if is_param(lhs, param)
                && free_of_param(cx, rhs, param)
                && typeck.expr_ty(rhs) == output
                && output_is_clone(cx, output) =>
        {
            let name = match op.node {
                BinOpKind::Eq => "equal_to",
                BinOpKind::Gt => "gt",
                BinOpKind::Lt => "lt",
                BinOpKind::Ge => "ge",
                BinOpKind::Le => "le",
                _ => return None,
            };
            Some(Combinator {
                name,
                args: vec![rhs],
            })
        }
        ExprKind::MethodCall(..) => method_combinator(cx, typeck, body, param, output),
        ExprKind::If(cond, then, Some(els)) if is_param(cond, param) && output.is_bool() => {
            let [then, els] = [then, els].map(body_expr);
            // `select` evaluates both operands eagerly: only literal and
            // path reads are safe to move out of the closure.
            let operand = |e: &'hir Expr<'hir>| {
                matches!(e.kind, ExprKind::Lit(_) | ExprKind::Path(..))
                    && free_of_param(cx, e, param)
            };
            let clone = cx.tcx.lang_items().clone_trait()?;
            (operand(then)
                && operand(els)
                && implements_trait(cx, typeck.expr_ty(body), clone, &[]))
            .then(|| Combinator {
                name: "select",
                args: vec![then, els],
            })
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
        let ExprKind::MethodCall(_, receiver, [func], _) = expr.kind else {
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
        let Some(comb) = combinator(cx, typeck, body_expr(body.value), v, output) else {
            return;
        };
        let mut applicability = Applicability::MachineApplicable;
        let receiver = snippet_with_applicability(cx, receiver.span, "..", &mut applicability);
        let args = comb
            .args
            .iter()
            .map(|arg| snippet_with_applicability(cx, arg.span, "..", &mut applicability))
            .collect::<Vec<_>>()
            .join(", ");
        span_lint_and_sugg(
            cx,
            MANUAL_SIGNAL_COMBINATOR,
            expr.span,
            MESSAGE,
            SUGGESTION_LABEL,
            format!("{receiver}.{}({args})", comb.name),
            applicability,
        );
    }
}

declare_lint_pass!(ManualSignalCombinator => [MANUAL_SIGNAL_COMBINATOR]);
