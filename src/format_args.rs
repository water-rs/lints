//! The early pass that keeps `format!(..)`'s AST [`FormatArgs`] alive for late
//! passes — HIR lowers `format_args!` into a desugared call that no longer
//! carries the literal's placeholders. Shared by every lint that rebuilds a
//! `format!` template (`signal_get_in_view`, `manual_text_map`,
//! `format_in_text`).

use clippy_utils::macros::{FormatArgsStorage, find_format_arg_expr, root_macro_call_first_node};
use clippy_utils::source::{SpanRangeExt, snippet_opt};
use rustc_ast::format::{
    FormatAlignment, FormatArgsPiece, FormatArgumentKind, FormatCount, FormatDebugHex,
    FormatOptions, FormatSign, FormatTrait,
};
use rustc_ast::{Crate as AstCrate, Expr as AstExpr, ExprKind as AstExprKind, FormatArgs};
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, hygiene, sym};
use std::mem;

use crate::snapshot_get::{get_receiver, is_snapshot_get};

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

/// Escape `text` for reuse inside a `text!` literal — braces double up, the
/// rest is verbatim.
pub(crate) fn escape_literal(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(ch),
        }
    }
}

/// `:?`/`:>8.2`/`:x?` — everything `FormatOptions`/`FormatTrait` carries,
/// returned with the leading `:`, or `None` when a spec uses an argument
/// count (`{:.*}`, `{:w$}`), which a `text!` rewrite cannot carry.
pub(crate) fn render_options(options: &FormatOptions, trait_: FormatTrait) -> Option<String> {
    for count in [options.width.as_ref(), options.precision.as_ref()]
        .into_iter()
        .flatten()
    {
        if matches!(count, FormatCount::Argument(_)) {
            return None;
        }
    }
    let mut spec = String::new();
    if let Some(alignment) = options.alignment {
        if let Some(fill) = options.fill {
            spec.push(fill);
        }
        spec.push(match alignment {
            FormatAlignment::Left => '<',
            FormatAlignment::Right => '>',
            FormatAlignment::Center => '^',
        });
    }
    if let Some(sign) = options.sign {
        spec.push(match sign {
            FormatSign::Plus => '+',
            FormatSign::Minus => '-',
        });
    }
    if options.alternate {
        spec.push('#');
    }
    if options.zero_pad {
        spec.push('0');
    }
    if let Some(FormatCount::Literal(width)) = &options.width {
        spec.push_str(&width.to_string());
    }
    if let Some(FormatCount::Literal(precision)) = &options.precision {
        spec.push('.');
        spec.push_str(&precision.to_string());
    }
    if let Some(debug_hex) = options.debug_hex {
        spec.push(match debug_hex {
            FormatDebugHex::Lower => 'x',
            FormatDebugHex::Upper => 'X',
        });
    }
    spec.push_str(match trait_ {
        FormatTrait::Display => "",
        FormatTrait::Debug => "?",
        FormatTrait::LowerExp => "e",
        FormatTrait::UpperExp => "E",
        FormatTrait::Octal => "o",
        FormatTrait::Pointer => "p",
        FormatTrait::Binary => "b",
        FormatTrait::LowerHex => "x",
        FormatTrait::UpperHex => "X",
    });
    Some(if spec.is_empty() {
        spec
    } else {
        format!(":{spec}")
    })
}

/// An identifier usable in a `text!` `{name}` placeholder, paired with the
/// source of the value it must be bound to — `None` when the name is a
/// plain in-scope binding that `text!` captures by itself.
fn placeholder_name(
    cx: &LateContext<'_>,
    fallback: usize,
    expr: &Expr<'_>,
) -> (String, Option<String>) {
    if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
        && let [segment] = path.segments
    {
        return (segment.ident.to_string(), None);
    }
    let name = match expr.kind {
        ExprKind::Field(_, ident) => ident.to_string(),
        ExprKind::Path(QPath::Resolved(None, path)) => path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_else(|| format!("arg{fallback}")),
        _ => format!("arg{fallback}"),
    };
    (
        name,
        Some(snippet_opt(cx, expr.span).unwrap_or_else(|| "_".into())),
    )
}

/// Builds the `text!("{name}"[, name = expr]*)` rewrite for a `format!(..)`
/// call in a text position, or `None` when the expansion or spans cannot be
/// mapped back to source.
pub(crate) fn text_macro_suggestion(
    storage: &FormatArgsStorage,
    cx: &LateContext<'_>,
    arg: &Expr<'_>,
) -> Option<String> {
    let macro_call = root_macro_call_first_node(cx, arg)?;
    if cx.tcx.get_diagnostic_name(macro_call.def_id) != Some(sym::format_macro) {
        return None;
    }
    let format_args = storage.get(cx, arg, macro_call.expn)?;

    // Name every argument; explicit `name = expr` bindings keep their name and
    // are re-emitted with the snapshot's receiver as the value.
    let mut names: Vec<String> = Vec::new();
    let mut bindings: Vec<String> = Vec::new();
    for (index, argument) in format_args.arguments.all_args().iter().enumerate() {
        let hir_arg = find_format_arg_expr(arg, argument);
        let get_receiver = hir_arg.and_then(|expr| {
            is_snapshot_get(cx, expr)
                .then(|| get_receiver(expr))
                .flatten()
        });
        let (name, binding) = match argument.kind {
            FormatArgumentKind::Captured(ident) => (ident.to_string(), None),
            FormatArgumentKind::Named(ident) => {
                let source = get_receiver
                    .or(hir_arg)
                    .and_then(|expr| snippet_opt(cx, expr.span))?;
                (ident.to_string(), Some(source))
            }
            FormatArgumentKind::Normal => placeholder_name(cx, index, get_receiver.or(hir_arg)?),
        };
        if let Some(source) = binding {
            bindings.push(format!("{name} = {source}"));
        }
        names.push(name);
    }

    // Rebuild the literal from the template: every placeholder becomes
    // `{name}` since `text!` only takes named slots. Template literals are
    // unescaped text, so braces they contain must be re-escaped as `{{`/`}}`.
    let mut literal = String::new();
    for piece in &format_args.template {
        match piece {
            FormatArgsPiece::Literal(text) => escape_literal(text.as_str(), &mut literal),
            FormatArgsPiece::Placeholder(placeholder) => {
                let index = placeholder.argument.index.ok()?;
                // A spec the slot can't carry (`{:.*}`, `{:w$}`) loses the
                // suggestion — the help stays.
                let spec = render_options(&placeholder.format_options, placeholder.format_trait)?;
                literal.push('{');
                literal.push_str(names.get(index)?);
                literal.push_str(&spec);
                literal.push('}');
            }
        }
    }

    let mut suggestion = format!("text!({literal:?}");
    for binding in &bindings {
        suggestion.push_str(", ");
        suggestion.push_str(binding);
    }
    suggestion.push(')');
    Some(suggestion)
}
