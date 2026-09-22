use std::ops::ControlFlow;

use clippy_utils::eq_expr_value;
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::{BinOpKind, Expr, ExprKind, LangItem, Node, QPath, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{TypeVisitableExt, TypeckResults};
use rustc_session::declare_lint_pass;
use rustc_span::{Pos, SyntaxContext};

use crate::binding::{
    BINDING_SET, binding_value_ty, coerced_operand, extend_accepts, named_op_assign,
};
use crate::carriers::strip_wraps;
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};
use crate::snapshot_get::{get_receiver, is_snapshot_get_in};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `Binding::set` and `CustomBinding::set` calls whose argument reads
    /// the same binding back through `.get()` —
    /// `count.set(count.get() + 1)` — including reads nested inside closures,
    /// conditionals, and method calls in the argument.
    ///
    /// ### Why is this bad?
    ///
    /// The `.get()` snapshots the value once, outside any subscription; by the
    /// time `set` runs the read may already be stale, and the write notifies
    /// watchers a second time. `Binding` names the in-place mutation:
    /// `count.add_assign(1)` (and the other `<op>_assign` methods, for
    /// `T: Op<Output = T> + Clone`), `name.append("!")` (for
    /// `T: Extend<E>`), `flag.toggle()`. Where none of those fits, the
    /// deref-assign `*count.get_mut() op= x` or `count.with_mut(|v| ..)`
    /// still keeps the read and the write in one step.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// count.set(count.get() + 1);
    /// name.set(name.get() + "!");
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// count.add_assign(1);
    /// name.append("!");
    /// ```
    pub SET_WITH_OWN_GET,
    style,
    "a `set` that recomputes the binding from its own `get()` snapshot"
}

declare_lint_pass!(SetWithOwnGet => [SET_WITH_OWN_GET]);

/// `set` methods the lint recognizes: `Binding`'s inherent `set` and the
/// `CustomBinding` trait's `set`, which trait-impl callees normalize onto
/// through [`implemented_trait_item`].
const SET_PATHS: &[&[&str]] = &[BINDING_SET, &["nami_core", "CustomBinding", "set"]];

const MESSAGE: &str = "this `set` recomputes the binding from its own `get()` snapshot";
const GET_LABEL: &str = "the value read here is stale by the time `set` runs";
const HELP: &str = "mutate in place with `b.<op>_assign(x)`, `b.append(x)`, `b.toggle()`, `*b.get_mut() op= x`, or `b.with_mut(|v| ..)`";

/// Whether `expr` is a snapshot `.get()` whose receiver is the same binding
/// as `binding` — the `set` receiver, compared with carriers stripped on
/// both sides so `&b`, `b.clone()`, and `state.count` match their use in the
/// `get`.
fn is_own_get<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    ctxt: SyntaxContext,
    binding: &'tcx Expr<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> bool {
    is_snapshot_get_in(cx, typeck, expr)
        && get_receiver(expr).is_some_and(|receiver| {
            eq_expr_value(cx, ctxt, strip_wraps(cx, typeck, receiver), binding)
        })
}

/// Every snapshot `.get()` inside `expr` — nested closures included — whose
/// receiver is `binding`. A `.get()` a macro wrote (`text!`'s subscription
/// plumbing) keeps its expansion span and is not the user's read.
fn own_gets<'tcx>(
    cx: &LateContext<'tcx>,
    ctxt: SyntaxContext,
    binding: &'tcx Expr<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Vec<&'tcx Expr<'tcx>> {
    let mut gets = Vec::new();
    for_each_expr(cx, expr, |expr| -> ControlFlow<(), Descend> {
        if !expr.span.from_expansion()
            && is_own_get(
                cx,
                cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(expr.hir_id)),
                ctxt,
                binding,
                expr,
            )
        {
            gets.push(expr);
        }
        ControlFlow::Continue(Descend::Yes)
    });
    gets
}

