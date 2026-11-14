use clippy_utils::source::snippet_with_applicability;
use rustc_errors::Applicability;
use rustc_hir::{Block, Expr, Stmt};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::diagnostics::span_lint_and_then;
use crate::discarded_call::{Discarded, discarded_calls, path_call};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `Query::wait_for_existence`/`wait_for_nonexistence`/
    /// `wait_for_value_eq` calls — and the `SemanticApp` methods of the same
    /// names — whose `bool` result is discarded: a bare `<call>;` statement,
    /// `let _ = <call>;`, or a `let x = <call>;` binding that is never read
    /// before the enclosing block ends.
    ///
    /// ### Why is this bad?
    ///
    /// The waits return `false` on timeout. `Query`'s are `#[must_use]`, so
    /// `let _ =` is the one form that silences the warning, and the
    /// `SemanticApp` methods carry no `#[must_use]` at all — either way the
    /// test waits out the timeout and can never fail on it.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let _ = app.query().label("Save").wait_for_existence(Duration::from_secs(1));
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// assert!(app.query().label("Save").wait_for_existence(Duration::from_secs(1)));
    /// ```
    pub DISCARDED_WAIT_RESULT,
    correctness,
    "the result of `wait_for_*` is discarded, so this wait can never fail"
}

declare_lint_pass!(DiscardedWaitResult => [DISCARDED_WAIT_RESULT]);

/// `waterui_testing` waits that return `false` on timeout — the `Query`
/// methods (each `#[must_use]`, which `let _ =` still silences) and the
/// `SemanticApp` conveniences they forward to (no `#[must_use]` at all, so a
/// discard there escapes `unused_must_use` entirely). `get_def_path` spells
/// both through their private defining modules.
const WAIT_PATHS: &[&[&str]] = &[
    &["waterui_testing", "query", "Query", "wait_for_existence"],
    &["waterui_testing", "query", "Query", "wait_for_nonexistence"],
    &["waterui_testing", "query", "Query", "wait_for_value_eq"],
    &[
        "waterui_testing",
        "app",
        "SemanticApp",
        "wait_for_existence",
    ],
    &[
        "waterui_testing",
        "app",
        "SemanticApp",
        "wait_for_nonexistence",
    ],
    &["waterui_testing", "app", "SemanticApp", "wait_for_value_eq"],
];

const MESSAGE: &str = "the result of `wait_for_*` is discarded, so this wait can never fail";
const CALL_LABEL: &str = "the wait's `false`-on-timeout result is dropped here";
const ASSERT_SUGGESTION: &str = "assert the result so a timeout fails the test";
const UNREAD_HELP: &str = "`wait_for_*` returns `false` on timeout; wrap it — `assert!(app.query().label(..).wait_for_existence(timeout))`";

/// A `<call>;` statement or `let _ = <call>;` — the `false`-on-timeout result
/// drops at the semicolon, so the statement rewrites to `assert!(<call>);`.
fn lint_discarded(cx: &LateContext<'_>, stmt: &Stmt<'_>, call: &Expr<'_>) {
    let mut applicability = Applicability::MachineApplicable;
    let call_src =
        snippet_with_applicability(cx, call.span.source_callsite(), "..", &mut applicability);
    span_lint_and_then(cx, DISCARDED_WAIT_RESULT, stmt.span, MESSAGE, |diag| {
        diag.span_label(call.span, CALL_LABEL);
        diag.span_suggestion(
            stmt.span,
            ASSERT_SUGGESTION,
            format!("assert!({call_src});"),
            applicability,
        );
    });
}

/// A `let x = <call>;` whose `x` nothing else in the block reads — the
/// timeout result is still dropped, but there is no mechanical rewrite, so
/// this only points at the fix.
fn lint_unread(cx: &LateContext<'_>, stmt: &Stmt<'_>, call: &Expr<'_>) {
    span_lint_and_then(cx, DISCARDED_WAIT_RESULT, stmt.span, MESSAGE, |diag| {
        diag.span_label(call.span, CALL_LABEL);
        diag.help(UNREAD_HELP);
    });
}

impl<'tcx> LateLintPass<'tcx> for DiscardedWaitResult {
    fn check_block(&mut self, cx: &LateContext<'tcx>, block: &'tcx Block<'tcx>) {
        for discarded in discarded_calls(cx, block, |cx, expr| path_call(cx, expr, WAIT_PATHS)) {
            match discarded {
                Discarded::AtSemicolon { stmt, call } => lint_discarded(cx, stmt, call),
                Discarded::NeverRead { stmt, call } => lint_unread(cx, stmt, call),
            }
        }
    }
}
