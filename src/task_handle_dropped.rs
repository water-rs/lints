use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::source::snippet_with_applicability;
use rustc_errors::Applicability;
use rustc_hir::{Block, Expr, Stmt};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::discarded_call::{Discarded, discarded_calls, path_call};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `waterui::task::spawn`/`spawn_local` calls whose task handle is
    /// dropped at once: a bare `spawn(..);` statement, `let _ = spawn(..);`,
    /// or a `let t = spawn(..);` binding that is never read before the
    /// enclosing block ends.
    ///
    /// ### Why is this bad?
    ///
    /// The handle cancels the task when it is dropped, so the spawned future
    /// never runs — the classic "a background task never runs" symptom.
    /// `.detach()` lets the task run to completion, while awaiting or storing
    /// the handle keeps it alive.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// spawn_local(async { load().await });
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// spawn_local(async { load().await }).detach();
    /// ```
    pub TASK_HANDLE_DROPPED,
    correctness,
    "this task handle is dropped at once, cancelling the task"
}

declare_lint_pass!(TaskHandleDropped => [TASK_HANDLE_DROPPED]);

/// `executor_core::std_on::{spawn, spawn_local}` — the defining-crate paths
/// behind `waterui::task::{spawn, spawn_local}` (waterui-internal 0.4.1
/// `runtime/task.rs` re-exports `executor_core::{spawn, spawn_local}`; the
/// functions live in a private `mod std_on` re-exported flat, so
/// `get_def_path` spells them `executor_core::std_on::*`).
const SPAWN_PATHS: &[&[&str]] = &[
    &["executor_core", "std_on", "spawn"],
    &["executor_core", "std_on", "spawn_local"],
];

const MESSAGE: &str = "this task handle is dropped at once, cancelling the task";
const CALL_LABEL: &str = "the task is cancelled when this handle is dropped";
const DETACH_SUGGESTION: &str = "detach the task so it runs to completion";
const UNREAD_HELP: &str =
    "`.await` the handle, keep it alive for as long as the task should run, or `.detach()` it";

/// A `spawn(..);` statement or `let _ = spawn(..);` — the handle drops at the
/// semicolon, so the statement rewrites to `<call>.detach();`.
fn lint_detached(cx: &LateContext<'_>, stmt: &Stmt<'_>, call: &Expr<'_>) {
    let mut applicability = Applicability::MachineApplicable;
    let call_src =
        snippet_with_applicability(cx, call.span.source_callsite(), "..", &mut applicability);
    span_lint_and_then(cx, TASK_HANDLE_DROPPED, stmt.span, MESSAGE, |diag| {
        diag.span_label(call.span, CALL_LABEL);
        diag.span_suggestion(
            stmt.span,
            DETACH_SUGGESTION,
            format!("{call_src}.detach();"),
            applicability,
        );
    });
}

/// A `let t = spawn(..);` whose `t` nothing else in the block reads — the
/// handle still drops at scope end, but there is no mechanical rewrite (any
/// read counts: `.await`, `.detach()`, a move into `drop(t)`, a capture), so
/// this only points at the options.
fn lint_unread(cx: &LateContext<'_>, stmt: &Stmt<'_>, call: &Expr<'_>) {
    span_lint_and_then(cx, TASK_HANDLE_DROPPED, stmt.span, MESSAGE, |diag| {
        diag.span_label(call.span, CALL_LABEL);
        diag.help(UNREAD_HELP);
    });
}

impl<'tcx> LateLintPass<'tcx> for TaskHandleDropped {
    fn check_block(&mut self, cx: &LateContext<'tcx>, block: &'tcx Block<'tcx>) {
        for discarded in discarded_calls(cx, block, |cx, expr| path_call(cx, expr, SPAWN_PATHS)) {
            match discarded {
                Discarded::AtSemicolon { stmt, call } => lint_detached(cx, stmt, call),
                Discarded::NeverRead { stmt, call } => lint_unread(cx, stmt, call),
            }
        }
    }
}