/// The compound-assignment lang item behind a binary operator — the trait
/// `*b.get_mut() op= rhs` needs `T` to implement.
fn op_assign_lang_item(op: BinOpKind) -> Option<LangItem> {
    Some(match op {
        BinOpKind::Add => LangItem::AddAssign,
        BinOpKind::Sub => LangItem::SubAssign,
        BinOpKind::Mul => LangItem::MulAssign,
        BinOpKind::Div => LangItem::DivAssign,
        BinOpKind::Rem => LangItem::RemAssign,
        BinOpKind::BitAnd => LangItem::BitAndAssign,
        BinOpKind::BitOr => LangItem::BitOrAssign,
        BinOpKind::BitXor => LangItem::BitXorAssign,
        BinOpKind::Shl => LangItem::ShlAssign,
        BinOpKind::Shr => LangItem::ShrAssign,
        _ => return None,
    })
}

/// Whether `expr` references a local binding named `name` — a `with_mut`
/// closure parameter would shadow it, so the suggestion picks another one.
fn references_local<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>, name: &str) -> bool {
    for_each_expr(cx, expr, |expr| {
        if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
            && matches!(path.res, Res::Local(_))
            && path
                .segments
                .last()
                .is_some_and(|seg| seg.ident.name.as_str() == name)
        {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(Descend::Yes)
        }
    })
    .is_some()
}

/// Whether `expr` is the operand of a postfix operation — a method-call
/// receiver, a field access, an index, or a `?` — where a spliced `*v` would
/// bind looser than the operation (`*v.max(0)` is `*(v.max(0))`) and needs
/// parentheses.
fn in_postfix_position(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let Node::Expr(parent) = cx.tcx.parent_hir_node(expr.hir_id) else {
        return false;
    };
    match parent.kind {
        ExprKind::MethodCall(_, receiver, ..) => receiver.hir_id == expr.hir_id,
        ExprKind::Field(base, _) | ExprKind::Index(base, ..) => base.hir_id == expr.hir_id,
        ExprKind::Match(scrutinee, _, rustc_hir::MatchSource::TryDesugar(_)) => {
            scrutinee.hir_id == expr.hir_id
        }
        _ => false,
    }
}

