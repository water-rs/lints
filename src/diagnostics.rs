//! The diagnostic emitters every lint reports through: the
//! `clippy_utils::diagnostics` functions of the same names and signatures,
//! behind one gate — a finding whose primary span lies in the linted crate's
//! build-script output is dropped.
//!
//! A file a build script writes under `OUT_DIR` and pulls in with
//! `include!(concat!(env!("OUT_DIR"), ..))` has no author: nobody can edit
//! the flagged line or place an `#[allow]` on it, so a finding there is noise
//! whatever the lint. Cargo hands `OUT_DIR` to the rustc invocation of every
//! crate with a build script — the same variable `env!("OUT_DIR")` reads — so
//! the lint sees exactly the directory the crate includes from.

use clippy_utils::diagnostics;
use rustc_errors::{Applicability, Diag, DiagMessage, MultiSpan};
use rustc_hir::HirId;
use rustc_lint::{LateContext, Lint, LintContext};
use rustc_span::{FileName, Span};

/// Whether `span` lies in a file under the linted crate's `OUT_DIR`.
fn in_build_script_output(cx: &impl LintContext, span: Span) -> bool {
    let Some(out_dir) = std::env::var_os("OUT_DIR") else {
        return false;
    };
    let FileName::Real(file) = cx.sess().source_map().span_to_filename(span) else {
        return false;
    };
    file.local_path()
        .is_some_and(|path| path.starts_with(&out_dir))
}

/// Whether the primary span of `sp` lies in build-script output.
fn is_generated(cx: &impl LintContext, sp: &MultiSpan) -> bool {
    sp.primary_span()
        .is_some_and(|span| in_build_script_output(cx, span))
}

/// [`clippy_utils::diagnostics::span_lint`], silent on build-script output.
pub(crate) fn span_lint<T: LintContext>(
    cx: &T,
    lint: &'static Lint,
    sp: impl Into<MultiSpan>,
    msg: impl Into<DiagMessage>,
) {
    let sp = sp.into();
    if !is_generated(cx, &sp) {
        diagnostics::span_lint(cx, lint, sp, msg);
    }
}

/// [`clippy_utils::diagnostics::span_lint_and_help`], silent on build-script
/// output.
pub(crate) fn span_lint_and_help<T: LintContext>(
    cx: &T,
    lint: &'static Lint,
    span: impl Into<MultiSpan>,
    msg: impl Into<DiagMessage>,
    help_span: Option<Span>,
    help: impl Into<DiagMessage>,
) {
    let span = span.into();
    if !is_generated(cx, &span) {
        diagnostics::span_lint_and_help(cx, lint, span, msg, help_span, help);
    }
}

/// [`clippy_utils::diagnostics::span_lint_and_then`], silent on build-script
/// output.
pub(crate) fn span_lint_and_then<C, S, M, F>(cx: &C, lint: &'static Lint, sp: S, msg: M, f: F)
where
    C: LintContext,
    S: Into<MultiSpan>,
    M: Into<DiagMessage>,
    F: FnOnce(&mut Diag<'_, ()>),
{
    let sp = sp.into();
    if !is_generated(cx, &sp) {
        diagnostics::span_lint_and_then(cx, lint, sp, msg, f);
    }
}

/// [`clippy_utils::diagnostics::span_lint_hir_and_then`], silent on
/// build-script output.
pub(crate) fn span_lint_hir_and_then(
    cx: &LateContext<'_>,
    lint: &'static Lint,
    hir_id: HirId,
    sp: impl Into<MultiSpan>,
    msg: impl Into<DiagMessage>,
    f: impl FnOnce(&mut Diag<'_, ()>),
) {
    let sp = sp.into();
    if !is_generated(cx, &sp) {
        diagnostics::span_lint_hir_and_then(cx, lint, hir_id, sp, msg, f);
    }
}

/// [`clippy_utils::diagnostics::span_lint_and_sugg`], silent on build-script
/// output.
pub(crate) fn span_lint_and_sugg<T: LintContext>(
    cx: &T,
    lint: &'static Lint,
    sp: Span,
    msg: impl Into<DiagMessage>,
    help: impl Into<DiagMessage>,
    sugg: String,
    applicability: Applicability,
) {
    if !in_build_script_output(cx, sp) {
        diagnostics::span_lint_and_sugg(cx, lint, sp, msg, help, sugg, applicability);
    }
}
