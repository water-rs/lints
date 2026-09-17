use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::res::MaybeQPath;
use clippy_utils::source::snippet_opt;
use rustc_ast::{LitFloatType, LitIntType, LitKind};
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::{Expr, ExprKind, Node};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::{Ty, TyKind, TypeVisitableExt};
use rustc_session::declare_lint_pass;
use rustc_span::Span;
use rustc_span::symbol::Symbol;

use crate::binding::{binding_krate, dedicated_ctor};
use crate::imports::{Bare, bare_status, use_insertion};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `binding(..)` and `Binding::container(..)` calls whose result
    /// type is `Binding<T>` with `T` one of `u32`, `u64`, `usize`, `i32`,
    /// `i64`, `isize`, `f32`, `f64`, or `bool` — the primitives `nami` gives
    /// a dedicated `Binding::<t>` constructor.
    ///
    /// ### Why is this bad?
    ///
    /// The generic spellings say the type twice (`let b: Binding<i32> =
    /// binding(0)`) or leave it to inference (`binding(0)`); `Binding::i32(0)`
    /// says it once, at the call.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let count: Binding<i32> = binding(0);
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let count = Binding::i32(0);
    /// ```
    pub TYPED_BINDING_CONSTRUCTOR,
    style,
    "`binding(..)`/`Binding::container(..)` for a primitive is `Binding::<t>(..)`"
}

declare_lint_pass!(TypedBindingConstructor => [TYPED_BINDING_CONSTRUCTOR]);

/// `nami`'s generic binding constructors — `binding(..)` and
/// `Binding::container(..)` — as `LateContext::get_def_path` segments.
const GENERIC_CTORS: &[&[&str]] = &[
    &["nami", "reactive_core", "binding", "binding"],
    &["nami", "reactive_core", "binding", "Binding", "container"],
];

/// The argument half of the rewrite. `binding` takes `impl Into<T>` but
/// `Binding::<t>` takes `T` itself, so the source is reusable verbatim only
/// when it already has type `T` or is an unsuffixed numeric literal that
/// re-infers to `T`. Returns `None` to leave the call alone (an argument of
/// a different `Into<T>` type, like `binding(x_i32)` for `Binding<i64>`),
/// `Some(None)` to keep the source, and `Some(Some(..))` to rewrite it — a
/// literal suffix naming `T` is dropped (`0_i32` → `0`), except a float
/// suffix whose removal would leave an integer literal (`3f32` keeps its
/// suffix).
fn arg_fix<'tcx>(
    cx: &LateContext<'tcx>,
    arg: &Expr<'tcx>,
    value_ty: Ty<'tcx>,
    ctor: &str,
) -> Option<Option<(Span, String)>> {
    if cx.typeck_results().expr_ty(arg) == value_ty {
        if let ExprKind::Lit(lit) = arg.kind
            && matches!(lit.node, LitKind::Int(..) | LitKind::Float(..))
            && let Some(text) = snippet_opt(cx.sess(), arg.span)
            && let Some(base) = text.strip_suffix(ctor)
        {
            let base = base.trim_end_matches('_');
            // `3f32` minus `f32` is the integer `3` — keep the suffix.
            let strippable =
                !matches!(lit.node, LitKind::Float(..)) || base.contains(['.', 'e', 'E']);
            return Some(strippable.then(|| (arg.span, base.to_owned())));
        }
        return Some(None);
    }
    match arg.kind {
        ExprKind::Lit(lit) => match lit.node {
            LitKind::Int(_, LitIntType::Unsuffixed)
            | LitKind::Float(_, LitFloatType::Unsuffixed) => Some(None),
            _ => None,
        },
        _ => None,
    }
}

