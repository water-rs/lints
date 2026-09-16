use clippy_utils::diagnostics::span_lint_and_then;
use rustc_ast::LitKind;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::def_path::def_path_eq;
use crate::param_bounds::{
    LABEL_PARAM_BOUNDS, call_arg_bounds, call_args, call_def_id, implemented_trait_item,
};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags an empty or whitespace-only string literal passed to a
    /// parameter bound by `IntoLabel` — `button("")`, `toggle("", &flag)`,
    /// `field("", &name)`, … — or to `.a11y_label("")`.
    ///
    /// ### Why is this bad?
    ///
    /// The label parameter is mandatory by type precisely so screen readers
    /// and `waterui-testing` queries can find the control; `""` satisfies
    /// the type and announces nothing. To hide a label visually the
    /// documented tools are `Label::new(name, icon).icon_only()`,
    /// `.hide_label()`, and `LabelDisplayMode::IconOnly`.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// button("")
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// button("Save")
    /// ```
    pub EMPTY_LABEL_LITERAL,
    a11y,
    "an empty string literal passed as a control's mandatory label"
}

const MESSAGE: &str = "an empty label defeats the mandatory label";
const LABEL: &str = "screen readers and `waterui-testing` queries find the control by this label";
const HELP: &str = "give the control its real name; to hide it visually use `Label::new(name, icon).icon_only()` / `.hide_label()` or `LabelDisplayMode::IconOnly`";

/// `waterui_internal::view::ViewExt::a11y_label` — its parameter is
/// `impl IntoComputed<Str>`, not `IntoLabel`, so the position is matched by
/// def path rather than by parameter bound.
const A11Y_LABEL: &[&str] = &["waterui_internal", "view", "ViewExt", "a11y_label"];

declare_lint_pass!(EmptyLabelLiteral => [EMPTY_LABEL_LITERAL]);

/// `arg` with drop-temps peeled (parentheses do not survive into HIR), when
/// it is a non-expansion string literal whose trimmed text is empty.
fn empty_literal<'hir>(arg: &'hir Expr<'hir>) -> Option<&'hir Expr<'hir>> {
    let mut expr = arg;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    let ExprKind::Lit(lit) = expr.kind else {
        return None;
    };
    match lit.node {
        LitKind::Str(sym, _) if sym.as_str().trim().is_empty() => {
            (!expr.span.from_expansion()).then_some(expr)
        }
        _ => None,
    }
}

fn report(cx: &LateContext<'_>, arg: &Expr<'_>) {
    if empty_literal(arg).is_some() {
        span_lint_and_then(cx, EMPTY_LABEL_LITERAL, arg.span, MESSAGE, |diag| {
            diag.span_label(arg.span, LABEL);
            diag.help(HELP);
        });
    }
}

impl<'tcx> LateLintPass<'tcx> for EmptyLabelLiteral {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
            return;
        }
        if let Some((_, per_arg)) = call_arg_bounds(cx, expr, &[LABEL_PARAM_BOUNDS]) {
            for (arg, bounds) in call_args(expr).into_iter().zip(per_arg) {
                if !bounds.is_empty() {
                    report(cx, arg);
                }
            }
        }
        if let ExprKind::MethodCall(_, _, args, _) = expr.kind
            && let Some(did) = call_def_id(cx.typeck_results(), expr)
            && def_path_eq(cx, implemented_trait_item(cx.tcx, did), A11Y_LABEL)
        {
            for arg in args {
                report(cx, arg);
            }
        }
    }
}
