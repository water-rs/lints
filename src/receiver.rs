//! Receiver-move analysis shared by lints whose fix deletes a `&self`
//! method segment (`x.computed()` → `x`, `x.clone().m()` → `x`,
//! `x.map(..)` → `x`): the deletion hands the receiver — previously only
//! borrowed — to whatever consumed the call's result.

use clippy_utils::res::MaybeResPath;
use clippy_utils::sugg::Sugg;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_hir::{Expr, ExprKind, Node, UnOp};
use rustc_lint::LateContext;
use rustc_middle::ty::adjustment::Adjust;
use rustc_middle::ty::{Ty, TyKind, TypeckResults};
use rustc_span::Span;
use std::ops::ControlFlow;

/// Whether `expr` sits inside a loop (`for`/`while` desugar to
/// `ExprKind::Loop`) that does not contain `binding_span` — a receiver local
/// bound outside the loop is read again on the next iteration, which the
/// source-position test below cannot see. Walks `expr`'s parents up to the
/// enclosing body.
pub(crate) fn in_repeating_loop(cx: &LateContext<'_>, expr: &Expr<'_>, binding_span: Span) -> bool {
    let owner = cx
        .tcx
        .local_def_id_to_hir_id(cx.tcx.hir_enclosing_body_owner(expr.hir_id));
    let mut id = expr.hir_id;
    while id != owner {
        match cx.tcx.parent_hir_node(id) {
            Node::Expr(parent) => {
                if let ExprKind::Loop(..) = parent.kind
                    && !parent.span.contains(binding_span)
                {
                    return true;
                }
                id = parent.hir_id;
            }
            // Statements, the body's own block, and the `for`-desugar's
            // `Some(pat) => ..` match arm sit between an expression and the
            // loop around it.
            Node::Stmt(stmt) => id = stmt.hir_id,
            Node::LetStmt(stmt) => id = stmt.hir_id,
            Node::Block(block) => id = block.hir_id,
            Node::Arm(arm) => id = arm.hir_id,
            _ => return false,
        }
    }
    false
}

/// Whether deleting a `&self` method segment off `expr` would move
/// `receiver` somewhere it must not go — only possible when the call's
/// result itself lands at a by-value position (`direct`; a `&self` consumer
/// between them keeps the receiver borrowed). A place must not move when it
/// is a field, index, or deref (the move would fail outright or partially
/// move the owner), a non-local path such as a `static`, or a local read
/// again — by a use after `expr` or by a loop iteration
/// (`in_repeating_loop`). A local on its last use and rvalue receivers
/// (calls, temporaries) keep the plain deletion.
pub(crate) fn receiver_needs_clone<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    receiver: &Expr<'tcx>,
    direct: bool,
) -> bool {
    if !direct {
        return false;
    }
    match receiver.kind {
        ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::Unary(UnOp::Deref, _) => true,
        ExprKind::Path(..) => match receiver.res_local_id() {
            Some(local) => {
                if in_repeating_loop(cx, expr, cx.tcx.hir_span(local)) {
                    return true;
                }
                let body = cx
                    .tcx
                    .hir_body_owned_by(cx.tcx.hir_enclosing_body_owner(expr.hir_id));
                for_each_expr(cx, body.value, |e| {
                    if e.hir_id != receiver.hir_id
                        && e.res_local_id() == Some(local)
                        && e.span.lo() > expr.span.hi()
                    {
                        return ControlFlow::Break(());
                    }
                    ControlFlow::Continue(Descend::Yes)
                })
                .is_some()
            }
            None => true,
        },
        _ => false,
    }
}

/// The number of leading `&`/`&mut` layers on `ty`. A spelled operand may
/// carry references: `a.zip(borrowed)` with `borrowed: &B` types the
/// argument as `&B` itself, and a receiver reached through autoderef
/// (`borrowed.zip(..)`) reads as `&A`/`&&A`/….
pub(crate) fn ref_depth(ty: Ty<'_>) -> usize {
    let mut depth = 0;
    let mut ty = ty;
    while let TyKind::Ref(_, inner, _) = ty.kind() {
        depth += 1;
        ty = *inner;
    }
    depth
}

/// The `*`s `expr` needs spelled before `.clone()` to reach the owned
/// value. Two sources count the same layers: `expr`'s own `&`/`&mut` type
/// (`borrowed: &B` spelled verbatim — an argument receives no adjustments),
/// and the `Deref` steps autoderef applied to reach the method's self type
/// (`borrowed.zip(..)`/`b.map(..)` for `b: Rc<B>` — overloaded derefs
/// included).
pub(crate) fn deref_depth(typeck: &TypeckResults<'_>, expr: &Expr<'_>) -> usize {
    let adjusted = typeck
        .expr_adjustments(expr)
        .iter()
        .filter(|adjustment| matches!(adjustment.kind, Adjust::Deref(_)))
        .count();
    ref_depth(typeck.expr_ty(expr)).max(adjusted)
}

/// `expr` spelled where an owned value is needed — the receiver of a
/// deleted `&self` segment, a by-value `a op b` operand: a place clones, a
/// temporary moves, and a place reached through `&`/`Deref` dereferences
/// back to the place first (`(*b).clone()` for `b: &B` or `b: Rc<B>`).
pub(crate) fn owned_spelling(
    typeck: &TypeckResults<'_>,
    expr: &Expr<'_>,
    sugg: Sugg<'_>,
) -> String {
    let depth = deref_depth(typeck, expr);
    if depth > 0 {
        format!("({}{}).clone()", "*".repeat(depth), sugg.maybe_paren())
    } else if expr.is_syntactic_place_expr() {
        format!("{}.clone()", sugg.maybe_paren())
    } else {
        sugg.maybe_paren().to_string()
    }
}
