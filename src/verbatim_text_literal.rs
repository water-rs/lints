//! `verbatim_text_literal` — a string literal reaches a text position
//! through a verbatim wrapper and silently leaves the localization
//! pipeline.

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::source::snippet_opt;
use rustc_ast::LitKind;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::declare_lint_pass;

use crate::carriers::{FROM, INTO, TEXT_VERBATIM, TO_OWNED, TO_STRING, is_string_ty};
use crate::def_path::def_path_eq;
use crate::param_bounds::{
    TEXT_PARAM_BOUNDS, call_arg_bounds, call_args, call_def_id, implemented_trait_item,
};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a string literal that reaches an `IntoText`/`IntoLabel`
    /// parameter through a verbatim wrapper: `Str::from_static("lit")`,
    /// `"lit".to_string()`, `"lit".to_owned()`, `String::from("lit")`,
    /// `Str::from("lit")`, `"lit".into()` to `String`/`Str`, or
    /// `Text::verbatim("lit")`.
    ///
    /// ### Why is this bad?
    ///
    /// `&'static str` in a text position is `Text::localized` — a runtime
    /// catalog lookup — while `String`/`Str` is `Text::verbatim` and never
    /// translates. A literal wrapped this way silently leaves the
    /// localization pipeline while looking identical on screen in the
    /// author's locale.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text(String::from("Save"))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("Save")
    /// ```
    pub VERBATIM_TEXT_LITERAL,
    suspicious,
    "a string literal reaches a text position through the verbatim path and skips localization"
}

const MESSAGE: &str =
    "this literal reaches the text position as verbatim text and skips localization";
const SUGGESTION: &str = "pass the literal itself — `&'static str` is looked up in the catalog";

/// `Str::from_static` — stores the literal verbatim without allocation.
const FROM_STATIC: &[&str] = &["waterui_str", "Str", "from_static"];

declare_lint_pass!(VerbatimTextLiteral => [VERBATIM_TEXT_LITERAL]);

/// The literal `arg` wraps in one of the verbatim shapes, after
/// parens/drop-temps are peeled — `None` for anything else, including
/// expansion-produced code.
fn verbatim_literal<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    arg: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    let mut expr = arg;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    if expr.span.from_expansion() {
        return None;
    }
    let wrapped = match expr.kind {
        ExprKind::Call(_, [inner]) => inner,
        ExprKind::MethodCall(_, receiver, [], _) => receiver,
        _ => return None,
    };
    let did = implemented_trait_item(cx.tcx, call_def_id(typeck, expr)?);
    let wraps_verbatim = def_path_eq(cx, did, FROM_STATIC)
        || def_path_eq(cx, did, TEXT_VERBATIM)
        || def_path_eq(cx, did, TO_STRING)
        || def_path_eq(cx, did, TO_OWNED)
        || ((def_path_eq(cx, did, FROM) || def_path_eq(cx, did, INTO))
            && is_string_ty(cx, typeck.expr_ty(expr)));
    if !wraps_verbatim {
        return None;
    }
    let mut literal = wrapped;
    while let ExprKind::DropTemps(inner) = literal.kind {
        literal = inner;
    }
    match literal.kind {
        ExprKind::Lit(lit) if matches!(lit.node, LitKind::Str(..)) => {
            (!literal.span.from_expansion()).then_some(literal)
        }
        _ => None,
    }
}

impl<'tcx> LateLintPass<'tcx> for VerbatimTextLiteral {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
            return;
        }
        let Some((_, per_arg)) = call_arg_bounds(cx, expr, &[TEXT_PARAM_BOUNDS]) else {
            return;
        };
        let typeck = cx.typeck_results();
        for (arg, bounds) in call_args(expr).into_iter().zip(per_arg) {
            if bounds.is_empty() {
                continue;
            }
            let Some(literal) = verbatim_literal(cx, typeck, arg) else {
                continue;
            };
            span_lint_and_then(cx, VERBATIM_TEXT_LITERAL, arg.span, MESSAGE, |diag| {
                if let Some(snippet) = snippet_opt(cx, literal.span) {
                    diag.span_suggestion(
                        arg.span,
                        SUGGESTION,
                        snippet,
                        Applicability::MachineApplicable,
                    );
                } else {
                    diag.help(SUGGESTION);
                }
            });
        }
    }
}
