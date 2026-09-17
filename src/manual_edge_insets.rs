//! `manual_edge_insets` — an `EdgeInsets` constructor passed straight to an
//! `impl Into<EdgeInsets>`/`impl IntoComputed<EdgeInsets>` parameter restates
//! a conversion the parameter performs itself.
//!
//! The `symmetric` → `(vertical, horizontal)` rewrite has one subtlety at an
//! `IntoComputed` position: the pair goes through nami's *heterogeneous*
//! `Signal` tuple impl, so its elements never unify, and `EdgeInsets`
//! converts only from `(f32, f32)`/`(f64, f64)`. A path operand that resolves
//! to a local binding (`let p = 8.0`) was typed `f32` only by `symmetric`'s
//! parameters and re-infers after the rewrite — whether the pair then needs
//! an `f32` suffix depends on the binding's other uses, which the lint does
//! not track, so any `Res::Local` operand demotes the suggestion to
//! `MaybeIncorrect` with the plain tuple text for a human to settle. A
//! `const`/`static`/associated-const path is fixed `f32`, so with no local
//! operand each unsuffixed float operand is spelled `f32` and the pair stays
//! `(f32, f32)` (`MachineApplicable`); a pair of unsuffixed floats falls back
//! to `(f64, f64)` and needs nothing. At a plain `Into` position the pair
//! always compiles — the single applicable `From` impl pins every inference
//! variable — so the rewrite stays `MachineApplicable` and unsuffixed there.

use clippy_utils::diagnostics::{span_lint_and_sugg, span_lint_and_then};
use clippy_utils::source::snippet_with_applicability;
use rustc_ast::{LitFloatType, LitKind};
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind, QPath, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint_pass;

use crate::applicability::comment_guard;
use crate::def_path::def_path_eq;
use crate::param_bounds::{
    BoundTarget, call_arg_all_bounds, call_args, call_def_id, implemented_trait_item,
};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `EdgeInsets::all(..)`, `EdgeInsets::symmetric(..)`, and
    /// `EdgeInsets::new(..)` passed straight to a parameter that takes the
    /// insets as a conversion — `impl Into<EdgeInsets>` or
    /// `impl IntoComputed<EdgeInsets>`, `.padding_with(..)` above all — when
    /// every operand is a literal or a path.
    ///
    /// ### Why is this bad?
    ///
    /// `EdgeInsets` converts from a single number (every edge), a
    /// `(vertical, horizontal)` pair, and a `[top, bottom, leading,
    /// trailing]` array, so the constructor names a conversion the caller
    /// never had to write. Inside `.padding_with(..)`, a `symmetric` call
    /// with one zero axis is `padding_horizontal`/`padding_vertical` written
    /// long.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text!("a").padding_with(EdgeInsets::symmetric(0.0, 12.0))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text!("a").padding_horizontal(12.0)
    /// ```
    pub MANUAL_EDGE_INSETS,
    style,
    "an `EdgeInsets` constructor where the parameter takes the value directly"
}

declare_lint_pass!(ManualEdgeInsets => [MANUAL_EDGE_INSETS]);

/// `waterui_layout::modifiers::padding::EdgeInsets`.
const EDGE_INSETS: &[&str] = &["waterui_layout", "modifiers", "padding", "EdgeInsets"];

/// `EdgeInsets::all`.
const ALL: &[&str] = &[
    "waterui_layout",
    "modifiers",
    "padding",
    "EdgeInsets",
    "all",
];

/// `EdgeInsets::symmetric`.
const SYMMETRIC: &[&str] = &[
    "waterui_layout",
    "modifiers",
    "padding",
    "EdgeInsets",
    "symmetric",
];

/// `EdgeInsets::new`.
const NEW: &[&str] = &[
    "waterui_layout",
    "modifiers",
    "padding",
    "EdgeInsets",
    "new",
];

/// `core::convert::Into` — the `Into<EdgeInsets>` parameter bound.
const INTO: &[&str] = &["core", "convert", "Into"];

/// `nami::reactive_core::signal::IntoComputed` — `padding_with`'s
/// `impl IntoComputed<EdgeInsets>` bound.
const INTO_COMPUTED: &[&str] = &["nami", "reactive_core", "signal", "IntoComputed"];

/// `core::marker::Sized` — implicit on every parameter, so it is not an
/// extra bound the rewrite goes unverified against.
const SIZED: &[&str] = &["core", "marker", "Sized"];

