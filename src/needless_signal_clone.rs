use clippy_utils::eq_expr_value;
use clippy_utils::source::SpanRangeExt;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind, Mutability};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{AssocContainer, TyKind};
use rustc_session::declare_lint_pass;
use rustc_span::{BytePos, Pos, Span, SyntaxContext, def_id::DefId};
use std::ops::ControlFlow;

use crate::binding::{BINDING, COMPUTED};
use crate::carriers::CLONE;
use crate::def_path::def_path_eq;
use crate::diagnostics::{span_lint, span_lint_and_then};
use crate::param_bounds::{call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `x.clone().<method>(..)` where `<method>` is a `SignalExt`
    /// combinator (`map`, `zip`, `select`, `not`, `equal_to`, `computed`,
    /// `cached`, `with`, …) or an inherent `Binding`/`Computed`/`List`
    /// method whose `self` parameter is `&self`.
    ///
    /// ### Why is this bad?
    ///
    /// Every one of those methods borrows the receiver and clones the cheap
    /// handle internally, so the `.clone()` in front is a second clone that
    /// changes nothing about ownership — the receiver is usable afterwards
    /// without it. Methods that consume `self` (the operator overloads, such
    /// as `Not::not` on `Binding<bool>`) still need it and stay silent.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// unavailable.clone().select(0.45, 1.0)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// unavailable.select(0.45, 1.0)
    /// ```
    pub NEEDLESS_SIGNAL_CLONE,
    style,
    "a `.clone()` on the receiver of a `&self` signal method"
}

declare_lint_pass!(NeedlessSignalClone => [NEEDLESS_SIGNAL_CLONE]);

/// `SignalExt` — the blanket `impl<C: Signal> SignalExt for C` puts every
/// combinator on every signal, and all of them borrow `&self`.
const SIGNAL_EXT: &[&str] = &["nami", "reactive_core", "ext", "SignalExt"];

/// The signal-handle types whose inherent `&self` methods qualify —
/// `Binding`'s derived-binding methods (`filter`, `clamp`, `unwrap_or*`,
/// `then_some`, `mapping`, `negate`, `reverse`, …) and `List`'s mutators
/// all borrow the handle. `Computed` has no inherent `&self` methods today;
/// the entry keeps the detection honest if it grows one.
const INHERENT_HOMES: &[&[&str]] = &[BINDING, COMPUTED, &["nami", "data", "collection", "List"]];

/// `did`'s first parameter is `&self` — not `self`, `&mut self`, or absent.
fn takes_ref_self(cx: &LateContext<'_>, did: DefId) -> bool {
    cx.tcx
        .fn_sig(did)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder()
        .inputs()
        .first()
        .is_some_and(|input| matches!(input.kind(), TyKind::Ref(_, _, Mutability::Not)))
}

/// Whether `did` — a resolved method — is a `SignalExt` combinator or an
/// inherent `Binding`/`Computed`/`List` method borrowing `&self`. The `&self`
/// requirement is what keeps the operator overloads silent: `b.clone().not()`
/// on a `Binding<bool>` resolves to `Not::not`, which consumes the clone.
fn borrowed_self_signal_method(cx: &LateContext<'_>, did: DefId) -> bool {
    if !takes_ref_self(cx, did) {
        return false;
    }
    let item = implemented_trait_item(cx.tcx, did);
    let Some(assoc) = cx.tcx.opt_associated_item(item) else {
        return false;
    };
    match assoc.container {
        AssocContainer::Trait => def_path_eq(cx, cx.tcx.parent(item), SIGNAL_EXT),
        AssocContainer::InherentImpl => {
            let self_ty = cx
                .tcx
                .type_of(cx.tcx.parent(item))
                .instantiate_identity()
                .skip_norm_wip();
            match self_ty.kind() {
                TyKind::Adt(adt, _) => INHERENT_HOMES
                    .iter()
                    .any(|path| def_path_eq(cx, adt.did(), path)),
                _ => false,
            }
        }
        AssocContainer::TraitImpl(_) => false,
    }
}

