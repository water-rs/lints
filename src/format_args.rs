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
use rustc_ast::{
    Crate as AstCrate, Expr as AstExpr, ExprKind as AstExprKind, FormatArgs, LitKind, token,
};
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lexer::{FrontmatterAllowed, LiteralKind, TokenKind, tokenize};
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, Symbol, hygiene, sym};
use std::mem;
use std::ops::Range;

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

/// A `text!("lit" @ ctx, name = expr, ..)` invocation's source, parsed for
/// lints that read or rewrite the call — `literal` is the template's cooked
/// text.
pub(crate) struct TextCall {
    /// The format literal's cooked value.
    pub(crate) literal: String,
    /// The `@ "ctx"` / `@ ident` clause, verbatim source to re-emit.
    pub(crate) context: Option<String>,
    /// `(name, value source range)` — the `name = expr` pairs.
    pub(crate) bindings: Vec<(String, Range<usize>)>,
}

/// The non-trivia tokens of `src` with their byte ranges.
fn tokens(src: &str) -> Vec<(TokenKind, Range<usize>)> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    for token in tokenize(src, FrontmatterAllowed::No) {
        let start = pos;
        pos += token.len as usize;
        if !matches!(
            token.kind,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } | TokenKind::Whitespace
        ) {
            out.push((token.kind, start..pos));
        }
    }
    out
}

/// Unescape a `".."` / `r#".."#` literal to its cooked value.
fn unescape_str(text: &str) -> Option<String> {
    let (kind, content) = if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.bytes().take_while(|&b| b == b'#').count();
        // `r###"content"###` — strip the `r`, the hashes, and both quotes.
        let content = rest.get(hashes + 1..rest.len() - hashes - 1)?;
        (token::LitKind::StrRaw(hashes as u8), content)
    } else {
        (
            token::LitKind::Str,
            text.strip_prefix('"')?.strip_suffix('"')?,
        )
    };
    let lit = LitKind::from_token_lit(token::Lit::new(kind, Symbol::intern(content), None)).ok()?;
    if let LitKind::Str(symbol, _) = lit {
        Some(symbol.to_string())
    } else {
        None
    }
}

/// Parse `text!("lit" @ ctx, name = expr, ..)` back into parts — `None` when
/// the source isn't that shape.
pub(crate) fn parse_text_call(src: &str) -> Option<TextCall> {
    let tokens = tokens(src);
    // `text ! ( ... )` — find the bang, then the opening delimiter.
    let bang = tokens
        .iter()
        .position(|(kind, _)| matches!(kind, TokenKind::Bang))?;
    if !matches!(
        tokens.get(bang + 1)?.0,
        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket
    ) {
        return None;
    }
    let mut literal = None;
    let mut context = None;
    let mut bindings = Vec::new();
    let mut index = bang + 2;
    while index < tokens.len() {
        let mut depth = 1usize;
        // Skip nested delimiters wholesale — the interesting tokens live at
        // depth 1.
        let (kind, range) = &tokens[index];
        match kind {
            TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => {
                index += 1;
                while index < tokens.len() && depth > 0 {
                    match tokens[index].0 {
                        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => {
                            depth += 1;
                        }
                        TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => {
                            depth -= 1
                        }
                        _ => {}
                    }
                    index += 1;
                }
                continue;
            }
            TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => break,
            TokenKind::Literal {
                kind:
                    LiteralKind::Str { terminated: true } | LiteralKind::RawStr { n_hashes: Some(_) },
                ..
            } if literal.is_none() => {
                literal = Some(unescape_str(&src[range.clone()])?);
            }
            TokenKind::At if literal.is_some() && context.is_none() => {
                // `@ "ctx"` or `@ ident` — the next token is the context.
                let ctx_range = tokens.get(index + 1)?.1.clone();
                context = Some(src[range.start..ctx_range.end].to_owned());
                index += 1;
            }
            TokenKind::Ident if matches!(tokens.get(index + 1), Some((TokenKind::Eq, _))) => {
                // `name = <expr>` — the value runs to the next depth-1 comma
                // or the closing delimiter.
                let name = src[range.clone()].to_owned();
                let value_start = tokens.get(index + 2)?.1.start;
                let mut end = index + 2;
                let mut nested = 0usize;
                while let Some((kind, _)) = tokens.get(end) {
                    match kind {
                        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => {
                            nested += 1;
                        }
                        TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => {
                            if nested == 0 {
                                break;
                            }
                            nested -= 1;
                        }
                        TokenKind::Comma if nested == 0 => break,
                        _ => {}
                    }
                    end += 1;
                }
                if end == index + 2 {
                    return None;
                }
                let value_end = tokens[end - 1].1.end;
                bindings.push((name, value_start..value_end));
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    Some(TextCall {
        literal: literal?,
        context,
        bindings,
    })
}

/// An `s!("lit", expr*, name = expr*)` invocation's pieces, parsed back from
/// source for the `s_macro_in_text` rewrite — a proc macro leaves no
/// `format_args!` node. `template` is the format literal's cooked text with
/// the format escapes (`{{`/`}}`) intact, so its literal runs copy into a
/// `text!` template verbatim; `positional` and `named` carry each
/// argument's source verbatim.
pub(crate) struct SCall {
    /// The format literal's cooked text, format escapes intact.
    pub(crate) template: String,
    /// Positional argument sources, in order.
    pub(crate) positional: Vec<String>,
    /// `name = expr` pairs, in order.
    pub(crate) named: Vec<(String, String)>,
}

/// Parse `s!(..)`'s source into its parts — `None` unless the tokens are
/// `ident ! delim literal , args.. delim` with a string literal first.
pub(crate) fn parse_s_call(src: &str) -> Option<SCall> {
    let toks = tokens(src);
    // `s ! ( ... )` — find the bang, then the opening delimiter.
    let bang = toks
        .iter()
        .position(|(kind, _)| matches!(kind, TokenKind::Bang))?;
    let open = toks.get(bang + 1)?;
    if !matches!(
        open.0,
        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket
    ) {
        return None;
    }
    // Split the depth-1 tokens on commas — each segment is one argument.
    let mut args: Vec<&str> = Vec::new();
    let mut depth = 1usize;
    let mut start = open.1.end;
    let mut index = bang + 2;
    while let Some((kind, range)) = toks.get(index) {
        match kind {
            TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => depth += 1,
            TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => {
                depth -= 1;
                if depth == 0 {
                    let arg = src[start..range.start].trim();
                    if !arg.is_empty() {
                        args.push(arg);
                    }
                    break;
                }
            }
            TokenKind::Comma if depth == 1 => {
                let arg = src[start..range.start].trim();
                if !arg.is_empty() {
                    args.push(arg);
                }
                start = range.end;
            }
            _ => {}
        }
        index += 1;
    }
    if depth != 0 {
        return None;
    }
    // The first argument is the format literal; the rest are positional
    // expressions or `name = expr` pairs (`s!` itself forbids mixing them).
    let (literal, rest) = args.split_first()?;
    let mut call = SCall {
        template: unescape_str(literal)?,
        positional: Vec::new(),
        named: Vec::new(),
    };
    for arg in rest {
        let arg_tokens = tokens(arg);
        if let Some((TokenKind::Ident, name)) = arg_tokens.first()
            && let Some((TokenKind::Eq, eq)) = arg_tokens.get(1)
        {
            call.named.push((
                arg[name.clone()].to_owned(),
                arg[eq.end..].trim().to_owned(),
            ));
        } else {
            call.positional.push((*arg).to_owned());
        }
    }
    Some(call)
}