/// `waterui_internal::view::ViewExt::padding_with` — only here does a
/// `symmetric` call with one zero axis have a named modifier to collapse to
/// (`padding_horizontal`/`padding_vertical`).
const PADDING_WITH: &[&str] = &["waterui_internal", "view", "ViewExt", "padding_with"];

/// Whether `target` is an `Into<EdgeInsets>`/`IntoComputed<EdgeInsets>`
/// bound — a parameter position the rewrites are verified against.
fn edge_insets_bound(cx: &LateContext<'_>, target: &BoundTarget<'_>) -> bool {
    if !(def_path_eq(cx, target.trait_did, INTO)
        || def_path_eq(cx, target.trait_did, INTO_COMPUTED))
    {
        return false;
    }
    target
        .args
        .first()
        .and_then(|arg| arg.as_type())
        .is_some_and(|ty| {
            matches!(ty.kind(), TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), EDGE_INSETS))
        })
}

/// Whether `target` is a bound the rewrites are verified against — an
/// `EdgeInsets` conversion bound or `Sized`.
fn known_bound(cx: &LateContext<'_>, target: &BoundTarget<'_>) -> bool {
    edge_insets_bound(cx, target) || def_path_eq(cx, target.trait_did, SIZED)
}

/// `expr` is a path to a local binding (`let`/parameter/`match` arm): its
/// type is decided where it is used, so moving it into the pair changes what
/// it infers — a human must pick the suffix.
fn local_path(expr: &Expr<'_>) -> bool {
    matches!(
        expr.kind,
        ExprKind::Path(QPath::Resolved(None, path)) if matches!(path.res, Res::Local(_))
    )
}

/// `expr` is a literal — a negated one counts — or a path: the operand
/// shapes the fix may reuse verbatim.
fn reusable_operand(expr: &Expr<'_>) -> bool {
    match expr.kind {
        ExprKind::Lit(..) | ExprKind::Path(..) => true,
        ExprKind::Unary(UnOp::Neg, inner) => matches!(inner.kind, ExprKind::Lit(..)),
        _ => false,
    }
}

/// `expr` is the literal `0`/`0.0` — a `symmetric` operand that leaves its
/// axis unpadded.
fn zero_literal(expr: &Expr<'_>) -> bool {
    let ExprKind::Lit(lit) = expr.kind else {
        return false;
    };
    match lit.node {
        LitKind::Int(value, _) => value.get() == 0,
        LitKind::Float(sym, _) => sym.as_str() == "0.0",
        _ => false,
    }
}

/// `expr` is an unsuffixed float literal, optionally negated — the operand
/// shape that re-infers `f64` at a tuple position without `symmetric`'s
/// `f32` parameter types to pin it.
fn unsuffixed_float(expr: &Expr<'_>) -> bool {
    let lit = match expr.kind {
        ExprKind::Lit(lit) => lit,
        ExprKind::Unary(UnOp::Neg, inner) => match inner.kind {
            ExprKind::Lit(lit) => lit,
            _ => return false,
        },
        _ => return false,
    };
    matches!(lit.node, LitKind::Float(_, LitFloatType::Unsuffixed))
}

/// `text` — an unsuffixed float literal's spelling, possibly negated — with
/// the `f32` suffix pinned on the literal. A trailing `.` gets a `0` first:
/// `16.f32` does not lex as a literal.
fn f32_suffixed(text: &str) -> String {
    match text.strip_suffix('.') {
        Some(head) => format!("{head}.0f32"),
        None => format!("{text}f32"),
    }
}

