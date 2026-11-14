//! `format_in_text` — `format!` prose reaching a text position renders as
//! `Text::verbatim`: untranslatable, and invisible to the plural machinery.

use clippy_utils::macros::{FormatArgsStorage, find_format_arg_expr, root_macro_call_first_node};
use rustc_ast::FormatArgs;
use rustc_ast::format::FormatArgsPiece;
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::impl_lint_pass;
use rustc_span::sym;
use rustc_span::symbol::Ident;

use crate::carriers::{FROM, INTO, TEXT_VERBATIM, TO_STRING, is_string_ty};
use crate::def_path::def_path_eq;
use crate::diagnostics::{span_lint_and_help, span_lint_and_then};
use crate::format_args::text_macro_suggestion;
use crate::param_bounds::{
    TEXT_PARAM_BOUNDS, call_arg_bounds, call_args, call_def_id, implemented_trait_item,
};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `format!(..)` whose template holds prose — at least one
    /// literal letter — whose result flows into an `IntoText`/`IntoLabel`
    /// parameter, directly or through `.to_string()`, `String::from`,
    /// `Str::from`, `.into()`, or `Text::verbatim(..)`. A pure data
    /// pass-through like `format!("{}", id)` stays silent.
    ///
    /// ### Why is this bad?
    ///
    /// The formatted `String` becomes `Text::verbatim`: untranslatable, and
    /// it bypasses the placeholder/plural machinery. `text!` keeps the
    /// sentence as one catalog key whose slots subscribe to the values.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text(format!("Record #{:06}", record.id))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text(text!("Record #{id:06}", id = record.id))
    /// ```
    pub FORMAT_IN_TEXT,
    suspicious,
    "`format!` prose in a text position is untranslatable verbatim text"
}

/// `PLURAL_BYPASS` shares this module with the lint above — one `format!`
/// analysis feeds both — so its declaration lives in a submodule to keep the
/// two `LINT_INFO`s apart.
pub(crate) mod plural_bypass {
    declare_waterui_lint! {
        /// ### What it does
        ///
        /// Flags the `format_in_text` case that also splices an integer into
        /// the prose: a `format!(..)` with letters in its template and an
        /// integral placeholder argument, in an `IntoText`/`IntoLabel`
        /// position.
        ///
        /// ### Why is this bad?
        ///
        /// An integer formatted into prose skips CLDR plural categories —
        /// `{#count}` in a `text!` key is the only channel that yields the
        /// `one`/`few`/`other` form a locale needs. A separate lint name so
        /// code that formats a number on purpose can allow it alone.
        ///
        /// ### Example
        ///
        /// ```rust,ignore
        /// text(format!("{count} unread"))
        /// ```
        ///
        /// Use instead:
        ///
        /// ```rust,ignore
        /// text(text!("{#count} unread"))
        /// ```
        pub PLURAL_BYPASS,
        suspicious,
        "an integer formatted into prose bypasses the plural categories"
    }
}

const FORMAT_MESSAGE: &str = "`format!` prose in a text position is untranslatable verbatim text";
const FORMAT_HELP: &str = "use `text!` so the sentence is one catalog key with slots";
const PLURAL_MESSAGE: &str = "an integer formatted into prose bypasses the plural categories";
const PLURAL_HELP: &str = "`{#count}` in a `text!` key is the only channel that yields correct \
     plural forms per locale";

pub(crate) struct FormatInText {
    format_args: FormatArgsStorage,
}

impl FormatInText {
    pub(crate) fn new(format_args: FormatArgsStorage) -> Self {
        Self { format_args }
    }
}

impl_lint_pass!(FormatInText => [
    FORMAT_IN_TEXT,
    plural_bypass::PLURAL_BYPASS,
]);

/// Whether `ident` can name a `text!` slot — a plain identifier: not a
/// keyword (`self`), not a special symbol, and not a tuple-index field name
/// (`0`), which `text!` could not bind.
fn slot_ident(ident: Ident) -> bool {
    !ident.is_reserved()
        && !ident.is_special()
        && ident
            .name
            .as_str()
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
}

