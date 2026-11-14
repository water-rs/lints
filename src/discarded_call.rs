//! The block walk shared by lints that flag a discarded call result —
//! `task_handle_dropped`, `discarded_wait_result`, and any future lint of the
//! same shape. Each lint keeps its own def-path table and diagnostics; the
//! traversal lives here once.

use clippy_utils::visitors::is_local_used;
use rustc_hir::{Block, Expr, ExprKind, PatKind, Stmt, StmtKind};
use rustc_lint::LateContext;

use crate::def_path::def_path_eq;
use crate::param_bounds::call_def_id;

/// A statement whose `matches`-matched call result is discarded.
pub(crate) enum Discarded<'hir> {
    /// A `<call>;` statement or `let _ = <call>;` — the value drops at the
    /// semicolon, so the statement can rewrite to a use of the call.
    AtSemicolon {
        stmt: &'hir Stmt<'hir>,
        call: &'hir Expr<'hir>,
    },
    /// `let x = <call>;` where the statements after it and the block tail
    /// never read `x` — there is no mechanical fix, only a finding.
    NeverRead {
        stmt: &'hir Stmt<'hir>,
        call: &'hir Expr<'hir>,
    },
}

/// `expr`, drop-temps peeled, when it is a `Call`/`MethodCall` whose callee's
/// `get_def_path` equals an entry of `paths`. Parentheses carry no HIR node,
/// so a parenthesized call reaches here with the parens folded into its span.
pub(crate) fn path_call<'hir>(
    cx: &LateContext<'hir>,
    expr: &'hir Expr<'hir>,
    paths: &[&[&'static str]],
) -> Option<&'hir Expr<'hir>> {
    let mut expr = expr;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    if expr.span.from_expansion()
        || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
    {
        return None;
    }
    call_def_id(cx.typeck_results(), expr)
        .is_some_and(|did| paths.iter().any(|path| def_path_eq(cx, did, path)))
        .then_some(expr)
}

/// Every statement in `block` whose `matches`-matched call result is
/// discarded, in source order.
pub(crate) fn discarded_calls<'hir>(
    cx: &LateContext<'hir>,
    block: &'hir Block<'hir>,
    matches: impl Fn(&LateContext<'hir>, &'hir Expr<'hir>) -> Option<&'hir Expr<'hir>>,
) -> Vec<Discarded<'hir>> {
    let mut found = Vec::new();
    if block.span.from_expansion() {
        return found;
    }
    for (index, stmt) in block.stmts.iter().enumerate() {
        match stmt.kind {
            // A no-semi `StmtKind::Expr` mid-block must be `()`-typed, so a
            // discarded call result can only sit in `Semi` or a `let`
            // initializer.
            StmtKind::Semi(expr) => {
                if let Some(call) = matches(cx, expr) {
                    found.push(Discarded::AtSemicolon { stmt, call });
                }
            }
            StmtKind::Let(local) => {
                let Some(call) = local.init.and_then(|init| matches(cx, init)) else {
                    continue;
                };
                match local.pat.kind {
                    PatKind::Wild => found.push(Discarded::AtSemicolon { stmt, call }),
                    PatKind::Binding(_, hir_id, ..)
                        if !is_local_used(cx, (&block.stmts[index + 1..], block.expr), hir_id) =>
                    {
                        found.push(Discarded::NeverRead { stmt, call });
                    }
                    _ => {}
                }
            }
            StmtKind::Expr(_) | StmtKind::Item(_) => {}
        }
    }
    found
}
