//! Suggestion-applicability helpers shared by lints whose fixes delete or
//! rewrite a source span.

use clippy_utils::span_contains_comment;
use rustc_errors::Applicability;
use rustc_lint::LateContext;
use rustc_span::Span;

/// A comment inside the span a suggestion replaces would be deleted by the
/// fix — downgrade to `MaybeIncorrect`.
pub(crate) fn comment_guard(cx: &LateContext<'_>, span: Span, applicability: &mut Applicability) {
    if *applicability == Applicability::MachineApplicable && span_contains_comment(cx, span) {
        *applicability = Applicability::MaybeIncorrect;
    }
}