/// Whether `args` uses `place` — an argument that is, moves, borrows, or
/// captures the cloned place (closure bodies included). `x.clone()` ends its
/// `&x` borrow before the arguments evaluate, while `x.m(..)` keeps it live
/// across them, so there the clone is load-bearing: `x.clone().select(x, ..)`
/// compiles where `x.select(x, ..)` cannot.
fn args_use_place<'tcx>(
    cx: &LateContext<'tcx>,
    ctxt: SyntaxContext,
    args: &'tcx [Expr<'tcx>],
    place: &Expr<'_>,
) -> bool {
    args.iter().any(|arg| {
        for_each_expr(cx, arg, |e| {
            if eq_expr_value(cx, ctxt, e, place) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(Descend::Yes)
            }
        })
        .is_some()
    })
}

impl<'tcx> LateLintPass<'tcx> for NeedlessSignalClone {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(segment, receiver, args, _) = expr.kind else {
            return;
        };
        // An `x.clone()` produced by a macro expansion would point the
        // diagnostic — and the edit — at the macro's body, not the callsite.
        if receiver.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        if !typeck
            .type_dependent_def_id(expr.hir_id)
            .is_some_and(|did| borrowed_self_signal_method(cx, did))
        {
            return;
        }
        // The receiver must be `x.clone()` resolving to `Clone::clone`; `x`
        // may be any expression — a method-chain temporary can be used
        // directly too.
        let ExprKind::MethodCall(_, inner, [], _) = receiver.kind else {
            return;
        };
        if !call_def_id(typeck, receiver)
            .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), CLONE))
        {
            return;
        }
        if args_use_place(cx, expr.span.ctxt(), args, inner) {
            return;
        }
        report(
            cx,
            receiver,
            inner,
            format!(
                "this `clone` is redundant: `{}` takes `&self`",
                segment.ident.name
            ),
        );
    }
}

/// Flags the `x.clone()` receiver.
fn report(cx: &LateContext<'_>, clone_call: &Expr<'_>, inner: &Expr<'_>, msg: String) {
    match clone_suffix(cx, clone_call, inner) {
        Some(sugg_span) => {
            span_lint_and_then(cx, NEEDLESS_SIGNAL_CLONE, clone_call.span, msg, |diag| {
                diag.span_suggestion(
                    sugg_span,
                    "drop the `.clone()`",
                    "",
                    Applicability::MachineApplicable,
                );
            });
        }
        None => span_lint(cx, NEEDLESS_SIGNAL_CLONE, clone_call.span, msg),
    }
}

/// The span of exactly `.clone()` inside `x.clone()` — the text between
/// `x`'s end and the clone call's end. `Some` only when that text is
/// `.clone()`, optionally trailed by the `)`s of enclosing parens, which
/// HIR lowering folds into the call's span — so `(x.clone()).map(..)`
/// deletes just `.clone()` and `(x)` survives. Anything else (whitespace,
/// a comment inside the call) gets the diagnostic without a suggestion.
fn clone_suffix(cx: &LateContext<'_>, clone_call: &Expr<'_>, inner: &Expr<'_>) -> Option<Span> {
    let snip = clone_call.span.get_source_text(cx)?;
    if inner.span.hi() < clone_call.span.lo() || inner.span.hi() > clone_call.span.hi() {
        return None;
    }
    let offset = (inner.span.hi() - clone_call.span.lo()).to_usize();
    let rest = snip.get(offset..)?.strip_prefix(".clone()")?;
    if !rest.bytes().all(|b| b == b')') {
        return None;
    }
    Some(
        clone_call
            .span
            .with_lo(inner.span.hi())
            .with_hi(inner.span.hi() + BytePos(u32::try_from(".clone()".len()).unwrap())),
    )
}
