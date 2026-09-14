use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::res::MaybeResPath;
use clippy_utils::source::snippet_opt;
use clippy_utils::usage::local_used_in;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;
use rustc_span::Symbol;

use crate::binding::BINDING_SET;
use crate::carriers::strip_wraps;
use crate::def_path::def_path_eq;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `.on_change(&source, |v| b.set(f(v)))` — an `on_change` handler
    /// whose body is a single `Binding::set` of a value computed from the
    /// watched one.
    ///
    /// ### Why is this bad?
    ///
    /// The `set` runs after `source` publishes its change, so `b` lags it by
    /// one update and becomes a second source of truth that can disagree with
    /// the derivation it copied. A value derived from a signal is
    /// `source.map(f)` — a `Computed` — not an imperative copy.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("a").on_change(&count, move |v| doubled.set(v * 2));
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let doubled = count.map(|v| v * 2);
    /// ```
    pub ON_CHANGE_DERIVES_BINDING,
    pedantic,
    "an `on_change` handler that copies a derived value into a second binding"
}

declare_lint_pass!(OnChangeDerivesBinding => [ON_CHANGE_DERIVES_BINDING]);

/// `ViewExt::on_change` — `on_change(self, source: &C, handler: F)` with
/// `F: Fn(C::Output)`.
const ON_CHANGE: &[&str] = &["waterui_internal", "view", "ViewExt", "on_change"];

const MESSAGE: &str = "this `on_change` copies a derived value into a second binding";
const FALLBACK_HELP: &str = "derive the value once with `.map(..)` on the source and read the `Computed` where the second binding was read";

/// The name a `let` for the derived value should take: the `set` receiver's
/// local or field name (`doubled` for `doubled` / `self.doubled`).
fn derived_name(stripped: &Expr<'_>) -> Option<Symbol> {
    if let Some((_, ident)) = stripped.res_local_id_and_ident() {
        Some(ident.name)
    } else if let ExprKind::Field(_, field) = stripped.kind {
        Some(field.name)
    } else {
        None
    }
}

impl<'tcx> LateLintPass<'tcx> for OnChangeDerivesBinding {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() || !matches!(expr.kind, ExprKind::MethodCall(..)) {
            return;
        }
        let Some(did) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        if !def_path_eq(cx, implemented_trait_item(cx.tcx, did), ON_CHANGE) {
            return;
        }
        let &[_, source, handler] = call_args(expr).as_slice() else {
            return;
        };
        let ExprKind::Closure(closure) = handler.kind else {
            return;
        };
        let body = cx.tcx.hir_body(closure.body);
        let [param] = body.params else {
            return;
        };
        // The single-set shape: `|v| b.set(..)` or `|v| { b.set(..) }` — a
        // block with statements is more than one effect and stays silent.
        let mut value = body.value;
        let set = loop {
            value = match value.kind {
                ExprKind::DropTemps(inner) => inner,
                ExprKind::Block(block, _) if block.stmts.is_empty() => {
                    let Some(tail) = block.expr else { return };
                    tail
                }
                _ => break value,
            };
        };
        if !matches!(set.kind, ExprKind::MethodCall(..)) {
            return;
        }
        // `set` lives in the closure's body, not the body `cx` is checking.
        let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(set.hir_id));
        let Some(set_did) = call_def_id(typeck, set) else {
            return;
        };
        if !def_path_eq(cx, implemented_trait_item(cx.tcx, set_did), BINDING_SET) {
            return;
        }
        let &[receiver, arg] = call_args(set).as_slice() else {
            return;
        };
        // `|_|` binds nothing; any binding the parameter pattern makes must
        // reach the `set` argument for the stored value to derive from `v`.
        let mut bindings = Vec::new();
        param
            .pat
            .each_binding(|_, hir_id, _, _| bindings.push(hir_id));
        if !bindings.iter().any(|&id| local_used_in(cx, id, arg)) {
            return;
        }
        span_lint_and_then(cx, ON_CHANGE_DERIVES_BINDING, expr.span, MESSAGE, |diag| {
            let stripped = strip_wraps(cx, typeck, receiver);
            let name = derived_name(stripped);
            let b = snippet_opt(cx, stripped.span)
                .or_else(|| name.map(|name| name.as_str().to_owned()));
            diag.span_label(
                handler.span,
                match &b {
                    Some(b) => format!(
                        "`{b}` lags the source by one update and is a second source of truth"
                    ),
                    None => {
                        "this binding lags the source by one update and is a second source of truth"
                            .to_owned()
                    }
                },
            );
            let source = strip_wraps(cx, cx.typeck_results(), source);
            match (
                name,
                snippet_opt(cx, source.span),
                snippet_opt(cx, param.pat.span),
                snippet_opt(cx, arg.span),
            ) {
                (Some(name), Some(source), Some(pat), Some(arg)) => {
                    diag.help(format!(
                        "derive it once — `let {name} = {source}.map(|{pat}| {arg});` — and read the `Computed` where `{name}` was read"
                    ));
                }
                _ => {
                    diag.help(FALLBACK_HELP);
                }
            }
        });
    }
}
