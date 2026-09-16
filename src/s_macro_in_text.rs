//! `s_macro_in_text` — an `s!(..)` reaching a text position formats its
//! signals into a `String` signal that renders as `Text::verbatim`:
//! untranslatable, and invisible to the plural machinery — the `format!`
//! mistake `format_in_text` flags, only reactive. `text!` takes the same
//! placeholders and yields a `Text` whose key is in the catalog and whose
//! slots subscribe to the signals.

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::macros::root_macro_call_first_node;
use clippy_utils::source::snippet_opt;
use rustc_data_structures::fx::FxHashSet;
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::{Expr, ExprKind, HirId, Node, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::impl_lint_pass;
use rustc_span::Span;

use crate::carriers::{FROM, INTO};
use crate::def_path::def_path_eq;
use crate::format_args::{SCall, parse_s_call};
use crate::param_bounds::{
    TEXT_PARAM_BOUNDS, call_arg_bounds, call_args, call_def_id, implemented_trait_item,
};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags an `s!(..)` (`nami_derive::s`, reached as
    /// `waterui::reactive::s`) whose signal-of-`String` flows into an
    /// `IntoText`/`IntoLabel` parameter — `text(..)`, `Text::new(..)`,
    /// `button(..)`, `.title(..)` — through the `.computed()`/
    /// `.into_computed()`/`Computed::new(..)` conversion the call needs, or
    /// through a `let`.
    ///
    /// ### Why is this bad?
    ///
    /// The `String` the signal carries renders as `Text::verbatim`: it walks
    /// around the translation catalog and the plural machinery exactly as
    /// `format!` does. `text!` formats the same signals through the catalog.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text(s!("Hello {name}").computed())
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text!("Hello {name}")
    /// ```
    pub S_MACRO_IN_TEXT,
    suspicious,
    "`s!` in a text position is untranslatable verbatim text; `text!` formats the same signals \
     through the catalog"
}

const MESSAGE: &str = "`s!` in a text position is untranslatable verbatim text; `text!` formats \
     the same signals through the catalog";
const HELP: &str = "use `text!` so the sentence is one catalog key with slots";

/// `nami_derive::s` — the `s!` proc macro, matched by def path, never by
/// name.
const S_MACRO: &[&str] = &["nami_derive", "s"];

/// Callees whose whole call the `text!` rewrite replaces —
/// `text(s!(..)..)`/`Text::new(s!(..)..)` become `text!(..)`; in any other
/// text position the `text!` call replaces just the argument.
const TEXT_CTORS: &[&[&str]] = &[
    &["waterui_text", "text", "text"],
    &["waterui_text", "text", "Text", "new"],
];

/// Calls that carry the `s!` signal into a shape a text parameter accepts —
/// `s!` yields `Map`, so reaching `IntoText`/`IntoLabel` goes through
/// `.computed()`, `.into_computed()`, or `Computed::new(..)`; `.into()` and
/// `From::from(..)` may rewrap the result.
const S_CARRIERS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "computed"],
    &[
        "nami",
        "reactive_core",
        "signal",
        "IntoComputed",
        "into_computed",
    ],
    &[
        "nami",
        "reactive_core",
        "signal",
        "computed",
        "Computed",
        "new",
    ],
    INTO,
    FROM,
];

/// `expr` with drop-temps and [`S_CARRIERS`] calls peeled — the value
/// flowing through a `.computed()` or `.into()` is the receiver/first
/// argument.
fn peel_carriers<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    loop {
        while let ExprKind::DropTemps(inner) = expr.kind {
            expr = inner;
        }
        let Some(did) = call_def_id(typeck, expr) else {
            return expr;
        };
        if !S_CARRIERS
            .iter()
            .any(|path| def_path_eq(cx, implemented_trait_item(cx.tcx, did), path))
        {
            return expr;
        }
        let Some(&inner) = call_args(expr).first() else {
            return expr;
        };
        expr = inner;
    }
}

/// `Some(span)` of the `s!(..)` callsite when `expr`, carriers peeled, is
/// the first node of an `s!` expansion — `None` when a different macro or no
/// macro produced it.
fn s_call<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<Span> {
    let expr = peel_carriers(cx, typeck, expr);
    let call = root_macro_call_first_node(cx, expr)?;
    def_path_eq(cx, call.def_id, S_MACRO).then(|| expr.span.source_callsite())
}