impl ManualEdgeInsets {
    /// `arg` sits at an `EdgeInsets`-conversion parameter — flag the
    /// `EdgeInsets::<ctor>(..)` it spells, if it is one.
    fn check_arg<'tcx>(
        &mut self,
        cx: &LateContext<'tcx>,
        call: &'tcx Expr<'tcx>,
        callee: DefId,
        arg: &'tcx Expr<'tcx>,
        bound: &'static str,
        into_computed: bool,
    ) {
        if arg.span.from_expansion() {
            return;
        }
        let ExprKind::Call(_, operands) = arg.kind else {
            return;
        };
        let Some(did) = call_def_id(cx.typeck_results(), arg) else {
            return;
        };
        let is = |path: &[&'static str]| def_path_eq(cx, did, path);
        if !(is(ALL) || is(SYMMETRIC) || is(NEW)) {
            return;
        }
        if !operands.iter().all(|operand| reusable_operand(operand)) {
            return;
        }
        let mut applicability = Applicability::MachineApplicable;
        comment_guard(cx, arg.span, &mut applicability);
        let texts: Vec<String> = operands
            .iter()
            .map(|operand| {
                snippet_with_applicability(
                    cx,
                    operand.span.source_callsite(),
                    "..",
                    &mut applicability,
                )
                .into_owned()
            })
            .collect();
        let msg = format!("this parameter takes `impl {bound}`; pass the insets directly");
        if is(ALL)
            && let [x] = &texts[..]
        {
            span_lint_and_sugg(
                cx,
                MANUAL_EDGE_INSETS,
                arg.span,
                msg,
                "pass the value directly",
                x.clone(),
                applicability,
            );
            return;
        }
        if is(SYMMETRIC)
            && let [v, h] = &texts[..]
        {
            let (v_zero, h_zero) = (zero_literal(&operands[0]), zero_literal(&operands[1]));
            // `.padding_with(EdgeInsets::symmetric(0.0, x))` is
            // `.padding_horizontal(x)` — the whole call rewrites; any other
            // `symmetric` becomes the `(vertical, horizontal)` pair.
            if v_zero != h_zero
                && let ExprKind::MethodCall(seg, ..) = call.kind
                && def_path_eq(cx, implemented_trait_item(cx.tcx, callee), PADDING_WITH)
            {
                let (name, text) = if v_zero {
                    ("padding_horizontal", h.clone())
                } else {
                    ("padding_vertical", v.clone())
                };
                span_lint_and_then(cx, MANUAL_EDGE_INSETS, arg.span, msg, |diag| {
                    diag.multipart_suggestion(
                        format!("use `{name}`"),
                        vec![(seg.ident.span, name.to_string()), (arg.span, text)],
                        applicability,
                    );
                });
                return;
            }
            // Pair spellings are reasoned through in the module docs. At a
            // plain `Into` position the pair always compiles; at an
            // `IntoComputed` position a `Res::Local` operand re-infers, so
            // the suggestion demotes to `MaybeIncorrect` with the plain
            // text; otherwise unsuffixed floats next to a pinned operand
            // (a `const`/`static`/associated-const path or a suffixed
            // literal — every `symmetric` operand is `f32`) get `f32`.
            let pair = if !into_computed {
                format!("({v}, {h})")
            } else if operands.iter().any(|operand| local_path(operand)) {
                applicability = Applicability::MaybeIncorrect;
                format!("({v}, {h})")
            } else {
                let pin = operands.iter().any(|operand| !unsuffixed_float(operand))
                    && operands.iter().any(|operand| unsuffixed_float(operand));
                let operand_text = |operand: &Expr<'tcx>, text: &String| {
                    if pin && unsuffixed_float(operand) {
                        f32_suffixed(text)
                    } else {
                        text.clone()
                    }
                };
                format!(
                    "({}, {})",
                    operand_text(&operands[0], v),
                    operand_text(&operands[1], h)
                )
            };
            span_lint_and_sugg(
                cx,
                MANUAL_EDGE_INSETS,
                arg.span,
                msg,
                "use a `(vertical, horizontal)` pair",
                pair,
                applicability,
            );
            return;
        }
        if is(NEW)
            && let [t, b, l, r] = &texts[..]
        {
            span_lint_and_sugg(
                cx,
                MANUAL_EDGE_INSETS,
                arg.span,
                msg,
                "use a `[top, bottom, leading, trailing]` array",
                format!("[{t}, {b}, {l}, {r}]"),
                applicability,
            );
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for ManualEdgeInsets {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        let args = call_args(expr);
        // Only an `EdgeInsets::<ctor>(..)` call in argument position can
        // fire — a cheap shape test before resolving the callee's bounds.
        if !args
            .iter()
            .any(|arg| matches!(arg.kind, ExprKind::Call(..)))
        {
            return;
        }
        let Some((callee, per_arg)) = call_arg_all_bounds(cx, expr) else {
            return;
        };
        for (arg, targets) in args.into_iter().zip(per_arg) {
            // Every bound on the parameter must be one the rewrite is
            // verified against — an extra bound (`impl Into<EdgeInsets> +
            // Debug`) could leave the fixed code not compiling.
            if !targets.iter().any(|target| edge_insets_bound(cx, target))
                || !targets.iter().all(|target| known_bound(cx, target))
            {
                continue;
            }
            let into_computed = targets
                .iter()
                .any(|target| def_path_eq(cx, target.trait_did, INTO_COMPUTED));
            let bound = if into_computed {
                "IntoComputed<EdgeInsets>"
            } else {
                "Into<EdgeInsets>"
            };
            self.check_arg(cx, expr, callee, arg, bound, into_computed);
        }
    }
}