/// `a` or `a.b.c` — a local, or a field projection of one, every step of
/// which can name a `text!` slot. The rewrite captures or aliases it
/// verbatim; anything more complex keeps the help without a suggestion.
fn simple_path(expr: &Expr<'_>) -> bool {
    match expr.kind {
        ExprKind::Path(QPath::Resolved(None, path)) => {
            matches!(path.res, Res::Local(_))
                && matches!(path.segments, [segment] if slot_ident(segment.ident))
        }
        ExprKind::Field(base, ident) => slot_ident(ident) && simple_path(base),
        _ => false,
    }
}

/// `Some((format_node, args))` when `arg`, after peeling drop-temps and the
/// value carriers (`.to_string()`, `String::from`, `Str::from`, `.into()`,
/// `Text::verbatim(..)`), is a `format!(..)` expansion — `format_node` is the
/// expansion's first node, `args` its preserved AST `FormatArgs`. `None`
/// when the outermost macro producing the expression is anything else —
/// `text!` itself expands through `format!`, and a macro that *generates* a
/// `format!` is not the user's prose.
fn format_call<'a, 'hir>(
    storage: &'a FormatArgsStorage,
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    arg: &'hir Expr<'hir>,
) -> Option<(&'hir Expr<'hir>, &'a FormatArgs)> {
    let mut expr = arg;
    loop {
        while let ExprKind::DropTemps(inner) = expr.kind {
            expr = inner;
        }
        if let Some(call) = root_macro_call_first_node(cx, expr) {
            return if cx.tcx.get_diagnostic_name(call.def_id) == Some(sym::format_macro) {
                storage.get(cx, expr, call.expn).map(|args| (expr, args))
            } else {
                None
            };
        }
        let did = implemented_trait_item(cx.tcx, call_def_id(typeck, expr)?);
        let carrier = def_path_eq(cx, did, TO_STRING)
            || def_path_eq(cx, did, TEXT_VERBATIM)
            || ((def_path_eq(cx, did, FROM) || def_path_eq(cx, did, INTO))
                && is_string_ty(cx, typeck.expr_ty(expr)));
        if !carrier {
            return None;
        }
        expr = *call_args(expr).first()?;
    }
}

impl<'tcx> LateLintPass<'tcx> for FormatInText {
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
            let Some((format_expr, format_args)) = format_call(&self.format_args, cx, typeck, arg)
            else {
                continue;
            };
            // Prose, not a data pass-through: at least one literal letter in
            // the template (`format!("{}", id)` stays silent).
            if !format_args.template.iter().any(|piece| {
                matches!(piece, FormatArgsPiece::Literal(text)
                    if text.as_str().chars().any(char::is_alphabetic))
            }) {
                continue;
            }
            let mut integral = false;
            let mut simple = true;
            for argument in format_args.arguments.all_args() {
                let Some(arg_expr) = find_format_arg_expr(format_expr, argument) else {
                    simple = false;
                    continue;
                };
                if typeck.expr_ty(arg_expr).peel_refs().is_integral() {
                    integral = true;
                }
                if !simple_path(arg_expr) {
                    simple = false;
                }
            }
            let suggestion =
                simple.then(|| text_macro_suggestion(&self.format_args, cx, format_expr));
            // The `format!` expansion root's span lives inside `alloc`'s
            // `macros.rs` — lint at the callsite so the diagnostic (and the
            // suggestion's replacement range) land on the user's `format!(..)`.
            let span = arg.span.source_callsite();
            span_lint_and_then(
                cx,
                FORMAT_IN_TEXT,
                span,
                FORMAT_MESSAGE,
                |diag| match suggestion.flatten() {
                    Some(suggestion) => {
                        diag.span_suggestion(
                            span,
                            FORMAT_HELP,
                            suggestion,
                            Applicability::MaybeIncorrect,
                        );
                    }
                    None => {
                        diag.help(FORMAT_HELP);
                    }
                },
            );
            if integral {
                span_lint_and_help(
                    cx,
                    plural_bypass::PLURAL_BYPASS,
                    span,
                    PLURAL_MESSAGE,
                    None,
                    PLURAL_HELP,
                );
            }
        }
    }
}
