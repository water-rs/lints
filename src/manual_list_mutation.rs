use std::ops::ControlFlow;

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::eq_expr_value;
use clippy_utils::res::MaybeResPath;
use clippy_utils::source::snippet_with_applicability;
use clippy_utils::visitors::{Descend, for_each_expr, is_local_used};
use rustc_errors::Applicability;
use rustc_hir::{Block, BlockCheckMode, Expr, ExprKind, HirId, Node, PatKind, Stmt, StmtKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::declare_lint_pass;
use rustc_span::Span;

use crate::applicability::comment_guard;
use crate::carriers::body_expr;
use crate::def_path::def_path_eq;
use crate::list::{replace_call, snapshot_receiver};
use crate::param_bounds::{call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `List` (nami `data::collection::List`) edited through a
    /// snapshot–mutate–replace round trip where `List` has the same method,
    /// or cleared through `replace` with an empty vector:
    ///
    /// - `list.replace(Vec::new())` / `list.replace(vec![])` /
    ///   `list.replace(Default::default())` → `list.clear()`
    /// - `let mut v = list.snapshot(); v.<method>(..); list.replace(v);`
    ///   and the same three statements folded into the argument block
    ///   `list.replace({ let mut v = list.snapshot(); v.<method>(..); v })`
    ///   → `list.<method>(..)` for `push`, `pop`, `insert`, `remove`,
    ///   `clear`, and `sort`
    ///
    /// The mutation's result must be discarded (a bare statement or
    /// `let _ =`), it must be the only use of the snapshot between the
    /// `let` and the `replace`, the `replace` result must be dropped, and
    /// the snapshot must not be read afterwards. A suggestion whose span
    /// carries a comment is `MaybeIncorrect`.
    ///
    /// ### Why is this bad?
    ///
    /// `List::push`/`pop`/`insert`/`remove`/`clear`/`sort` notify watchers
    /// once with the precise change; `snapshot` clones the vector and
    /// `replace` re-publishes the whole thing. The named method is one
    /// expression and one precise notification.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let mut v = items.snapshot();
    /// v.push(x);
    /// items.replace(v);
    /// items.replace(Vec::new());
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// items.push(x);
    /// items.clear();
    /// ```
    pub MANUAL_LIST_MUTATION,
    style,
    "a `List` edited through snapshot–mutate–replace where `List` has the method"
}

declare_lint_pass!(ManualListMutation => [MANUAL_LIST_MUTATION]);

/// `alloc::vec::Vec::new` — also what `vec![]` lowers to.
const VEC_NEW: &[&str] = &["alloc", "vec", "Vec", "new"];

/// `core::default::Default::default` — `Default::default()`,
/// `Vec::default()`, and `<Vec<T> as Default>::default()` all resolve here
/// through [`implemented_trait_item`].
const DEFAULT_DEFAULT: &[&str] = &["core", "default", "Default", "default"];

/// The single `Vec` mutations `List` exposes one-to-one, with each call's
/// argument count. The bounds each `List` method adds (`T: Clone`, `Ord`
/// for `sort`) are already discharged by the `snapshot()` and the `Vec`
/// call the round trip contains.
const MUTATIONS: &[(&str, usize)] = &[
    ("push", 1),
    ("pop", 0),
    ("insert", 2),
    ("remove", 1),
    ("clear", 0),
    ("sort", 0),
];

/// `List::pop` and `List::remove` are `#[must_use]` — a discarded-result
/// rewrite spells `let _ =` for them.
const MUST_USE: &[&str] = &["pop", "remove"];

const SUGGESTION_LABEL: &str = "use the `List` method";

/// `expr`'s drop-temps peeled — argument positions can carry one where a
/// statement's expression does not.
fn peel_drop_temps<'hir>(expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    let mut expr = expr;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    expr
}

