use clippy_utils::diagnostics::span_lint_and_sugg;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::applicability::comment_guard;
use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `.foreground(MutedForeground)` — the muted-foreground theme
    /// token passed to the foreground modifier by hand, under any spelling
    /// (`MutedForeground`, `theme_color::MutedForeground`,
    /// `waterui::theme::color::MutedForeground`).
    ///
    /// ### Why is this bad?
    ///
    /// `ViewExt::muted` is that exact call — `self.foreground(MutedForeground)`
    /// — so spelling the token out names the plumbing instead of the intent.
    /// Any other argument, `Foreground` included, stays silent.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("a").foreground(MutedForeground)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("a").muted()
    /// ```
    pub MANUAL_MUTED_FOREGROUND,
    style,
    "a hand-written `.foreground(MutedForeground)` where `.muted()` is the named modifier"
}

declare_lint_pass!(ManualMutedForeground => [MANUAL_MUTED_FOREGROUND]);

/// `waterui_internal::view::ViewExt::foreground` — the call the lint rewrites.
const FOREGROUND: &[&str] = &["waterui_internal", "view", "ViewExt", "foreground"];

/// `waterui_internal::theme::color::MutedForeground` — the token `muted()`
/// installs.
const MUTED_FOREGROUND: &[&str] = &["waterui_internal", "theme", "color", "MutedForeground"];

/// Whether `arg` is a path naming the `MutedForeground` token — a unit
/// struct, so value-position resolution lands on its constant ctor; the
/// ctor's parent is the struct.
fn is_muted_foreground(cx: &LateContext<'_>, arg: &Expr<'_>) -> bool {
    let ExprKind::Path(qpath) = arg.kind else {
        return false;
    };
    let Res::Def(kind, did) = cx.typeck_results().qpath_res(&qpath, arg.hir_id) else {
        return false;
    };
    let did = if matches!(kind, DefKind::Ctor(..)) {
        cx.tcx.parent(did)
    } else {
        did
    };
    def_path_eq(cx, did, MUTED_FOREGROUND)
}

impl<'tcx> LateLintPass<'tcx> for ManualMutedForeground {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(segment, receiver, [arg], _) = expr.kind else {
            return;
        };
        if segment.ident.name.as_str() != "foreground" {
            return;
        }
        let Some(callee) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if !def_path_eq(cx, implemented_trait_item(cx.tcx, callee), FOREGROUND)
            || !is_muted_foreground(cx, arg)
        {
            return;
        }
        // Replace `.foreground(..)` to the closing paren — the receiver and
        // the chain on either side stay untouched. A `macro_rules!` receiver
        // (`header!().foreground(..)`) keeps definition-site positions, so
        // walk to the call site: the suggestion's `lo` is the `.` the user
        // wrote, never a byte inside the macro definition.
        let mut lo = receiver.span;
        while lo.from_expansion() {
            lo = lo.source_callsite();
        }
        let span = expr.span.with_lo(lo.hi());
        let mut applicability = Applicability::MachineApplicable;
        comment_guard(cx, span, &mut applicability);
        span_lint_and_sugg(
            cx,
            MANUAL_MUTED_FOREGROUND,
            span,
            "`.foreground(MutedForeground)` restates `.muted()`",
            "use `.muted()`",
            ".muted()".to_string(),
            applicability,
        );
    }
}
