//! The translation keys `text!(..)` and `Text::localized*` embed — the shared
//! channel the catalog lints (`missing_translation`, `orphan_translation`,
//! `long_text_key`) read from.
//!
//! `Text::localized(key)`/`localized_or(key, ..)` carry the key as a plain
//! string literal argument — read from HIR. `text!("..")` expands to a single
//! `Text::localized_with` call whose resolver closure ends in the *default
//! format*: the key with `{#` rewritten to `{`, held as a `LitStr` for a
//! slot-free key and as the `format!(..)` template for a slot-carrying one.
//! HIR lowers `format!` to a packed `core::fmt::Arguments` bytecode literal
//! that no longer carries the text, so the default format is collected on the
//! post-expansion AST by [`TextKeyCollector`] and handed to the late pass
//! through [`TextKeys`] — the same hand-off `FormatArgsStorage` uses.

use clippy_utils::macros::macro_backtrace;
use rustc_ast::token::{self, LitKind as TokenLitKind};
use rustc_ast::visit::{self, Visitor};
use rustc_ast::{Crate as AstCrate, Expr as AstExpr, ExprKind as AstExprKind, LitKind, StmtKind};
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, Symbol};
use std::mem;
use std::sync::{Arc, OnceLock};

use crate::def_path::def_path_eq;
use crate::param_bounds::call_def_id;

/// `Text::localized`/`localized_or` — the key is argument 0 verbatim.
const LOCALIZED_FNS: &[&[&str]] = &[
    &["waterui_text", "text", "Text", "localized"],
    &["waterui_text", "text", "Text", "localized_or"],
];

/// `Text::localized_with` — the call `text!` emits. A direct call carries a
/// resolver, not a key; only one reached through a `text!` expansion counts.
const LOCALIZED_WITH: &[&str] = &["waterui_text", "text", "Text", "localized_with"];

/// The `text!` proc macro.
const TEXT_MACRO: &[&str] = &["waterui_macros", "text"];

/// `text!` call-site span → the default format the expansion embeds (the
/// translation key with `{#` already rewritten to `{`). Written once by the
/// early pass, read by late passes — the same hand-off `FormatArgsStorage`
/// uses.
#[derive(Clone, Default)]
pub(crate) struct TextKeys {
    map: Arc<OnceLock<FxHashMap<Span, Symbol>>>,
}

impl TextKeys {
    /// The key recorded for the `text!` call at `call_site`.
    fn get(&self, call_site: Span) -> Option<Symbol> {
        self.map.get()?.get(&call_site).copied()
    }
}

/// Early pass that fills [`TextKeys`].
///
/// Runs on the post-expansion AST, where the `format!` inside a
/// slot-carrying `text!` default is still a `FormatArgs` node and its
/// `uncooked_fmt_str` is the whole template string.
pub(crate) struct TextKeyCollector {
    map: FxHashMap<Span, Symbol>,
    keys: TextKeys,
}

impl TextKeyCollector {
    pub(crate) fn new(keys: TextKeys) -> Self {
        Self {
            map: FxHashMap::default(),
            keys,
        }
    }
}

impl_lint_pass!(TextKeyCollector => []);

/// The cooked string of a `Str`/`StrRaw` token literal — the catalog and the
/// `text!` expansion both hold cooked text, so the raw token symbol (which
/// keeps `\` escapes) cannot be compared directly.
fn cooked_str(kind: TokenLitKind, symbol: Symbol) -> Option<Symbol> {
    if !matches!(kind, TokenLitKind::Str | TokenLitKind::StrRaw(_)) {
        return None;
    }
    match LitKind::from_token_lit(token::Lit::new(kind, symbol, None)) {
        Ok(LitKind::Str(text, _)) => Some(text),
        _ => None,
    }
}

/// The string literals in `expr`'s subtree — plain `LitStr`s and the raw
/// template of each `format_args!`. The template is taken whole and the node
/// is not descended: literals inside the format arguments are values, not
/// text.
#[derive(Default)]
struct KeyLiterals {
    symbols: Vec<Symbol>,
}

impl<'ast> Visitor<'ast> for KeyLiterals {
    fn visit_expr(&mut self, expr: &'ast AstExpr) {
        match &expr.kind {
            AstExprKind::Lit(lit) => {
                if let Some(text) = cooked_str(lit.kind, lit.symbol) {
                    self.symbols.push(text);
                }
            }
            AstExprKind::FormatArgs(args) => {
                let (kind, symbol) = args.uncooked_fmt_str;
                if let Some(text) = cooked_str(kind, symbol) {
                    self.symbols.push(text);
                }
                return;
            }
            _ => {}
        }
        visit::walk_expr(self, expr);
    }
}

