use std::ops::ControlFlow;

use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr_without_closures};
use rustc_data_structures::fx::FxHashSet;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{CRATE_HIR_ID, Expr, ExprKind, Item, ItemKind, Node};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter::All;
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};
use rustc_session::impl_lint_pass;
use rustc_span::sym;

use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::call_def_id;
use crate::signature::normalized_inputs;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `waterui::task::spawn_local` — and `waterui::task::spawn` of a
    /// `!Send` future — written in the body of the app entry function,
    /// `fn app(Environment) -> App` (the function `waterui_ffi::export!`
    /// invokes), or in a function that body calls directly in the same crate.
    /// A spawn inside a closure is exempt: `.task(..)` futures, `.on_appear`,
    /// and handler bodies run after mount, where the executor already exists.
    /// A function the entry reaches only through such a closure, or through
    /// another function, is likewise out of scope — the check is one level of
    /// direct calls.
    ///
    /// ### Why is this bad?
    ///
    /// Every runner installs the thread-local executor inside its mount,
    /// after `app(env)` returns, so a `spawn_local` reached while the app or
    /// its stores are still being built panics with "Local executor not set".
    /// The call compiles and survives runners that never mount it, so the
    /// panic lands only on the paths that do.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// pub fn app(env: Environment) -> App {
    ///     spawn_local(async { warm_cache().await }).detach();
    ///     App::new(root, env)
    /// }
    /// ```
    ///
    /// Move startup async work into `.task(..)` instead:
    ///
    /// ```rust,ignore
    /// App::new(|| root().task(async { warm_cache().await }), env)
    /// ```
    pub SPAWN_LOCAL_IN_APP,
    correctness,
    "`spawn_local` reached during app construction panics — the local executor is not installed yet"
}

impl_lint_pass!(SpawnLocalInApp => [SPAWN_LOCAL_IN_APP]);

/// `executor_core::std_on::{spawn, spawn_local}` — the defining-crate paths
/// behind `waterui::task::{spawn, spawn_local}` (the functions live in a
/// private `mod std_on` re-exported flat, so `get_def_path` spells them
/// `executor_core::std_on::*`).
const SPAWN: &[&str] = &["executor_core", "std_on", "spawn"];
const SPAWN_LOCAL: &[&str] = &["executor_core", "std_on", "spawn_local"];

/// The app entry's signature endpoints — `fn app(Environment) -> App`. Both
/// names are facade re-exports, so the check is on the defining-crate path:
/// `Environment` lives in `waterui_core`, `App` in `waterui_internal` (whose
/// `runtime` module the `waterui` facade re-exports).
const ENVIRONMENT: &[&str] = &["waterui_core", "foundation", "env", "Environment"];
const APP_PATHS: &[&[&str]] = &[
    &["waterui_internal", "runtime", "app", "App"],
    &["waterui", "runtime", "app", "App"],
];

const LOCAL_MESSAGE: &str = "this `spawn_local` runs while the app is being constructed, before the runner installs the local executor";
const SPAWN_MESSAGE: &str = "this `spawn` of a `!Send` future runs while the app is being constructed, before the runner installs the local executor";
const CALL_LABEL: &str = "panics with `Local executor not set` when it runs";
const HELP: &str =
    "move startup async work into `.task(..)` or `.on_appear` on a view, or into a handler body";

/// `ty`'s `Adt` def path equals `path` — aliases normalized away by the
/// caller.
fn adt_eq(cx: &LateContext<'_>, ty: Ty<'_>, path: &[&'static str]) -> bool {
    matches!(*ty.kind(), TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), path))
}

/// `did`'s return type, normalized the same way
/// [`normalized_inputs`] normalizes the parameters.
fn normalized_output<'tcx>(cx: &LateContext<'tcx>, did: DefId) -> Ty<'tcx> {
    let output = cx
        .tcx
        .fn_sig(did)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder()
        .output();
    cx.tcx
        .try_normalize_erasing_regions(
            ty::TypingEnv::post_analysis(cx.tcx, did),
            ty::Unnormalized::new_wip(output),
        )
        .unwrap_or(output)
}