/// `Some((local, span))` — the binding's `HirId` and the `s!(..)` callsite —
/// when `expr`, carriers peeled, reads a `let` local whose initializer is an
/// `s!` expansion.
fn s_let<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<(HirId, Span)> {
    let ExprKind::Path(QPath::Resolved(None, path)) = peel_carriers(cx, typeck, expr).kind else {
        return None;
    };
    let Res::Local(pat) = path.res else {
        return None;
    };
    let local = cx
        .tcx
        .hir_parent_id_iter(pat)
        .find_map(|id| match cx.tcx.hir_node(id) {
            Node::LetStmt(local) => Some(local),
            _ => None,
        })?;
    let span = s_call(cx, typeck, local.init?)?;
    Some((local.hir_id, span))
}

/// `text!("..", bindings..)` for a parsed `s!` call: `{}`/`{N}` placeholders
/// become named `arg{i}` slots bound after the literal (`{}` counts only
/// implicit positions, matching `format!`), `{name}` slots stay, and the
/// `name = expr` arguments carry over. The template is already in
/// format-source form — `{{`/`}}` escapes and `:spec`s copy verbatim, so a
/// `{:?}` re-emission only re-escapes the Rust-level characters.
fn text_call(call: &SCall) -> Option<String> {
    let mut template = String::new();
    let mut implicit = 0usize;
    let mut rest = call.template.as_str();
    while let Some(open) = rest.find('{') {
        template.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        if rest.starts_with('{') {
            template.push_str("{{");
            rest = &rest[1..];
            continue;
        }
        let close = rest.find('}')?;
        let (name, spec) = match rest[..close].find(':') {
            Some(colon) => (&rest[..colon], &rest[colon..close]),
            None => (&rest[..close], ""),
        };
        let name = if name.is_empty() {
            let index = implicit;
            implicit += 1;
            format!("arg{index}")
        } else if name.bytes().all(|byte| byte.is_ascii_digit()) {
            format!("arg{name}")
        } else {
            name.to_owned()
        };
        template.push('{');
        template.push_str(&name);
        template.push_str(spec);
        template.push('}');
        rest = &rest[close + 1..];
    }
    template.push_str(rest);
    let mut suggestion = format!("text!({template:?}");
    for (index, source) in call.positional.iter().enumerate() {
        suggestion.push_str(&format!(", arg{index} = {source}"));
    }
    for (name, source) in &call.named {
        suggestion.push_str(&format!(", {name} = {source}"));
    }
    suggestion.push(')');
    Some(suggestion)
}

/// The `text!(..)` expression the `s!(..)` at `span` rewrites to, or `None`
/// when the invocation's source cannot be read or parsed — the diagnostic
/// then carries help only.
fn text_suggestion(cx: &LateContext<'_>, span: Span) -> Option<String> {
    let call = parse_s_call(&snippet_opt(cx, span)?)?;
    text_call(&call)
}

/// The pass holds the `let` bindings already reported — a local used in two
/// text positions warns once, on the `s!`.
#[derive(Default)]
pub(crate) struct SMacroInText {
    seen: FxHashSet<HirId>,
}

impl_lint_pass!(SMacroInText => [S_MACRO_IN_TEXT]);

impl<'tcx> LateLintPass<'tcx> for SMacroInText {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
            return;
        }
        let typeck = cx.typeck_results();
        let Some((callee, per_arg)) = call_arg_bounds(cx, expr, &[TEXT_PARAM_BOUNDS]) else {
            return;
        };
        let whole_call = TEXT_CTORS.iter().any(|path| def_path_eq(cx, callee, path));
        for (arg, bounds) in call_args(expr).into_iter().zip(per_arg) {
            if bounds.is_empty() {
                continue;
            }
            if let Some(span) = s_call(cx, typeck, arg) {
                let suggestion = text_suggestion(cx, span);
                span_lint_and_then(cx, S_MACRO_IN_TEXT, span, MESSAGE, |diag| {
                    match suggestion {
                        Some(text) => {
                            // `text(s!(..))`/`Text::new(s!(..))` — the `text!`
                            // call replaces the whole constructor call.
                            let span = if whole_call { expr.span } else { arg.span };
                            diag.span_suggestion(
                                span,
                                HELP,
                                text,
                                Applicability::MachineApplicable,
                            );
                        }
                        None => {
                            diag.help(HELP);
                        }
                    }
                });
            } else if let Some((local, span)) = s_let(cx, typeck, arg)
                && self.seen.insert(local)
            {
                // The `let` may have other readers — the `text!` rewrite is
                // help, not a suggestion.
                let suggestion = text_suggestion(cx, span);
                span_lint_and_then(
                    cx,
                    S_MACRO_IN_TEXT,
                    span,
                    MESSAGE,
                    |diag| match suggestion {
                        Some(text) => {
                            diag.help(format!(
                                "use `{text}` — `text!` keeps the slots in the catalog"
                            ));
                        }
                        None => {
                            diag.help(HELP);
                        }
                    },
                );
            }
        }
    }
}