/// The statement `expr`'s value drops at — `expr;` (possibly under a
/// `DropTemps` or a block tail, as in `{ list.replace(v) };`) or
/// `let _ = expr;`. `None` when the value is bound or otherwise observed:
/// `let old = list.replace(v)` keeps the previous vector, and a tail `expr`
/// in a used position stays a use.
fn discarded_stmt<'hir>(
    cx: &LateContext<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<&'hir Stmt<'hir>> {
    let mut id = expr.hir_id;
    loop {
        match cx.tcx.parent_hir_node(id) {
            // A block tail climbs through its block's `Expr` node; reaching
            // one directly means `id` was that tail.
            Node::Expr(e) if matches!(e.kind, ExprKind::DropTemps(_) | ExprKind::Block(..)) => {
                id = e.hir_id;
            }
            Node::Block(block) if block.expr.is_some_and(|tail| tail.hir_id == id) => {
                id = block.hir_id;
            }
            Node::Stmt(stmt) => match stmt.kind {
                StmtKind::Semi(e) | StmtKind::Expr(e) if e.hir_id == id => break Some(stmt),
                _ => break None,
            },
            Node::LetStmt(local)
                if local.init.is_some_and(|init| init.hir_id == id)
                    && matches!(local.pat.kind, PatKind::Wild) =>
            {
                match cx.tcx.parent_hir_node(local.hir_id) {
                    Node::Stmt(stmt) => break Some(stmt),
                    _ => break None,
                }
            }
            _ => break None,
        }
    }
}

/// `stmt` is `let <v> = <recv>.snapshot();` — the let that binds snapshot
/// local `v`. Returns the snapshot's receiver.
fn snapshot_let<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    stmt: &'hir Stmt<'hir>,
    v: HirId,
) -> Option<&'hir Expr<'hir>> {
    if stmt.span.from_expansion() {
        return None;
    }
    let StmtKind::Let(local) = stmt.kind else {
        return None;
    };
    if !matches!(local.pat.kind, PatKind::Binding(_, id, _, None) if id == v) {
        return None;
    }
    snapshot_receiver(cx, typeck, peel_drop_temps(local.init?))
}

/// `stmt` is the round trip's one mutation — `v.<method>(<args>);` or
/// `let _ = v.<method>(<args>);` for a `MUTATIONS` method on local `v`,
/// whose arguments do not mention `v` and cannot rebind the list's base
/// local. Returns the method name, the argument expressions, and whether
/// the statement was a `let _ =`.
fn mutation_stmt<'hir>(
    cx: &LateContext<'hir>,
    stmt: &'hir Stmt<'hir>,
    v: HirId,
) -> Option<(&'static str, &'hir [Expr<'hir>], bool)> {
    if stmt.span.from_expansion() {
        return None;
    }
    let (call, let_underscore) = match stmt.kind {
        StmtKind::Semi(e) | StmtKind::Expr(e) => (peel_drop_temps(e), false),
        StmtKind::Let(local) if matches!(local.pat.kind, PatKind::Wild) && local.els.is_none() => {
            (peel_drop_temps(local.init?), true)
        }
        _ => return None,
    };
    if call.span.from_expansion() {
        return None;
    }
    let ExprKind::MethodCall(segment, receiver, args, _) = call.kind else {
        return None;
    };
    // `v` is `Vec<T>` — the name resolves to the inherent `Vec`/slice
    // method over any in-scope trait.
    let &(name, arity) = MUTATIONS
        .iter()
        .find(|(name, _)| *name == segment.ident.name.as_str())?;
    if args.len() != arity
        || body_expr(receiver).res_local_id() != Some(v)
        || args
            .iter()
            .any(|arg| is_local_used(cx, arg, v) || arg_rebinds(cx, arg))
    {
        return None;
    }
    Some((name, args, let_underscore))
}

/// `arg` contains an assignment or a block with statements — either can
/// rebind the receiver's base local between the snapshot's evaluation and
/// the `replace`'s, so the collapsed call could bind a different list.
fn arg_rebinds<'hir>(cx: &LateContext<'hir>, arg: &'hir Expr<'hir>) -> bool {
    for_each_expr(cx, arg, |e| -> ControlFlow<(), Descend> {
        if matches!(e.kind, ExprKind::Assign(..) | ExprKind::AssignOp(..))
            || matches!(e.kind, ExprKind::Block(block, _) if !block.stmts.is_empty())
        {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(Descend::Yes)
        }
    })
    .is_some()
}