/// `expr` is the whole initializer of `let <pat>: Binding<T> = expr` — the
/// annotation is redundant once the call names `T`, so return the
/// `: Binding<T>` span to erase. `None` without an annotation or when it is
/// anything but that exact path (a type alias, a projection, a tuple).
fn redundant_annotation(cx: &LateContext<'_>, expr: &Expr<'_>) -> Option<Span> {
    let mut hir_id = expr.hir_id;
    let local = loop {
        match cx.tcx.parent_hir_node(hir_id) {
            Node::Expr(parent) if matches!(parent.kind, ExprKind::DropTemps(_)) => {
                hir_id = parent.hir_id;
            }
            Node::LetStmt(local) => break local,
            _ => return None,
        }
    };
    if local.init.is_none_or(|init| init.hir_id != hir_id) {
        return None;
    }
    let ty = local.ty?;
    // The call can sit at a macro call site while the `let` lives in the
    // `macro_rules!` body — erasing the annotation there would edit the
    // definition, once per expansion.
    if local.span.from_expansion() || ty.span.from_expansion() {
        return None;
    }
    if !matches!(ty.kind, rustc_hir::TyKind::Path(..))
        || !matches!(ty.res(cx), Res::Def(DefKind::Struct, did)
            if crate::def_path::def_path_eq(cx, did, crate::binding::BINDING))
    {
        return None;
    }
    Some(ty.span.with_lo(local.pat.span.hi()))
}

impl<'tcx> LateLintPass<'tcx> for TypedBindingConstructor {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::Call(func, [arg]) = expr.kind else {
            return;
        };
        let Res::Def(_, did) = func.res(cx) else {
            return;
        };
        if !GENERIC_CTORS
            .iter()
            .any(|path| crate::def_path::def_path_eq(cx, did, path))
        {
            return;
        }
        let typeck = cx.typeck_results();
        let TyKind::Adt(adt, args) = *typeck.expr_ty(expr).kind() else {
            return;
        };
        if !crate::def_path::def_path_eq(cx, adt.did(), crate::binding::BINDING) {
            return;
        }
        let value_ty = args.type_at(0);
        if value_ty.has_infer() {
            return;
        }
        let Some(ctor) = dedicated_ctor(value_ty) else {
            return;
        };
        let Some(arg_fix) = arg_fix(cx, arg, value_ty, ctor) else {
            return;
        };

        let status = bare_status(
            cx,
            expr.hir_id,
            expr.span.lo(),
            Symbol::intern("Binding"),
            Namespace::TypeNS,
            Some(adt.did()),
        );
        let krate = binding_krate(cx);
        let mut parts: Vec<(Span, String)> = Vec::new();
        let mut applicable = Applicability::MachineApplicable;
        let ctor_text = match status {
            Bare::Same => Some(format!("Binding::{ctor}")),
            Bare::Free => krate.map(|krate| {
                match use_insertion(cx, expr.hir_id, &format!("{krate}::Binding")) {
                    Some((point, before, after)) => {
                        parts.push((point, format!("{before}use {krate}::Binding;{after}")));
                        format!("Binding::{ctor}")
                    }
                    // The module's `use` position is in an expansion — spell
                    // the qualified path instead.
                    None => {
                        applicable = Applicability::MaybeIncorrect;
                        format!("{krate}::Binding::{ctor}")
                    }
                }
            }),
            // `Binding` is taken (or a local glob leaves it unclear) —
            // qualify instead of importing.
            Bare::Conflict | Bare::Unknown => krate.map(|krate| {
                applicable = Applicability::MaybeIncorrect;
                format!("{krate}::Binding::{ctor}")
            }),
        };
        span_lint_and_then(
            cx,
            TYPED_BINDING_CONSTRUCTOR,
            expr.span,
            "this binding has a dedicated constructor",
            |diag| match ctor_text {
                Some(ctor_text) => {
                    parts.push((func.span, ctor_text));
                    if let Some((span, text)) = arg_fix {
                        parts.push((span, text));
                    }
                    if let Some(span) = redundant_annotation(cx, expr) {
                        parts.push((span, String::new()));
                    }
                    diag.multipart_suggestion(format!("use `Binding::{ctor}`"), parts, applicable);
                }
                // Neither `waterui` nor `nami` is nameable here — the rewrite
                // cannot be spelled, so no suggestion is attached.
                None => {
                    diag.help(format!("use `Binding::{ctor}(..)`"));
                }
            },
        );
    }
}
