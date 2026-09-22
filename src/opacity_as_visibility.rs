use clippy_utils::source::snippet_opt;
use clippy_utils::usage::local_used_after_expr;
use rustc_ast::LitKind;
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::{Block, Expr, ExprKind, PatKind, QPath, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::def_path::def_path_eq;
use crate::diagnostics::{span_lint_and_help, span_lint_and_sugg};
use crate::param_bounds::{call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `.opacity(..)` used as a visibility toggle: a literal `0.0`, a
    /// `bool` signal's `select(1.0, 0.0)` in either order, or
    /// `.map(|b| if b { 1.0 } else { 0.0 })`.
    ///
    /// ### Why is this bad?
    ///
    /// `.visible(..)` sets opacity *and* marks the node accessibility-hidden
    /// and non-hittable; `.opacity(0.0)` leaves an invisible view that screen
    /// readers announce and taps hit.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("dot").opacity(selected.select(1.0, 0.0))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("dot").visible(selected)
    /// ```
    pub OPACITY_AS_VISIBILITY,
    suspicious,
    "`.opacity(0.0)` hides a view from sight but not from the accessibility tree or hit-testing"
}

declare_lint_pass!(OpacityAsVisibility => [OPACITY_AS_VISIBILITY]);

/// `waterui_internal::view::ViewExt::opacity` — the call the lint rewrites.
const OPACITY: &[&str] = &["waterui_internal", "view", "ViewExt", "opacity"];

/// `nami::reactive_core::ext::SignalExt::select` — `sig.select(on, off)`.
const SELECT: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "select"];

/// `nami::reactive_core::ext::SignalExt::map` — `sig.map(|b| ..)`.
const MAP: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "map"];

const MESSAGE: &str = "`.opacity(0.0)` hides the view visually but leaves it in the accessibility tree and hit-testing";
const SUGGESTION: &str =
    "use `.visible(..)`, which also hides the view from screen readers and taps";

/// `expr` as a bare tail: drop-temps and statement-free blocks peeled, so
/// `{ 1.0 }` and `1.0` count the same. Parentheses do not survive into HIR.
fn tail<'hir>(mut expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(
                Block {
                    stmts: [],
                    expr: Some(inner),
                    ..
                },
                _,
            ) => inner,
            _ => return expr,
        };
    }
}

/// The numeric value of a float or integer literal — `0.0`, `0`, `1f32` —
/// so both spellings of a zero/one toggle are caught.
fn lit_value(expr: &Expr<'_>) -> Option<f64> {
    let ExprKind::Lit(lit) = tail(expr).kind else {
        return None;
    };
    match lit.node {
        LitKind::Float(sym, _) => sym.as_str().parse().ok(),
        LitKind::Int(value, _) => Some(value.get() as f64),
        _ => None,
    }
}

/// What the flagged `.opacity(..)` call rewrites to.
enum Fix<'hir> {
    /// `.visible(false)` — the argument is a literal `0`.
    False,
    /// `.visible(sig)` — or `.visible(sig.not())` when the signal's `true`
    /// side mapped to `0.0`.
    Signal(&'hir Expr<'hir>, bool),
}

/// `sig.map(|b| if b { then } else { else_ })` — the closure must take one
/// identifier parameter and its body must be `if <param>` over the two
/// literals.
fn map_fix<'hir>(
    cx: &LateContext<'hir>,
    receiver: &'hir Expr<'hir>,
    func: &'hir Expr<'hir>,
) -> Option<Fix<'hir>> {
    let ExprKind::Closure(closure) = tail(func).kind else {
        return None;
    };
    let body = cx.tcx.hir_body(closure.body);
    let [param] = body.params else {
        return None;
    };
    let PatKind::Binding(_, param_hir, ..) = param.pat.kind else {
        return None;
    };
    let ExprKind::If(cond, then, Some(else_)) = tail(body.value).kind else {
        return None;
    };
    let ExprKind::Path(QPath::Resolved(None, path)) = tail(cond).kind else {
        return None;
    };
    if path.res != Res::Local(param_hir) {
        return None;
    }
    match (lit_value(then), lit_value(else_)) {
        (Some(1.0), Some(0.0)) => Some(Fix::Signal(receiver, false)),
        (Some(0.0), Some(1.0)) => Some(Fix::Signal(receiver, true)),
        _ => None,
    }
}

