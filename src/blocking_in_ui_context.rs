use clippy_utils::diagnostics::span_lint_and_help;
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::def_id::LocalDefId;
use rustc_hir::{ClosureKind, CoroutineDesugaring, CoroutineKind, Expr, ExprKind, Node};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::impl_lint_pass;
use rustc_span::Symbol;

use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, handler_closures, implemented_trait_item};
use crate::thread_sleep::{SLEEP_PATH, is_test_wrapper};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a call that blocks the current thread — every `std::fs::*` and
    /// `std::net::*` function, `std::process::Command::{output, status}`,
    /// `std::process::Child::wait`, `futures::executor::block_on` /
    /// `futures_executor::block_on` / `smol::block_on` / `async_io::block_on`,
    /// `reqwest::blocking::*`, `std::sync::Mutex::lock`, and
    /// `std::sync::RwLock::{read, write}` — inside a scope that runs on the UI
    /// thread's executor: a closure passed to a `Handler`/`HandlerOnce`
    /// parameter (`action`, `action_async`, `on_tap`, `gesture`, …), an
    /// `async` block or `async fn` body, `View::body`, or `GpuView::render`.
    /// `std::thread::sleep` reports under `thread_sleep_in_ui` instead.
    ///
    /// Only calls written directly in such a scope are flagged: a blocking
    /// call inside a plain `fn` the scope calls, or inside a nested
    /// non-handler closure (which may run on another thread, e.g.
    /// `waterui::task::spawn`'s worker), is out of scope.
    ///
    /// The path list is the default allowlist; a `dylint.toml`
    /// `[waterui-lints]` table may extend it through
    /// `blocking_in_ui_context_paths` — a list of def paths where `"a::b::c"`
    /// matches exactly and `"a::b::*"` matches everything under `a::b`.
    ///
    /// ### Why is this bad?
    ///
    /// Handlers, `async` continuations, `View::body`, and `GpuView::render`
    /// all run on the UI thread's local executor, which must never block:
    /// one blocking call stalls every view it drives. The sanctioned shapes
    /// are `.action_async`/`.task(..)` futures, `waterui::task::sleep`, and
    /// `waterui::task::spawn` for work that has to block.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// button("load").action(|| {
    ///     let data = std::fs::read_to_string("save.json");
    /// });
    /// ```
    ///
    /// Move the blocking call to a worker and await it instead:
    ///
    /// ```rust,ignore
    /// button("load").action_async(|| async {
    ///     let data = waterui::task::spawn(|| std::fs::read_to_string("save.json")).await;
    /// });
    /// ```
    pub BLOCKING_IN_UI_CONTEXT,
    suspicious,
    "a blocking call inside a handler, `async` scope, `View::body`, or `GpuView::render`"
}

/// `THREAD_SLEEP_IN_UI` shares this module with the lint above — one pass
/// recognizes both — so its declaration lives in a submodule to keep the two
/// `LINT_INFO`s apart.
pub(crate) mod thread_sleep {
    declare_waterui_lint! {
        /// ### What it does
        ///
        /// Flags `std::thread::sleep` inside a scope that runs on the UI
        /// thread's executor — a `Handler`/`HandlerOnce` closure, an `async`
        /// block or `async fn` body, `View::body`, or `GpuView::render` (the
        /// same contexts `blocking_in_ui_context` covers).
        ///
        /// ### Why is this bad?
        ///
        /// `sleep` parks the thread outright: no task, handler, or frame on
        /// the UI executor makes progress until it returns. It is also the
        /// most common wrong-sleep spelling — the executor's own timer is
        /// meant here.
        ///
        /// ### Example
        ///
        /// ```rust,ignore
        /// button("a").action(|| std::thread::sleep(Duration::from_millis(1)));
        /// ```
        ///
        /// Await the executor's timer inside an async handler instead:
        ///
        /// ```rust,ignore
        /// button("a").action_async(|| async {
        ///     waterui::task::sleep(Duration::from_millis(1)).await;
        /// });
        /// ```
        pub THREAD_SLEEP_IN_UI,
        correctness,
        "`std::thread::sleep` blocks the UI thread's executor"
    }
}

