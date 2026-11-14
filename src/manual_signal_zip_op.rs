//! `manual_signal_zip_op` — `a.zip(&b).map(|(x, y)| x op y)` restates a
//! named signal operation.

use clippy_utils::get_parent_expr;
use clippy_utils::res::MaybeResPath;
use clippy_utils::sugg::{Sugg, make_binop};
use clippy_utils::ty::implements_trait;
use rustc_errors::Applicability;
use rustc_hir::{BinOpKind, Expr, ExprKind, HirId, LangItem, MatchSource, PatKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeVisitableExt;
use rustc_session::declare_lint_pass;

use crate::carriers::{body_expr, strip_wraps};
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_sugg;
use crate::param_bounds::implemented_trait_item;
use crate::receiver::{owned_spelling, ref_depth};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `a.zip(&b).map(..)` whose closure combines the pair with a
    /// two-operand expression a named signal operation already spells:
    ///
    /// - `x && y` on `bool` outputs → `a.and(&b)`
    /// - `x || y` on `bool` outputs → `a.or(&b)`
    /// - `x op y` for `+ - * / % & | ^ << >>` → `a op b`
    ///
    /// The closure's one parameter may be a `(x, y)` tuple pattern or a
    /// single binding read only as `t.0`/`t.1`. Operands in swapped order
    /// (`y && x`), a component read more than once (`x + x`), and any
    /// other body stay silent, as do `Iterator::zip`/`Option::zip` chains.
    ///
    /// ### Why is this bad?
    ///
    /// `and`/`or` and the `impl_signal_ops!` operator overloads map through
    /// `fn` items, so `a.and(&b)` and `a + b` share the canonical
    /// `(fn, Output)` `SignalIdentity` wherever they are reconstructed. A
    /// hand-written `zip`/`map` closure is its own type and reads as a
    /// bespoke transform where the operation's documented name belongs.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let both = a.zip(&b).map(|(x, y)| x && y);
    /// let total = count.zip(&other).map(|(x, y)| x + y);
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let both = a.and(&b);
    /// let total = count.clone() + other.clone();
    /// ```
    pub MANUAL_SIGNAL_ZIP_OP,
    style,
    "a `zip`/`map` pair that restates a named signal operation"
}

const MESSAGE: &str = "this `zip`/`map` pair is a named signal operation";

/// `SignalExt::map` — the blanket `impl<C: Signal> SignalExt for C` puts it
/// on every signal, so a `MethodCall` resolving here is a signal transform.
const SIGNAL_EXT_MAP: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "map"];

/// `SignalExt::zip` — `a.zip(&b)` produces the `(A, B)` pair the `map`
/// closure combines.
const SIGNAL_EXT_ZIP: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "zip"];