/// `arg` is an empty vector — `Vec::new()` (also the `vec![]` lowering) or
/// a `Default::default()` in `Vec` position, which `replace`'s signature
/// pins to `<Vec<T> as Default>::default`.
fn is_empty_vec<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    arg: &Expr<'hir>,
) -> bool {
    let Some(did) = call_def_id(typeck, arg) else {
        return false;
    };
    def_path_eq(cx, did, VEC_NEW)
        || def_path_eq(cx, implemented_trait_item(cx.tcx, did), DEFAULT_DEFAULT)
}

/// `stmt` and `expr` resolve to the same discard position, and `block`
/// holds `stmt` — for the statement spelling, the `let`, the mutation, and
/// the `replace` sit in one block.
fn stmt_block<'hir>(
    cx: &LateContext<'hir>,
    stmt: &'hir Stmt<'hir>,
) -> Option<(&'hir Block<'hir>, usize)> {
    let Node::Block(block) = cx.tcx.parent_hir_node(stmt.hir_id) else {
        return None;
    };
    let index = block.stmts.iter().position(|s| s.hir_id == stmt.hir_id)?;
    Some((block, index))
}

/// `<recv>.<name>(<args>)` as source text — `let _ =`-prefixed when the
/// caller asks for the discard spelling, `;`-terminated when the replaced
/// span ends at a semicolon (the statement spelling replaces through it).
fn method_call_text<'hir>(
    cx: &LateContext<'hir>,
    receiver: &Expr<'hir>,
    name: &str,
    args: &[&'hir Expr<'hir>],
    let_underscore: bool,
    semi: bool,
    applicability: &mut Applicability,
) -> String {
    let receiver = snippet_with_applicability(cx, receiver.span, "..", applicability);
    let args = args
        .iter()
        .map(|arg| snippet_with_applicability(cx, arg.span, "..", applicability))
        .collect::<Vec<_>>()
        .join(", ");
    let prefix = if let_underscore { "let _ = " } else { "" };
    let semi = if semi { ";" } else { "" };
    format!("{prefix}{receiver}.{name}({args}){semi}")
}

/// Emits the `List::<name>` round-trip diagnostic on `span` with the
/// collapsed call as the fix.
fn report(
    cx: &LateContext<'_>,
    span: Span,
    name: &str,
    suggestion: String,
    applicability: Applicability,
) {
    span_lint_and_then(
        cx,
        MANUAL_LIST_MUTATION,
        span,
        format!("this snapshot/replace round trip restates `List::{name}`"),
        |diag| {
            diag.span_suggestion(span, SUGGESTION_LABEL, suggestion, applicability);
        },
    );
}

/// `let mut v = list.snapshot(); v.<method>(<args>); list.replace(v);` —
/// three statements in one block, `v` unread after the `replace`.
fn check_stmt_spelling<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    stmt: &'hir Stmt<'hir>,
    receiver: &'hir Expr<'hir>,
    arg: &'hir Expr<'hir>,
) {
    let Some(v) = arg.res_local_id() else { return };
    let Some((block, index)) = stmt_block(cx, stmt) else {
        return;
    };
    if index < 2 || is_local_used(cx, (&block.stmts[index + 1..], block.expr), v) {
        return;
    }
    let Some((name, args, let_underscore)) = mutation_stmt(cx, &block.stmts[index - 1], v) else {
        return;
    };
    let Some(snapshot) = snapshot_let(cx, typeck, &block.stmts[index - 2], v) else {
        return;
    };
    // `v` must be a snapshot of the list being replaced — a copy of
    // another list pushed onto this one is not `list.push`.
    if !eq_expr_value(cx, expr.span.ctxt(), snapshot, receiver) {
        return;
    }
    let mut applicability = Applicability::MachineApplicable;
    let args = args.iter().collect::<Vec<_>>();
    let span = block.stmts[index - 2].span.to(stmt.span);
    // The suggestion fills whole statements, so a `let _ =` always parses;
    // `List::pop`/`remove` are `#[must_use]` and need it regardless.
    let suggestion = method_call_text(
        cx,
        receiver,
        name,
        &args,
        let_underscore || MUST_USE.contains(&name),
        true,
        &mut applicability,
    );
    comment_guard(cx, span, &mut applicability);
    report(cx, span, name, suggestion, applicability);
}

