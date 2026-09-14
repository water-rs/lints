//! The early pass that keeps `format!(..)`'s AST [`FormatArgs`] alive for late
//! passes — HIR lowers `format_args!` into a desugared call that no longer
//! carries the literal's placeholders. Shared by every lint that rebuilds a
//! `format!` template (`signal_get_in_view`, `manual_text_map`).

use clippy_utils::macros::FormatArgsStorage;
use clippy_utils::source::SpanRangeExt;
use rustc_ast::{Crate as AstCrate, Expr as AstExpr, ExprKind as AstExprKind, FormatArgs};
use rustc_data_structures::fx::FxHashMap;
use rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};
use rustc_lint::{EarlyContext, EarlyLintPass};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, hygiene};
use std::mem;

/// Early pass mirroring clippy's `FormatArgsCollector`: snapshots the AST
/// `FormatArgs` nodes so late passes can rebuild a `format!(..)` argument as a
/// `text!` invocation — the desugared HIR no longer carries the literal's
/// placeholders.
pub(crate) struct FormatArgsCollector {
    format_args: FxHashMap<Span, FormatArgs>,
    storage: FormatArgsStorage,
}

impl FormatArgsCollector {
    pub(crate) fn new(storage: FormatArgsStorage) -> Self {
        Self {
            format_args: FxHashMap::default(),
            storage,
        }
    }
}

impl_lint_pass!(FormatArgsCollector => []);

impl EarlyLintPass for FormatArgsCollector {
    fn check_expr(&mut self, cx: &EarlyContext<'_>, expr: &AstExpr) {
        if let AstExprKind::FormatArgs(args) = &expr.kind {
            if has_span_from_proc_macro(cx, args) {
                return;
            }

            self.format_args
                .insert(expr.span.with_parent(None), (**args).clone());
        }
    }

    fn check_crate_post(&mut self, _: &EarlyContext<'_>, _: &AstCrate) {
        self.storage.set(mem::take(&mut self.format_args));
    }
}

/// Detects if the format string or an argument has its span set by a proc
/// macro to something inside a macro callsite (clippy's guard of the same
/// name — e.g. `println!(some_proc_macro!("input {}"), a)`).
fn has_span_from_proc_macro(cx: &EarlyContext<'_>, args: &FormatArgs) -> bool {
    let ctxt = args.span.ctxt();

    let mut spans = std::iter::once(args.span).chain(
        args.arguments
            .explicit_args()
            .iter()
            .map(|argument| hygiene::walk_chain(argument.expr.span, ctxt)),
    );
    let Some(mut start) = spans.next() else {
        return false;
    };
    for end in spans {
        if !start
            .between(end)
            .check_source_text(cx, between_args_is_comma_name)
        {
            return true;
        }
        start = end;
    }
    false
}

/// The source between two consecutive `format!` inputs must be `, name` or
/// `, name =` — anything else means the spans were mapped by a proc macro.
fn between_args_is_comma_name(src: &str) -> bool {
    let mut tokens = tokenize(src, FrontmatterAllowed::No).filter(|token| {
        !matches!(
            token.kind,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } | TokenKind::Whitespace
        )
    });
    tokens
        .next()
        .is_some_and(|t| matches!(t.kind, TokenKind::Comma))
        && tokens.all(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Eq))
}