/// The operation a `x op y` body spells — `and`/`or` borrow both signals
/// (`&self`, `&B`); the arithmetic and bitwise operators consume them.
enum Operation {
    /// `x && y` → `and`; `x || y` → `or`.
    Bool(&'static str),
    /// `x op y` → `a op b`.
    Op(BinOpKind),
}

/// How the `map` closure's single parameter reads the zipped pair.
#[derive(Clone, Copy)]
enum PairRead {
    /// `(x, y)` — a tuple pattern binding each component.
    Destructured(HirId, HirId),
    /// `t` — a binding read through `.0`/`.1` projections.
    Indexed(HirId),
}

/// `expr` is exactly the `index`th component read of the pair — `x`/`y`
/// for a destructured pattern, `t.0`/`t.1` for a binding.
fn reads_component(read: PairRead, expr: &Expr<'_>, index: usize) -> bool {
    match read {
        PairRead::Destructured(x, y) => expr.res_local_id() == Some(if index == 0 { x } else { y }),
        PairRead::Indexed(t) => {
            let name = if index == 0 { "0" } else { "1" };
            matches!(expr.kind, ExprKind::Field(base, ident)
                if base.res_local_id() == Some(t) && ident.name.as_str() == name)
        }
    }
}

/// The `core::ops` lang item behind `op` — the `impl_signal_binary_ops!`
/// set nami puts on its signal types (`Binding`, `Computed`, `Map`,
/// `Constant`, `Lazy`, `Cached`, `WithMetadata`).
fn op_lang_item(op: BinOpKind) -> Option<LangItem> {
    Some(match op {
        BinOpKind::Add => LangItem::Add,
        BinOpKind::Sub => LangItem::Sub,
        BinOpKind::Mul => LangItem::Mul,
        BinOpKind::Div => LangItem::Div,
        BinOpKind::Rem => LangItem::Rem,
        BinOpKind::BitAnd => LangItem::BitAnd,
        BinOpKind::BitOr => LangItem::BitOr,
        BinOpKind::BitXor => LangItem::BitXor,
        BinOpKind::Shl => LangItem::Shl,
        BinOpKind::Shr => LangItem::Shr,
        _ => return None,
    })
}

/// `expr` sits where a bare `a op b` would rebind — a method receiver, a
/// field/index base, a call callee, a unary/`&`/`as`/`.await` operand, or
/// either side of a binary operator — so the rewrite needs `(...)`.
fn needs_parens(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let mut node = expr;
    let parent = loop {
        let Some(parent) = get_parent_expr(cx, node) else {
            return false;
        };
        if matches!(parent.kind, ExprKind::DropTemps(..)) {
            node = parent;
        } else {
            break parent;
        }
    };
    match parent.kind {
        ExprKind::MethodCall(_, receiver, _, _) => receiver.hir_id == node.hir_id,
        ExprKind::Call(callee, _) | ExprKind::Index(callee, _, _) => callee.hir_id == node.hir_id,
        ExprKind::Field(..)
        | ExprKind::Unary(..)
        | ExprKind::AddrOf(..)
        | ExprKind::Cast(..)
        | ExprKind::Binary(..)
        | ExprKind::Yield(..) => true,
        ExprKind::Match(_, _, MatchSource::AwaitDesugar) => true,
        _ => false,
    }
}

impl<'tcx> LateLintPass<'tcx> for ManualSignalZipOp {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(_, receiver, [func], _) = expr.kind else {
            return;
        };
        let typeck = cx.typeck_results();
        if !typeck
            .type_dependent_def_id(expr.hir_id)
            .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), SIGNAL_EXT_MAP))
        {
            return;
        }
        // The `map` must sit on a `SignalExt::zip` — `a.zip(&b)` — whose
        // argument peels (`&`, `.clone()`, parens) to the second signal `b`.
        let ExprKind::MethodCall(_, a, [zip_arg], _) = receiver.kind else {
            return;
        };
        if !typeck
            .type_dependent_def_id(receiver.hir_id)
            .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), SIGNAL_EXT_ZIP))
        {
            return;
        }
        let b = strip_wraps(cx, typeck, zip_arg);
        let mut func = func;
        while let ExprKind::DropTemps(inner) = func.kind {
            func = inner;
        }
        let ExprKind::Closure(closure) = func.kind else {
            return;
        };
        let body = cx.tcx.hir_body(closure.body);
        let [param] = body.params else {
            return;
        };
        let read = match param.pat.kind {
            PatKind::Tuple([px, py], _) => {
                let (PatKind::Binding(_, x, _, None), PatKind::Binding(_, y, _, None)) =
                    (px.kind, py.kind)
                else {
                    return;
                };
                PairRead::Destructured(x, y)
            }
            PatKind::Binding(_, t, _, None) => PairRead::Indexed(t),
            _ => return,
        };
        // The closure is a nested body; its expressions are typed through
        // `typeck_body`, not the enclosing body's table.
        let body_typeck = cx.tcx.typeck_body(closure.body);
        let ExprKind::Binary(op, lhs, rhs) = body_expr(body.value).kind else {
            return;
        };
        // The operands must read the pair in order — `y && x`, `x + x`,
        // and any other use of the components stay silent.
        if !(reads_component(read, lhs, 0) && reads_component(read, rhs, 1)) {
            return;
        }
        let operation = match op.node {
            BinOpKind::And | BinOpKind::Or
                if body_typeck.expr_ty(lhs).is_bool() && body_typeck.expr_ty(rhs).is_bool() =>
            {
                Operation::Bool(if op.node == BinOpKind::And {
                    "and"
                } else {
                    "or"
                })
            }
            _ => {
                // `a op b` needs `A: Op<B>`: the closure proves `X: Op<Y>`
                // on the outputs, but the signal impl additionally requires
                // the receiver type to carry the overload — a `Signal` type
                // nami did not write would leave the suggestion uncompilable.
                let a_ty = typeck.expr_ty(a).peel_refs();
                let b_ty = typeck.expr_ty(b).peel_refs();
                let Some(op_trait) =
                    op_lang_item(op.node).and_then(|item| cx.tcx.lang_items().get(item))
                else {
                    return;
                };
                if a_ty.has_infer()
                    || b_ty.has_infer()
                    || !implements_trait(cx, a_ty, op_trait, &[b_ty.into()])
                {
                    return;
                }
                Operation::Op(op.node)
            }
        };
        let mut applicability = Applicability::MachineApplicable;
        let a_sugg = Sugg::hir_with_applicability(cx, a, "..", &mut applicability);
        let b_sugg = Sugg::hir_with_applicability(cx, b, "..", &mut applicability);
        let sugg = match operation {
            Operation::Bool(name) => {
                // `and`/`or` take `&self`/`&B`: `a` stays verbatim and `b`
                // is re-borrowed — unless `zip` already took it as `&B`.
                let other = match ref_depth(typeck.expr_ty(b)) {
                    0 => format!("&{}", b_sugg.maybe_paren()),
                    1 => b_sugg.to_string(),
                    _ => format!("*{}", b_sugg.maybe_paren()),
                };
                format!("{}.{}({other})", a_sugg.maybe_paren(), name)
            }
            Operation::Op(kind) => {
                // The operands are spelled as atoms already; wrap the join in
                // a `Sugg` so `.maybe_paren()` can add `(...)` where the
                // surrounding expression would rebind a bare `a op b`.
                let sugg = make_binop(
                    kind,
                    &Sugg::NonParen(owned_spelling(typeck, a, a_sugg).into()),
                    &Sugg::NonParen(owned_spelling(typeck, b, b_sugg).into()),
                );
                if needs_parens(cx, expr) {
                    sugg.maybe_paren().into_string()
                } else {
                    sugg.into_string()
                }
            }
        };
        span_lint_and_sugg(
            cx,
            MANUAL_SIGNAL_ZIP_OP,
            expr.span,
            MESSAGE,
            "use the signal operation",
            sugg,
            applicability,
        );
    }
}

declare_lint_pass!(ManualSignalZipOp => [MANUAL_SIGNAL_ZIP_OP]);