/// The rewrite `.opacity(<arg>)` implies, or `None` when `<arg>` is none of
/// the toggle shapes.
fn fix<'hir>(cx: &LateContext<'hir>, arg: &'hir Expr<'hir>) -> Option<Fix<'hir>> {
    let arg = tail(arg);
    if lit_value(arg) == Some(0.0) {
        return Some(Fix::False);
    }
    let ExprKind::MethodCall(_, receiver, args, _) = arg.kind else {
        return None;
    };
    let callee = implemented_trait_item(cx.tcx, call_def_id(cx.typeck_results(), arg)?);
    if def_path_eq(cx, callee, SELECT) {
        let [if_true, if_false] = args else {
            return None;
        };
        return match (lit_value(if_true), lit_value(if_false)) {
            (Some(1.0), Some(0.0)) => Some(Fix::Signal(receiver, false)),
            (Some(0.0), Some(1.0)) => Some(Fix::Signal(receiver, true)),
            _ => None,
        };
    }
    if def_path_eq(cx, callee, MAP)
        && let [func] = args
    {
        return map_fix(cx, receiver, func);
    }
    None
}

/// `expr` with explicit `&`-borrows and drop-temps peeled — `select`/`map`
/// take `&self`, so `(&sig).select(..)`'s HIR receiver carries the borrow the
/// source wrote.
fn unborrow<'hir>(mut expr: &'hir Expr<'hir>) -> (&'hir Expr<'hir>, bool) {
    let mut borrowed = false;
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::AddrOf(.., inner) => {
                borrowed = true;
                inner
            }
            _ => return (expr, borrowed),
        };
    }
}

/// Whether `.visible(..)` consuming `expr` would not compile where the
/// original call only borrowed it: a field, an index, a deref, a non-local
/// path, or a local the enclosing body still uses after `call`.
/// `Signal: Clone`, so the fix passes a clone in those cases.
fn needs_clone(cx: &LateContext<'_>, expr: &Expr<'_>, call: &Expr<'_>) -> bool {
    match expr.kind {
        ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::Unary(UnOp::Deref, _) => true,
        ExprKind::Path(QPath::Resolved(None, path)) => match path.res {
            Res::Local(local) => local_used_after_expr(cx, local, call),
            _ => true,
        },
        ExprKind::Path(..) => true,
        _ => false,
    }
}

/// Whether `expr`'s snippet can take a postfix `.method()`/`.field` without
/// parentheses.
fn postfixable(expr: &Expr<'_>) -> bool {
    matches!(
        expr.kind,
        ExprKind::Path(..)
            | ExprKind::Field(..)
            | ExprKind::Index(..)
            | ExprKind::Call(..)
            | ExprKind::MethodCall(..)
            | ExprKind::Lit(..)
            | ExprKind::Block(..)
            | ExprKind::Array(..)
            | ExprKind::Tup(..)
            | ExprKind::Struct(..)
    )
}

/// The `.visible(..)` argument for a `select`/`map` receiver: `.not()` for an
/// inverted toggle (it borrows, so the signal is never moved), a clone for a
/// receiver `visible` could not consume, else the snippet verbatim.
fn signal_text(
    cx: &LateContext<'_>,
    receiver: &Expr<'_>,
    call: &Expr<'_>,
    inverted: bool,
) -> Option<String> {
    let (expr, borrowed) = unborrow(receiver);
    let snippet = snippet_opt(cx, expr.span)?;
    let snippet = if postfixable(expr) {
        snippet
    } else {
        format!("({snippet})")
    };
    Some(if inverted {
        format!("{snippet}.not()")
    } else if borrowed || needs_clone(cx, expr, call) {
        format!("{snippet}.clone()")
    } else {
        snippet
    })
}

impl<'tcx> LateLintPass<'tcx> for OpacityAsVisibility {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(segment, _, [arg], _) = expr.kind else {
            return;
        };
        let Some(callee) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if !def_path_eq(cx, implemented_trait_item(cx.tcx, callee), OPACITY) {
            return;
        }
        let Some(fix) = fix(cx, arg) else {
            return;
        };
        // Replace `.opacity`-to-end so the receiver expression stays
        // untouched.
        let span = expr.span.with_lo(segment.ident.span.lo());
        let argument = match fix {
            Fix::False => "false".to_string(),
            Fix::Signal(receiver, inverted) => match signal_text(cx, receiver, expr, inverted) {
                Some(argument) => argument,
                None => {
                    span_lint_and_help(cx, OPACITY_AS_VISIBILITY, span, MESSAGE, None, SUGGESTION);
                    return;
                }
            },
        };
        span_lint_and_sugg(
            cx,
            OPACITY_AS_VISIBILITY,
            span,
            MESSAGE,
            SUGGESTION,
            format!("visible({argument})"),
            Applicability::MachineApplicable,
        );
    }
}
