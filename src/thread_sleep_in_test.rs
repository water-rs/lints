use rustc_data_structures::fx::FxHashMap;
use rustc_hir::def_id::LocalDefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::impl_lint_pass;

use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_help;
use crate::param_bounds::call_def_id;
use crate::thread_sleep::{SLEEP_PATH, is_test_wrapper};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `std::thread::sleep` call written in the body of a
    /// `#[waterui::test]` or `#[waterui::bench]` function — directly, in a
    /// nested block or loop, or inside a closure literal within the body. A
    /// sleep inside a separate `fn` the test calls is out of scope (a
    /// callsite lint).
    ///
    /// ### Why is this bad?
    ///
    /// The harness's animation clock advances when frames are pumped, not
    /// with wall time: a sleep freezes it, so a transition snapshot silently
    /// shows its end state while the test passes.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// #[waterui::test(home)]
    /// fn opens(app: &mut SemanticApp) {
    ///     std::thread::sleep(Duration::from_millis(300));
    ///     assert!(app.query().label("Done").exists());
    /// }
    /// ```
    ///
    /// Pump frames to advance the clock, or wait on a condition:
    ///
    /// ```rust,ignore
    /// #[waterui::test(home)]
    /// fn opens(app: &mut SemanticApp) {
    ///     app.pump_for(Duration::from_millis(300));
    ///     assert!(app.query().label("Done").wait_for_existence(Duration::from_secs(1)));
    /// }
    /// ```
    pub THREAD_SLEEP_IN_TEST,
    correctness,
    "`std::thread::sleep` inside a `#[waterui::test]` freezes the animation clock"
}

/// The wrapper verdict per enclosing fn `LocalDefId` — a body with several
/// sleeps resolves it once.
pub(crate) struct ThreadSleepInTest {
    wrappers: FxHashMap<LocalDefId, bool>,
    /// `SLEEP_PATH` as `get_def_path` segments.
    sleep_path: Vec<&'static str>,
}

impl Default for ThreadSleepInTest {
    fn default() -> Self {
        Self {
            wrappers: FxHashMap::default(),
            sleep_path: SLEEP_PATH.split("::").collect(),
        }
    }
}

impl_lint_pass!(ThreadSleepInTest => [THREAD_SLEEP_IN_TEST]);

const HELP: &str = "the clock advances when frames are pumped, not with wall time — use `app.pump_for(duration)` to advance it, or `Query::wait_for_existence`/`wait_for_value_eq` to wait for a condition";

impl<'tcx> LateLintPass<'tcx> for ThreadSleepInTest {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() || !matches!(expr.kind, ExprKind::Call(..)) {
            return;
        }
        let Some(callee) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if !def_path_eq(cx, callee, &self.sleep_path) {
            return;
        }
        // The typeck root is the enclosing non-closure body owner — the `fn`
        // the `#[waterui::test]`/`#[waterui::bench]` attribute expanded.
        let owner = cx
            .tcx
            .typeck_root_def_id(cx.tcx.hir_enclosing_body_owner(expr.hir_id).into())
            .expect_local();
        if !is_test_wrapper(cx, owner, &mut self.wrappers) {
            return;
        }
        span_lint_and_help(
            cx,
            THREAD_SLEEP_IN_TEST,
            expr.span,
            "`std::thread::sleep` inside a `#[waterui::test]` freezes the animation clock",
            None,
            HELP,
        );
    }
}