impl_lint_pass!(BlockingInUiContext => [
    BLOCKING_IN_UI_CONTEXT,
    thread_sleep::THREAD_SLEEP_IN_UI,
]);

const SLEEP_MSG: &str = "`std::thread::sleep` blocks the UI thread's executor";
const SLEEP_HELP: &str =
    "await `waterui::task::sleep(duration)` in an `.action_async`/`.task(..)` future instead";
const BLOCKING_HELP: &str = "move the blocking work to `waterui::task::spawn` (a worker thread) and await its handle, or use the async API";

/// The default blocking-call table — spec spellings mapped to the defining
/// crate's def path. `futures::executor::block_on` and
/// `futures_executor::block_on` resolve to the one `futures_executor` item,
/// and `smol::block_on` re-exports `async_io`'s; `reqwest` and the
/// executor crates resolve to nothing when the linted crate does not depend
/// on them.
const DEFAULT_BLOCKING_PATHS: &[&str] = &[
    "std::fs::*",
    "std::net::*",
    "std::process::Command::output",
    "std::process::Command::status",
    "std::process::Child::wait",
    "futures_executor::local_pool::block_on",
    "async_io::driver::block_on",
    "reqwest::blocking::*",
    "std::sync::poison::mutex::Mutex::lock",
    "std::sync::poison::rwlock::RwLock::read",
    "std::sync::poison::rwlock::RwLock::write",
];

/// Trait methods whose impl bodies are UI contexts.
const VIEW_BODY: &[&str] = &["waterui_core", "ui", "view", "View", "body"];
const GPU_RENDER: &[&str] = &[
    "waterui_graphics",
    "gpu",
    "gpu_surface",
    "GpuView",
    "render",
];

/// A def-path pattern: `a::b::c` matches that item exactly; a trailing `::*`
/// makes it a prefix (`std::net::*` covers `std::net::tcp::TcpStream::connect`).
struct PathPattern {
    segments: Vec<String>,
    prefix: bool,
}

impl PathPattern {
    fn parse(path: &str) -> Self {
        let (path, prefix) = match path.strip_suffix("::*") {
            Some(stripped) => (stripped, true),
            None => (path, false),
        };
        Self {
            segments: path.split("::").map(str::to_owned).collect(),
            prefix,
        }
    }

    /// Whether `def_path` — `LateContext::get_def_path` segments — matches.
    fn matches(&self, def_path: &[Symbol]) -> bool {
        let n = self.segments.len();
        (if self.prefix {
            def_path.len() >= n
        } else {
            def_path.len() == n
        }) && self
            .segments
            .iter()
            .map(String::as_str)
            .eq(def_path[..n].iter().map(Symbol::as_str))
    }
}

/// Which UI context a blocking call sits in — the name the diagnostic uses.
#[derive(Clone, Copy)]
enum Context {
    /// A closure in a `Handler`/`HandlerOnce` argument position.
    Handler,
    /// An `async` block or `async fn` body — the desugared coroutine covers
    /// both (`CoroutineSource::Block`/`Closure`/`Fn`).
    AsyncBlock,
    /// `impl View for _ :: body`.
    ViewBody,
    /// `impl GpuView for _ :: render`.
    GpuRender,
}

impl Context {
    fn name(self) -> &'static str {
        match self {
            Self::Handler => "handler",
            Self::AsyncBlock => "async block",
            Self::ViewBody => "View::body",
            Self::GpuRender => "GpuView::render",
        }
    }
}

