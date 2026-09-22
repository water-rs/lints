//! `long_text_key` — a `text!`/`Text::localized` key longer than
//! `long_text_key_words` words is prose parked in the key position.

use rustc_hir::Expr;
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::impl_lint_pass;
use rustc_span::Span;

use crate::diagnostics::span_lint_and_help;
use crate::text_key::{self, TextKeys};

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

/// Default `long_text_key_words`.
const DEFAULT_WORD_LIMIT: usize = 12;

pub(crate) struct LongTextKey {
    /// `long_text_key_words`, resolved once.
    limit: usize,
    /// `text!` keys collected by the early pass, keyed by macro call span.
    text_keys: TextKeys,
}

impl LongTextKey {
    pub(crate) fn new(text_keys: TextKeys) -> Self {
        let config = crate::config::config();
        Self {
            limit: config.long_text_key_words.unwrap_or(DEFAULT_WORD_LIMIT),
            text_keys,
        }
    }

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

impl_lint_pass!(LongTextKey => [LONG_TEXT_KEY]);

impl<'tcx> LateLintPass<'tcx> for LongTextKey {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if let Some(key) = text_key::localized_key(cx, &self.text_keys, expr) {
            self.report(cx, key.span, key.text.as_str());
        }
    }
}