/// Whether `item` is the app entry — `fn app(Environment) -> App`, the
/// function `waterui_ffi::export!`'s generated `waterui_app` calls by name.
/// The name plus both signature endpoints are matched, so an unrelated
/// `fn app` of another shape stays out of scope.
fn is_app_entry(cx: &LateContext<'_>, item: &Item<'_>) -> bool {
    let ItemKind::Fn { ident, .. } = item.kind else {
        return false;
    };
    if ident.name.as_str() != "app" {
        return false;
    }
    let did = item.owner_id.def_id.to_def_id();
    let [(_, input)] = normalized_inputs(cx, did)[..] else {
        return false;
    };
    let output = normalized_output(cx, did);
    adt_eq(cx, input, ENVIRONMENT) && APP_PATHS.iter().any(|path| adt_eq(cx, output, path))
}

/// Whether `call` is a `spawn` of a `!Send` future — `spawn_local` covers
/// every future; `spawn` asks `Send`, so a `!Send` argument would still need
/// the local executor.
fn spawn_of_non_send(cx: &LateContext<'_>, call: &Expr<'_>) -> bool {
    let ExprKind::Call(_, args) = call.kind else {
        return false;
    };
    let Some(send) = cx.tcx.get_diagnostic_item(sym::Send) else {
        return false;
    };
    let ty = cx.typeck_results().expr_ty_adjusted(&args[0]);
    !implements_trait(cx, ty, send, &[])
}

#[derive(Default)]
pub(crate) struct SpawnLocalInApp {
    /// Body owners a `spawn_local` is flagged in: the app entry function's
    /// `LocalDefId` plus the local callees its body reaches through direct
    /// calls. Populated in `check_crate`, before any `check_expr`.
    construction: FxHashSet<LocalDefId>,
}

/// Finds the crate's app entry functions in `check_crate` and records them
/// plus their direct local callees in `construction`.
struct EntrySweep<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    construction: &'a mut FxHashSet<LocalDefId>,
}

impl<'tcx> Visitor<'tcx> for EntrySweep<'_, 'tcx> {
    type MaybeTyCtxt = TyCtxt<'tcx>;
    type NestedFilter = All;

    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.cx.tcx
    }

    fn visit_item(&mut self, item: &'tcx Item<'tcx>) {
        if is_app_entry(self.cx, item) {
            let owner = item.owner_id.def_id;
            self.construction.insert(owner);
            let typeck = self.cx.tcx.typeck(owner);
            for_each_expr_without_closures(self.cx.tcx.hir_body_owned_by(owner), |expr| {
                if matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
                    && let Some(local) = call_def_id(typeck, expr).and_then(DefId::as_local)
                {
                    self.construction.insert(local);
                }
                ControlFlow::<(), Descend>::Continue(Descend::Yes)
            });
        }
        intravisit::walk_item(self, item);
    }
}

impl<'tcx> LateLintPass<'tcx> for SpawnLocalInApp {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        let Node::Crate(module) = cx.tcx.hir_node(CRATE_HIR_ID) else {
            return;
        };
        intravisit::walk_mod(
            &mut EntrySweep {
                cx,
                construction: &mut self.construction,
            },
            module,
        );
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() || !matches!(expr.kind, ExprKind::Call(..)) {
            return;
        }
        let Some(callee) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        let local = def_path_eq(cx, callee, SPAWN_LOCAL);
        if !local && !(def_path_eq(cx, callee, SPAWN) && spawn_of_non_send(cx, expr)) {
            return;
        }
        let owner = cx.tcx.hir_enclosing_body_owner(expr.hir_id);
        if !self.construction.contains(&owner) {
            return;
        }
        span_lint_and_then(
            cx,
            SPAWN_LOCAL_IN_APP,
            expr.span,
            if local { LOCAL_MESSAGE } else { SPAWN_MESSAGE },
            |diag| {
                diag.span_label(expr.span, CALL_LABEL);
                diag.help(HELP);
            },
        );
    }
}
