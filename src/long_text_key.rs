//! `long_text_key` — a `text!`/`Text::localized` key longer than
//! `long_text_key_words` words is prose parked in the key position.

use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::macros::root_macro_call;
use clippy_utils::source::snippet_opt;
use rustc_ast::LitKind;
use rustc_data_structures::fx::FxHashSet;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, Symbol};

use crate::def_path::def_path_eq;
use crate::format_args::parse_text_call;
use crate::param_bounds::call_def_id;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a localization key longer than `long_text_key_words` words
    /// (default 12): the `text!("..")` format literal and the string literal
    /// passed to `Text::localized(..)`/`Text::localized_or(..)`.
    ///
    /// The limit is configurable through `dylint.toml`'s `[waterui-lints]`
    /// table — `long_text_key_words = 8`.
    ///
    /// ### Why is this bad?
    ///
    /// The whole literal is the catalog lookup key. Prose in the key
    /// position is duplicated into every `i18n/<locale>.toml` entry, and any
    /// copy edit renames the key — every translation silently falls back to
    /// the new, untranslated string.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text!("This is a long sentence that has more than twelve words in it for sure")
    /// ```
    ///
    /// Use a short catalog key and put the prose in `i18n/en.toml` instead:
    ///
    /// ```rust,ignore
    /// text!("$about_blurb")
    /// ```
    pub LONG_TEXT_KEY,
    pedantic,
    "a text key longer than `long_text_key_words` words belongs in the catalog"
}

const HELP: &str =
    "use a short catalog key such as `\"$about_blurb\"` and put the prose in `i18n/<locale>.toml`";

/// `waterui_text::text::Text::{localized, localized_or}` — the key is the
/// first argument.
const LOCALIZED: &[&str] = &["waterui_text", "text", "Text", "localized"];
const LOCALIZED_OR: &[&str] = &["waterui_text", "text", "Text", "localized_or"];

/// `Text::localized_with` — the call every `text!` expansion lowers to. In
/// the linted crate's HIR it is written only by the proc macro: users call
/// `text!`, `localized`, or `localized_or` instead.
const LOCALIZED_WITH: &[&str] = &["waterui_text", "text", "Text", "localized_with"];

/// `waterui_macros::text` — the `text!` proc macro's def path.
const TEXT_MACRO: &[&str] = &["waterui_macros", "text"];

/// Default `long_text_key_words`.
const DEFAULT_WORD_LIMIT: usize = 12;

pub(crate) struct LongTextKey {
    /// `long_text_key_words`, resolved once.
    limit: usize,
    /// `text!` call-site spans already reported — one diagnostic per
    /// invocation.
    reported: FxHashSet<Span>,
}

impl Default for LongTextKey {
    fn default() -> Self {
        let config = crate::config::config();
        Self {
            limit: config.long_text_key_words.unwrap_or(DEFAULT_WORD_LIMIT),
            reported: FxHashSet::default(),
        }
    }
}

impl_lint_pass!(LongTextKey => [LONG_TEXT_KEY]);

impl LongTextKey {
    /// Emit the diagnostic when `key`'s word count exceeds the limit.
    fn report(&self, cx: &LateContext<'_>, span: Span, key: &str) {
        let words = key.split_whitespace().count();
        if words <= self.limit {
            return;
        }
        span_lint_and_help(
            cx,
            LONG_TEXT_KEY,
            span,
            format!(
                "this text key is {words} words; keys longer than {} words break every translation on a copy edit",
                self.limit
            ),
            None,
            HELP,
        );
    }
}

/// `arg` peeled of drop-temps, when it is a user-written string literal —
/// returns its cooked text.
fn str_lit<'hir>(arg: &'hir Expr<'hir>) -> Option<Symbol> {
    let mut expr = arg;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    match expr.kind {
        ExprKind::Lit(lit) if !expr.span.from_expansion() => match lit.node {
            LitKind::Str(key, _) => Some(key),
            _ => None,
        },
        _ => None,
    }
}

impl<'tcx> LateLintPass<'tcx> for LongTextKey {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let ExprKind::Call(_, args) = expr.kind else {
            return;
        };
        let Some(callee) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if def_path_eq(cx, callee, LOCALIZED) || def_path_eq(cx, callee, LOCALIZED_OR) {
            if let Some(key) = args.first().and_then(|arg| str_lit(arg)) {
                self.report(cx, expr.span, key.as_str());
            }
            return;
        }
        if def_path_eq(cx, callee, LOCALIZED_WITH)
            && let Some(macro_call) = root_macro_call(expr.span)
            && def_path_eq(cx, macro_call.def_id, TEXT_MACRO)
            && self.reported.insert(macro_call.span)
        {
            // The `text!` key survives lowering only inside `format_args!`
            // bytecode — read it back from the call-site source, where it is
            // the invocation's first string literal.
            if let Some(key) = snippet_opt(cx.sess(), macro_call.span)
                .as_deref()
                .and_then(parse_text_call)
                .map(|call| call.literal)
            {
                self.report(cx, macro_call.span, &key);
            }
        }
    }
}
