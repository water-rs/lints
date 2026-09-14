use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::ty::implements_trait;
use rustc_hir::Expr;
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::print::with_forced_trimmed_paths;
use rustc_session::declare_lint_pass;

use crate::watch::watch_call;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `watch(signal, f)` and `Dynamic::watch(signal, f)` calls whose
    /// watched value's type implements `nami::collection::Collection` — a
    /// `Vec`, an array, a `reactive::collection::List`, a `SignalCollection`,
    /// or any other observable collection — whatever shape `f` has.
    ///
    /// ### Why is this bad?
    ///
    /// `watch` replaces its entire subtree every time the signal changes, so
    /// over a collection a single-row edit rebuilds every row and can escalate
    /// to a full-window rebuild. Reactive collections exist so that membership
    /// changes update only the affected rows.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// watch(items, |items| vstack((..)))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// Lazy::for_each(list, |item| ..)
    /// ```
    pub WATCH_OVER_COLLECTION,
    suspicious,
    "a `watch` over a collection rebuilds every row on each change"
}

declare_lint_pass!(WatchOverCollection => [WATCH_OVER_COLLECTION]);

/// `nami_core::collection::Collection` — `Vec`, `&'static [T]`, `[T; N]`,
/// `Rc<[T]>`, `Box`/`Rc` of a collection, `nami::collection::List`, and
/// `SignalCollection` all implement it, so one trait check covers every
/// collection shape.
const COLLECTION: &str = "nami_core::collection::Collection";

const MESSAGE: &str = "this `watch` rebuilds every row whenever the collection changes";
const HELP: &str = "render a changing set of views over a reactive collection: `Lazy::for_each(list, |item| ..)`, `List::for_each`, or `VStack::for_each` with `reactive::collection::List<T>` (items `Identifiable`), or `SignalCollection::new(signal)` for a set derived from a signal";

impl<'tcx> LateLintPass<'tcx> for WatchOverCollection {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let Some(call) = watch_call(cx, expr) else {
            return;
        };
        let watches_collection = lookup_path_str(cx.tcx, PathNS::Type, COLLECTION)
            .into_iter()
            .any(|trait_did| implements_trait(cx, call.value_ty, trait_did, &[]));
        if !watches_collection {
            return;
        }
        span_lint_and_then(cx, WATCH_OVER_COLLECTION, expr.span, MESSAGE, |diag| {
            diag.span_label(
                call.signal.span,
                with_forced_trimmed_paths!(format!(
                    "`{}` is a collection — `watch` replaces the whole subtree on every change",
                    call.value_ty
                )),
            );
            diag.help(HELP);
        });
    }
}