/// A block's tail expression — in the AST the tail is a trailing
/// [`StmtKind::Expr`], not a field on the block.
fn block_tail(expr: &AstExpr) -> Option<&AstExpr> {
    let AstExprKind::Block(block, _) = &expr.kind else {
        return None;
    };
    match block.stmts.last()?.kind {
        StmtKind::Expr(ref tail) => Some(tail),
        _ => None,
    }
}

/// The default-format string `Text::localized_with({ .. })` carries in a
/// `text!` expansion: the tail of the `move |env, locale| { ..; default }`
/// resolver closure — exactly one string literal for a slot-free key, exactly
/// one `format!` template for a slot-carrying one. Any other shape (a foreign
/// `localized_with` callee, a macro version this does not know) yields `None`
/// rather than a guess.
fn default_format(arg: &AstExpr) -> Option<Symbol> {
    let AstExprKind::Closure(closure) = &block_tail(arg)?.kind else {
        return None;
    };
    let tail = block_tail(&closure.body)?;
    let mut literals = KeyLiterals::default();
    literals.visit_expr(tail);
    literals.symbols.sort_unstable();
    literals.symbols.dedup();
    let [key] = literals.symbols.as_slice() else {
        return None;
    };
    Some(*key)
}

impl EarlyLintPass for TextKeyCollector {
    fn check_expr(&mut self, _: &EarlyContext<'_>, expr: &AstExpr) {
        let AstExprKind::Call(func, args) = &expr.kind else {
            return;
        };
        let AstExprKind::Path(_, path) = &func.kind else {
            return;
        };
        if path
            .segments
            .last()
            .is_none_or(|segment| segment.ident.name.as_str() != "localized_with")
        {
            return;
        }
        // The innermost macro frame is the macro that emitted this call —
        // `text!` for a real expansion. The entry is keyed by that frame's
        // call-site span and the late pass verifies the frame is
        // `waterui_macros::text` by def path before reading it, so foreign
        // `localized_with` callees and aliased `text!` imports both resolve
        // correctly.
        let Some(call) = macro_backtrace(expr.span).next() else {
            return;
        };
        if let [arg] = &args[..]
            && let Some(key) = default_format(arg)
        {
            self.map.entry(call.span).or_insert(key);
        }
    }

    fn check_crate_post(&mut self, _: &EarlyContext<'_>, _: &AstCrate) {
        let _ = self.keys.map.set(mem::take(&mut self.map));
    }
}

/// Which space a collected key belongs to: `Text::localized*` arguments are
/// looked up in the catalog verbatim ([`Catalog`](KeySpace::Catalog)), while a
/// `text!` default format already has `{#` rewritten to `{`
/// ([`Format`](KeySpace::Format)) — the same rewrite the catalog key gets for
/// comparison.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum KeySpace {
    Catalog,
    Format,
}

/// A translation key found in source: the key and the span of the call or
/// macro invocation that embeds it.
pub(crate) struct LocalizedKey {
    /// The key text, in `space`.
    pub(crate) text: Symbol,
    /// The span of the `Text::localized*` call or the `text!` invocation.
    pub(crate) span: Span,
    /// Which space `text` belongs to.
    pub(crate) space: KeySpace,
}

/// The translation key `expr` contributes, when `expr` is a
/// `Text::localized`/`localized_or` call with a literal first argument or a
/// `Text::localized_with` call emitted by `text!` — `None` otherwise, so the
/// result is at most one key per call/macro invocation.
pub(crate) fn localized_key<'tcx>(
    cx: &LateContext<'tcx>,
    keys: &TextKeys,
    expr: &'tcx Expr<'tcx>,
) -> Option<LocalizedKey> {
    let ExprKind::Call(_, args) = expr.kind else {
        return None;
    };
    let def_id = call_def_id(cx.typeck_results(), expr)?;
    if LOCALIZED_FNS
        .iter()
        .any(|path| def_path_eq(cx, def_id, path))
    {
        let mut arg = args.first()?;
        while let ExprKind::DropTemps(inner) = arg.kind {
            arg = inner;
        }
        let ExprKind::Lit(lit) = arg.kind else {
            return None;
        };
        if arg.span.from_expansion() {
            return None;
        }
        let LitKind::Str(text, _) = lit.node else {
            return None;
        };
        return Some(LocalizedKey {
            text,
            span: expr.span,
            space: KeySpace::Catalog,
        });
    }
    if def_path_eq(cx, def_id, LOCALIZED_WITH)
        && let Some(call) =
            macro_backtrace(expr.span).find(|call| def_path_eq(cx, call.def_id, TEXT_MACRO))
        && let Some(text) = keys.get(call.span)
    {
        return Some(LocalizedKey {
            text,
            span: call.span,
            space: KeySpace::Format,
        });
    }
    None
}