/// The UI context `expr`'s enclosing scope is, if any. Walked with
/// `hir_parent_iter`: the first `Closure` decides — an `async` coroutine is
/// an async context, a closure in the `handlers` set is a handler context,
/// and any other closure is a scope boundary (a nested non-handler closure
/// may run off the UI thread — `task::spawn`'s argument is the sanctioned
/// shape). The first enclosing item decides between `View::body`/
/// `GpuView::render` and no context: a `fn` boundary ends the scope, so a
/// blocking call inside a helper a handler calls is out of scope.
/// A `#[waterui::test]`/`#[waterui::bench]` wrapper body is a harness
/// context, not a UI one — `thread_sleep_in_test` reports the sleep there.
fn ui_context(
    cx: &LateContext<'_>,
    handlers: &FxHashSet<LocalDefId>,
    test_wrappers: &mut FxHashMap<LocalDefId, bool>,
    expr: &Expr<'_>,
) -> Option<Context> {
    let owner = cx
        .tcx
        .typeck_root_def_id(cx.tcx.hir_enclosing_body_owner(expr.hir_id).into())
        .expect_local();
    if is_test_wrapper(cx, owner, test_wrappers) {
        return None;
    }
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        match node {
            Node::Expr(Expr {
                kind: ExprKind::Closure(closure),
                ..
            }) => match closure.kind {
                ClosureKind::Coroutine(CoroutineKind::Desugared(CoroutineDesugaring::Async, _)) => {
                    return Some(Context::AsyncBlock);
                }
                ClosureKind::Closure if handlers.contains(&closure.def_id) => {
                    return Some(Context::Handler);
                }
                _ => return None,
            },
            Node::Item(..) | Node::TraitItem(..) | Node::ImplItem(..) | Node::ForeignItem(..) => {
                let did = match node {
                    Node::Item(item) => item.owner_id,
                    Node::TraitItem(item) => item.owner_id,
                    Node::ImplItem(item) => item.owner_id,
                    _ => return None,
                };
                let trait_item = implemented_trait_item(cx.tcx, did.to_def_id());
                if def_path_eq(cx, trait_item, VIEW_BODY) {
                    return Some(Context::ViewBody);
                }
                if def_path_eq(cx, trait_item, GPU_RENDER) {
                    return Some(Context::GpuRender);
                }
                return None;
            }
            _ => {}
        }
    }
    None
}

pub(crate) struct BlockingInUiContext {
    /// `LocalDefId`s of closures seen in `Handler`/`HandlerOnce` argument
    /// positions. `check_expr` on the enclosing call runs before the
    /// closure's body exprs (the late walk is preorder), so a blocking call
    /// can ask whether an enclosing closure is one of these.
    handlers: FxHashSet<LocalDefId>,
    /// `is_test_wrapper` verdicts per enclosing fn `LocalDefId`.
    test_wrappers: FxHashMap<LocalDefId, bool>,
    sleep: PathPattern,
    blocking: Vec<PathPattern>,
}

impl Default for BlockingInUiContext {
    fn default() -> Self {
        let config = crate::config::config();
        Self {
            handlers: FxHashSet::default(),
            test_wrappers: FxHashMap::default(),
            sleep: PathPattern::parse(SLEEP_PATH),
            blocking: DEFAULT_BLOCKING_PATHS
                .iter()
                .copied()
                .chain(
                    config
                        .blocking_in_ui_context_paths
                        .iter()
                        .map(String::as_str),
                )
                .map(PathPattern::parse)
                .collect(),
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for BlockingInUiContext {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        self.handlers.extend(
            handler_closures(cx, expr)
                .into_iter()
                .map(|closure| closure.def_id),
        );
        let Some(callee) =
            call_def_id(cx.typeck_results(), expr).map(|did| implemented_trait_item(cx.tcx, did))
        else {
            return;
        };
        let def_path = cx.get_def_path(callee);
        let sleep = self.sleep.matches(&def_path);
        if !sleep && !self.blocking.iter().any(|path| path.matches(&def_path)) {
            return;
        }
        let Some(context) = ui_context(cx, &self.handlers, &mut self.test_wrappers, expr) else {
            return;
        };
        if sleep {
            span_lint_and_help(
                cx,
                thread_sleep::THREAD_SLEEP_IN_UI,
                expr.span,
                SLEEP_MSG,
                None,
                SLEEP_HELP,
            );
        } else {
            span_lint_and_help(
                cx,
                BLOCKING_IN_UI_CONTEXT,
                expr.span,
                format!("this call blocks inside a UI context ({})", context.name()),
                None,
                BLOCKING_HELP,
            );
        }
    }
}
