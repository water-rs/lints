//! `manual_computed` — `Computed::new(signal)` and the `From`/`Into`/
//! `IntoComputed` spellings of the same conversion restate
//! `SignalExt::computed()`.

use clippy_utils::diagnostics::{span_lint, span_lint_and_sugg};
use clippy_utils::is_expr_temporary_value;
use clippy_utils::sugg::Sugg;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::declare_lint_pass;

use crate::carriers::{CLONE, FROM, INTO};
use crate::computed::{COMPUTED_NEW, INTO_COMPUTED, erases_signal};
use crate::def_path::def_path_eq;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags the hand-written spellings of a `Computed` erasure:
    /// `Computed::new(e)`, `Computed::from(e)`/`From::from(e)`,
    /// `e.into()`/`Into::into(e)` whose type is `Computed<T>`, and
    /// `e.into_computed()` — wherever `e` is a signal whose `Signal::Output`
    /// is that `T`. An `e` that is `s.clone()` over a place `s` rewrites to
    /// `s.computed()`.
    ///
    /// ### Why is this bad?
    ///
    /// `SignalExt::computed` is the erasure's own name: `total.computed()`
    /// reads as what it is and chains, while the constructor and
    /// conversion-trait spellings route through the plumbing. The
    /// `IntoComputed::into_computed` bound can also convert the output
    /// (`Output: From<C::Output>`) — those calls stay silent, as do
    /// `Computed::constant(x)` and `constant(x)`, which have no signal to
    /// call `computed()` on.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let total: Computed<i32> = count.clone().into();
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let total: Computed<i32> = count.computed();
    /// ```
    pub MANUAL_COMPUTED,
    style,
    "a `Computed` erasure written out where `SignalExt::computed` is the method form"
}

declare_lint_pass!(ManualComputed => [MANUAL_COMPUTED]);

const MESSAGE: &str = "this erasure is `SignalExt::computed`";
const HELP: &str = "use the method form";

/// Callees that hand-spell the `computed()` erasure: `Computed::new(e)`,
/// `Computed::from(e)`/`From::from(e)`, `e.into()`/`Into::into(e)`, and
/// `e.into_computed()`. In every shape `call_args(call)[0]` is the signal —
/// the receiver for the method calls, the parameter for the associated
/// functions.
const ERASURES: &[&[&str]] = &[COMPUTED_NEW, FROM, INTO, INTO_COMPUTED];

/// `e` is `s.clone()` — `Clone::clone` in either spelling — over a place
/// `s`: the clone exists only to hand the erasure an owned handle, which
/// `s.computed()` produces itself. A non-place receiver is a real
/// computation (`make().clone()`); `needless_signal_clone` owns that fix.
fn cloned_place<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    e: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    let mut e = e;
    while let ExprKind::DropTemps(inner) = e.kind {
        e = inner;
    }
    let did = call_def_id(typeck, e)?;
    if !def_path_eq(cx, implemented_trait_item(cx.tcx, did), CLONE) {
        return None;
    }
    let s = *call_args(e).first()?;
    (!is_expr_temporary_value(cx, s)).then_some(s)
}

impl<'tcx> LateLintPass<'tcx> for ManualComputed {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        let typeck = cx.typeck_results();
        let Some(did) = call_def_id(typeck, expr) else {
            return;
        };
        if !ERASURES
            .iter()
            .any(|path| def_path_eq(cx, implemented_trait_item(cx.tcx, did), path))
        {
            return;
        }
        let Some(&signal) = call_args(expr).first() else {
            return;
        };
        if signal.span.from_expansion() {
            return;
        }
        // `Computed::new` and `From<Binding<T>>` preserve `T` by signature;
        // `IntoComputed<Output>` is allowed to convert (`Output:
        // From<C::Output>`), which is what this check rejects.
        if !erases_signal(cx, typeck, expr, signal) {
            return;
        }
        let receiver = cloned_place(cx, typeck, signal).unwrap_or(signal);
        let Some(sugg) = Sugg::hir_opt(cx, receiver) else {
            span_lint(cx, MANUAL_COMPUTED, expr.span, MESSAGE);
            return;
        };
        span_lint_and_sugg(
            cx,
            MANUAL_COMPUTED,
            expr.span,
            MESSAGE,
            HELP,
            format!("{}.computed()", sugg.maybe_paren()),
            Applicability::MachineApplicable,
        );
    }
}
