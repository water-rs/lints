//! `localized_concat` — a `+` that appends to a localized `Text` builds a
//! sentence no translation catalog can reorder.

use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::get_parent_expr;
use clippy_utils::macros::root_macro_call_first_node;
use rustc_hir::{BinOpKind, Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyKind, TypeckResults};
use rustc_session::declare_lint_pass;

use crate::carriers::{CLONE, INTO, TO_OWNED};
use crate::def_path::def_path_eq;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `+` concatenation producing `Text` whose left operand is
    /// localized text — a `text!(..)` expansion, a `Text::localized*` call,
    /// or a `text(..)`/`Text::new(..)`/`.into_text()`/`.into()` conversion of
    /// a `&'static str` (which `IntoText` resolves through the catalog) — in
    /// any position. A chain reports once, at its outermost `+`.
    ///
    /// ### Why is this bad?
    ///
    /// Sentence fragments cannot be translated as a unit: word order differs
    /// by language, so a concatenated `Text` is untranslatable wherever it
    /// ends up. The fix is one `text!` key whose slots carry the varying
    /// parts.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text!("Dear ") + text!("{name}")
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text!("Dear {name}", name = name)
    /// ```
    pub LOCALIZED_CONCAT,
    pedantic,
    "concatenating localized text builds an untranslatable sentence"
}

const MESSAGE: &str = "concatenating localized text builds an untranslatable sentence";
const HELP: &str = "word order differs by language; make it one `text!` key with slots — \
     `text!(\"Dear {name}\", name = ..)`";

/// `waterui_text::text::Text` — `impl<T: IntoText> Add<T> for Text` is the
/// only `Add` returning it, so a `+` typed to `Text` is a text concat.
const TEXT: &[&str] = &["waterui_text", "text", "Text"];

/// `waterui_macros::text` — the `text!` proc macro.
const TEXT_MACRO: &[&str] = &["waterui_macros", "text"];

/// `Text` constructors that always produce localized text.
const LOCALIZED_CTORS: &[&[&str]] = &[
    &["waterui_text", "text", "Text", "localized"],
    &["waterui_text", "text", "Text", "localized_or"],
    &["waterui_text", "text", "Text", "localized_with"],
];

/// Calls that carry their input's provenance — `.clone()`/`.to_owned()` on a
/// `Text`, and the `IntoText` entry points `text(..)`, `Text::new(..)`,
/// `.into_text()`, `.into()`, whose `&'static str` input is `Text::localized`
/// under the hood while `String`/`Str` stay verbatim.
const FORWARDING: &[&[&str]] = &[
    CLONE,
    TO_OWNED,
    INTO,
    &["waterui_text", "text", "text"],
    &["waterui_text", "text", "Text", "new"],
    &["waterui_text", "text", "IntoText", "into_text"],
];

declare_lint_pass!(LocalizedConcat => [LOCALIZED_CONCAT]);

/// Whether `ty` is `waterui_text::text::Text`.
fn is_text(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    matches!(ty.kind(), TyKind::Adt(def, _) if def_path_eq(cx, def.did(), TEXT))
}

/// `Some(lhs)` when `expr` is a `+` typed to `Text` — a text concatenation.
fn text_add<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    match expr.kind {
        ExprKind::Binary(op, lhs, _)
            if op.node == BinOpKind::Add && is_text(cx, typeck.expr_ty(expr)) =>
        {
            Some(lhs)
        }
        _ => None,
    }
}

/// Whether `expr` — the left operand of a `Text` concat — is localized text:
/// a `text!(..)` expansion, a `Text::localized*` call, or a forwarding call
/// or wrapper whose own input is. Nested `+`s descend the left spine, so a
/// chain is decided by its leftmost leaf.
fn localized_operand<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> bool {
    loop {
        while let ExprKind::DropTemps(inner) = expr.kind {
            expr = inner;
        }
        // The outermost macro producing `expr` decides — `text!` is
        // localized; another macro's output is not the author's fragment.
        if let Some(call) = root_macro_call_first_node(cx, expr) {
            return def_path_eq(cx, call.def_id, TEXT_MACRO);
        }
        if let Some(lhs) = text_add(cx, typeck, expr) {
            expr = lhs;
            continue;
        }
        if let ExprKind::Block(block, _) = expr.kind {
            let Some(tail) = block.expr else {
                return false;
            };
            expr = tail;
            continue;
        }
        let Some(did) = call_def_id(typeck, expr) else {
            return false;
        };
        let did = implemented_trait_item(cx.tcx, did);
        if LOCALIZED_CTORS.iter().any(|p| def_path_eq(cx, did, p)) {
            return true;
        }
        if !FORWARDING.iter().any(|p| def_path_eq(cx, did, p)) {
            return false;
        }
        let Some(&input) = call_args(expr).first() else {
            return false;
        };
        // `&'static str` input resolves through the catalog; anything else is
        // verbatim — unless it is itself localized `Text`, which the loop
        // sees next.
        if matches!(typeck.expr_ty(input).peel_refs().kind(), TyKind::Str) {
            return true;
        }
        expr = input;
    }
}

impl<'tcx> LateLintPass<'tcx> for LocalizedConcat {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let typeck = cx.typeck_results();
        let Some(lhs) = text_add(cx, typeck, expr) else {
            return;
        };
        // A `+` inside a `Text`-`+` chain defers to the outermost one.
        let mut parent = get_parent_expr(cx, expr);
        while let Some(parent_expr) = parent
            && matches!(parent_expr.kind, ExprKind::DropTemps(_))
        {
            parent = get_parent_expr(cx, parent_expr);
        }
        if parent.is_some_and(|parent| text_add(cx, typeck, parent).is_some()) {
            return;
        }
        if localized_operand(cx, typeck, lhs) {
            span_lint_and_help(cx, LOCALIZED_CONCAT, expr.span, MESSAGE, None, HELP);
        }
    }
}