/// `list.replace({ let mut v = list.snapshot(); v.<method>(<args>); v })` —
/// the round trip folded into the argument block.
fn check_block_spelling<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    stmt: &'hir Stmt<'hir>,
    receiver: &'hir Expr<'hir>,
    block: &'hir Block<'hir>,
) {
    if block.span.from_expansion() {
        return;
    }
    let [snapshot_stmt, mutation] = block.stmts else {
        return;
    };
    let Some(tail) = block.expr else { return };
    let Some(v) = body_expr(tail).res_local_id() else {
        return;
    };
    let Some((name, args, let_underscore)) = mutation_stmt(cx, mutation, v) else {
        return;
    };
    let Some(snapshot) = snapshot_let(cx, typeck, snapshot_stmt, v) else {
        return;
    };
    if !eq_expr_value(cx, expr.span.ctxt(), snapshot, receiver) {
        return;
    }
    let mut applicability = Applicability::MachineApplicable;
    let args = args.iter().collect::<Vec<_>>();
    // The suggestion fills the call's expression slot: a `let _ =` prefix
    // parses as a statement only when the `replace` itself was a bare
    // statement; inside `let _ = list.replace(..)` the existing binding
    // already discards.
    let let_underscore = (let_underscore || MUST_USE.contains(&name))
        && matches!(stmt.kind, StmtKind::Semi(_) | StmtKind::Expr(_));
    let suggestion = method_call_text(
        cx,
        receiver,
        name,
        &args,
        let_underscore,
        false,
        &mut applicability,
    );
    comment_guard(cx, expr.span, &mut applicability);
    report(cx, expr.span, name, suggestion, applicability);
}

impl<'tcx> LateLintPass<'tcx> for ManualListMutation {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let typeck = cx.typeck_results();
        let Some((receiver, arg)) = replace_call(cx, typeck, expr) else {
            return;
        };
        // `replace` returns the previous vector — the round trip is a
        // mutation only when that result is dropped.
        let Some(stmt) = discarded_stmt(cx, expr) else {
            return;
        };
        let arg = peel_drop_temps(arg);
        match arg.kind {
            ExprKind::Path(..) => check_stmt_spelling(cx, typeck, expr, stmt, receiver, arg),
            // An `unsafe`/labeled/`try` block wraps the round trip in a
            // context the collapsed call would lose — the rewrite drops the
            // block, so only a plain one is replaced.
            ExprKind::Block(block, None)
                if matches!(block.rules, BlockCheckMode::DefaultBlock)
                    && !block.targeted_by_break =>
            {
                check_block_spelling(cx, typeck, expr, stmt, receiver, block)
            }
            _ if is_empty_vec(cx, typeck, arg) => {
                let mut applicability = Applicability::MachineApplicable;
                let suggestion =
                    method_call_text(cx, receiver, "clear", &[], false, false, &mut applicability);
                comment_guard(cx, expr.span, &mut applicability);
                span_lint_and_then(
                    cx,
                    MANUAL_LIST_MUTATION,
                    expr.span,
                    "this `replace` with an empty vector is `List::clear`",
                    |diag| {
                        diag.span_suggestion(
                            expr.span,
                            SUGGESTION_LABEL,
                            suggestion,
                            applicability,
                        );
                    },
                );
            }
            _ => {}
        }
    }
}