/// `(suggestion, label, applicability)` for the whole `set` call, or `None`
/// when the receiver is not a `Binding` or the source text cannot be read —
/// the diagnostic then falls back to `HELP`.
fn suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    ctxt: SyntaxContext,
    receiver: &'tcx Expr<'tcx>,
    binding: &'tcx Expr<'tcx>,
    arg: &'tcx Expr<'tcx>,
    gets: &[&'tcx Expr<'tcx>],
) -> Option<(String, &'static str, Applicability)> {
    let typeck = cx.typeck_results();
    let value_ty = binding_value_ty(cx, typeck, receiver)?;

    // `b.set(b.get() op rhs)`. The left operand must be exactly the binding's
    // own `get()` (parens/`&`/`.clone()` peeled) and `rhs` must not read the
    // binding again — a second read would observe the live guard, not the
    // snapshot. The named method comes first: `b.<op>_assign(rhs)` when `rhs`
    // is a `T`, `b.append(rhs)` when `T: Extend<R>`, and the deref-assign
    // through `get_mut` for a `T` that only implements the `OpAssign` trait.
    if let ExprKind::Binary(op, lhs, rhs) = strip_wraps(cx, typeck, arg).kind
        && let Some(item) = op_assign_lang_item(op.node)
        && is_own_get(cx, typeck, ctxt, binding, strip_wraps(cx, typeck, lhs))
        && own_gets(cx, ctxt, binding, rhs).is_empty()
    {
        let (b, rhs_src) = (snippet_opt(cx, binding.span)?, snippet_opt(cx, rhs.span)?);
        // The operator's operand type: `name.get() + &other` adds a `&str`,
        // which is what `Extend` is asked about. The named methods take a
        // generic argument, so a coercion the operator applied has to be
        // written out — `&*other` — for the rewrite to type-check.
        let rhs_ty = typeck.expr_ty_adjusted(rhs);
        let operand = coerced_operand(typeck, rhs, &rhs_src);
        if let Some(method) = named_op_assign(cx, value_ty, op.node, rhs_ty)
            && let Some(operand) = &operand
        {
            return Some((
                format!("{b}.{method}({operand})"),
                "mutate the binding in place through the named method",
                Applicability::MachineApplicable,
            ));
        }
        if op.node == BinOpKind::Add
            && extend_accepts(cx, value_ty, rhs_ty)
            && let Some(operand) = &operand
        {
            return Some((
                format!("{b}.append({operand})"),
                "append to the binding in place",
                Applicability::MachineApplicable,
            ));
        }
        let applicable = !value_ty.has_infer()
            && !rhs_ty.has_infer()
            && cx
                .tcx
                .lang_items()
                .get(item)
                .is_some_and(|did| implements_trait(cx, value_ty, did, &[rhs_ty.into()]));
        return Some((
            format!("*{b}.get_mut() {}= {rhs_src}", op.node.as_str()),
            "mutate the binding in place through `get_mut`",
            if applicable {
                Applicability::MachineApplicable
            } else {
                Applicability::MaybeIncorrect
            },
        ));
    }

    // `b.set(!b.get())` with `T = bool` → `b.toggle()`.
    if let ExprKind::Unary(UnOp::Not, inner) = strip_wraps(cx, typeck, arg).kind
        && value_ty.is_bool()
        && is_own_get(cx, typeck, ctxt, binding, strip_wraps(cx, typeck, inner))
    {
        return snippet_opt(cx, binding.span).map(|b| {
            (
                format!("{b}.toggle()"),
                "toggle the binding instead",
                Applicability::MachineApplicable,
            )
        });
    }

    // Anything else → `b.with_mut(|v| *v = <arg>)` with every same-binding
    // `get()` spliced to `*v`. `with_mut` needs `T: Clone` and the rewrite is
    // textual, so it stays `MaybeIncorrect`.
    let (b, src) = (snippet_opt(cx, binding.span)?, snippet_opt(cx, arg.span)?);
    let param = if references_local(cx, arg, "v") {
        "value"
    } else {
        "v"
    };
    let mut sorted = gets.to_vec();
    sorted.sort_by_key(|get| get.span.lo());
    let mut rewritten = String::with_capacity(src.len());
    let mut cursor = 0;
    for get in sorted {
        if get.span.lo() < arg.span.lo() || get.span.hi() > arg.span.hi() {
            return None;
        }
        let (lo, hi) = (
            (get.span.lo() - arg.span.lo()).to_usize(),
            (get.span.hi() - arg.span.lo()).to_usize(),
        );
        if lo < cursor || hi > src.len() {
            return None;
        }
        rewritten.push_str(&src[cursor..lo]);
        if in_postfix_position(cx, get) {
            rewritten.push_str(&format!("(*{param})"));
        } else {
            rewritten.push('*');
            rewritten.push_str(param);
        }
        cursor = hi;
    }
    rewritten.push_str(&src[cursor..]);
    Some((
        format!("{b}.with_mut(|{param}| *{param} = {rewritten})"),
        "mutate the binding in place through `with_mut`",
        Applicability::MaybeIncorrect,
    ))
}

impl<'tcx> LateLintPass<'tcx> for SetWithOwnGet {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let Some(did) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if !SET_PATHS
            .iter()
            .any(|path| def_path_eq(cx, implemented_trait_item(cx.tcx, did), path))
        {
            return;
        }
        let &[receiver, arg] = call_args(expr).as_slice() else {
            return;
        };
        let ctxt = expr.span.ctxt();
        let binding = strip_wraps(cx, cx.typeck_results(), receiver);
        let gets = own_gets(cx, ctxt, binding, arg);
        if gets.is_empty() {
            return;
        }
        let fix = suggestion(cx, ctxt, receiver, binding, arg, &gets);
        span_lint_and_then(cx, SET_WITH_OWN_GET, expr.span, MESSAGE, |diag| {
            for get in &gets {
                diag.span_label(get.span, GET_LABEL);
            }
            match fix {
                Some((sugg, label, app)) => {
                    diag.span_suggestion(expr.span, label, sugg, app);
                }
                None => {
                    diag.help(HELP);
                }
            }
        });
    }
}
