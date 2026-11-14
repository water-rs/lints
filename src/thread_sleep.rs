//! `std::thread::sleep` plumbing shared by the lints that flag it: the sleep
//! def path, plus recognition of the `#[waterui::test]`/`#[waterui::bench]`
//! wrapper functions — a body inside one is a harness context, not a UI one
//! (`blocking_in_ui_context`/`thread_sleep_in_ui` stay silent there;
//! `thread_sleep_in_test` owns the sleep).

use clippy_utils::macros::macro_backtrace;
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::def::DefKind;
use rustc_hir::def_id::LocalDefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::hir::nested_filter::OnlyBodies;
use rustc_middle::ty::{TyCtxt, TypeckResults};

use crate::def_path::def_path_eq;
use crate::param_bounds::call_def_id;

/// `std::thread::sleep` — its def path is `std::thread::functions::sleep`:
/// `sleep` is `pub use`d from the private `functions` module.
pub(crate) const SLEEP_PATH: &str = "std::thread::functions::sleep";

/// `waterui_testing::app::ui` — the session-builder call both harness macros
/// start their generated body with (`::waterui_testing::ui()` in the expansion
/// resolves here through the crate-root re-export).
const UI: &[&str] = &["waterui_testing", "app", "ui"];

/// The attribute macros that emit a test wrapper — `#[waterui::test]` is
/// `waterui_macros::ui_test` re-exported, `#[waterui::bench]` is
/// `waterui_macros::bench`.
const WRAPPER_MACROS: &[&[&str]] = &[&["waterui_macros", "ui_test"], &["waterui_macros", "bench"]];

/// Whether `did` names a function `#[waterui::test]`/`#[waterui::bench]`
/// emitted. Both expand to `#[test] fn <name>() { … }` whose body contains a
/// `<testing>::ui()` builder call — in the first statement for `ui_test`, in
/// the `run_bench` closure for `bench`, and in the `block_on(async { .. })`
/// coroutine for an `async` test. The generated `ui()` token carries the
/// attribute macro's expansion context, so a body holding a
/// `waterui_testing::app::ui` call whose macro backtrace reaches
/// `ui_test`/`bench` is a generated wrapper; a user-written `ui()` call keeps
/// its own spans and does not match. `cache` memoizes the verdict per
/// `LocalDefId` so a body with several sleeps resolves it once.
pub(crate) fn is_test_wrapper(
    cx: &LateContext<'_>,
    did: LocalDefId,
    cache: &mut FxHashMap<LocalDefId, bool>,
) -> bool {
    if let Some(&verdict) = cache.get(&did) {
        return verdict;
    }
    let verdict = has_wrapper_call(cx, did);
    cache.insert(did, verdict);
    verdict
}

fn has_wrapper_call(cx: &LateContext<'_>, did: LocalDefId) -> bool {
    if cx.tcx.def_kind(did) != DefKind::Fn {
        return false;
    }
    let body = cx.tcx.hir_body_owned_by(did);
    let mut scan = WrapperCall {
        cx,
        typeck: cx.tcx.typeck(did),
        found: false,
    };
    scan.visit_body(body);
    scan.found
}

/// Finds the generated `waterui_testing::app::ui` call in a wrapper body. The
/// walk descends into nested bodies — `bench` puts the call inside the
/// `run_bench` closure and an `async` test inside the `block_on` coroutine —
/// all of which share the enclosing fn's `TypeckResults`.
struct WrapperCall<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    typeck: &'tcx TypeckResults<'tcx>,
    found: bool,
}

impl<'tcx> Visitor<'tcx> for WrapperCall<'_, 'tcx> {
    type NestedFilter = OnlyBodies;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.cx.tcx
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if self.found {
            return;
        }
        if matches!(expr.kind, ExprKind::Call(..))
            && call_def_id(self.typeck, expr).is_some_and(|callee| def_path_eq(self.cx, callee, UI))
            && macro_backtrace(expr.span).any(|call| {
                WRAPPER_MACROS
                    .iter()
                    .any(|path| def_path_eq(self.cx, call.def_id, path))
            })
        {
            self.found = true;
            return;
        }
        intravisit::walk_expr(self, expr);
    }
}
