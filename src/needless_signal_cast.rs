//! `needless_signal_cast` — `.map(|v| v as f32)`-style conversions on a
//! signal that only ever reaches an `impl IntoSignalF32` parameter.

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::MaybeResPath;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind, HirId, LetStmt, Node, PatKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{FloatTy, Ty, TyKind, TypeVisitableExt, TypeckResults};
use rustc_session::declare_lint_pass;
use std::ops::ControlFlow;

use crate::applicability::comment_guard;
use crate::carriers::{CLONE, FROM, INTO, body_expr, resolves_to};
use crate::def_path::def_path_eq;
use crate::param_bounds::{arg_has_bound, call_args, implemented_trait_item};
use crate::receiver::receiver_needs_clone;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `signal.map(..)` calls whose argument is a pure numeric
    /// conversion to `f32` — `|v| v as f32`, `|v| f32::from(v)`, `f32::from`,
    /// `|v| v.into()` when `f32` is inferred, `IntoF32::into_f32`, and
    /// `|v| v.to_f32().unwrap()` — when the produced signal only ever reaches
    /// an `impl IntoSignalF32` parameter: as the direct argument, through
    /// value-keeping `SignalExt` adapters (`.with(..)`, `.cached()`,
    /// `.computed()`), or through a `let` binding whose every use is such an
    /// argument.
    ///
    /// The fix deletes the `.map(..)` segment; when the receiver is a place
    /// that must not be moved — a local read again later, a local bound
    /// outside an enclosing loop (the next iteration reads it again), a
    /// field, an index, a deref — the segment becomes `.clone()` instead, so
    /// the parameter still converts while the receiver keeps its value.
    ///
    /// ### Why is this bad?
    ///
    /// `IntoSignalF32` is implemented for every signal whose output is
    /// `IntoF32` (`f32`, `f64`, every integer type), and `into_signal_f32` is
    /// the `map(IntoF32::into_f32)` the caller would otherwise write —
    /// metadata attached with `.with(..)` survives the internal map the same
    /// way. The cast names a conversion the parameter already performs.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let animated = radius
    ///     .map(|v| v as f32)
    ///     .with(Animation::ease_in_out(Duration::from_millis(300)));
    /// image.opacity(animated)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let animated = radius.with(Animation::ease_in_out(Duration::from_millis(300)));
    /// image.opacity(animated)
    /// ```
    pub NEEDLESS_SIGNAL_CAST,
    style,
    "a `map` to `f32` on a signal that only feeds `impl IntoSignalF32` parameters"
}

declare_lint_pass!(NeedlessSignalCast => [NEEDLESS_SIGNAL_CAST]);

const MESSAGE: &str =
    "this conversion is performed by the `impl IntoSignalF32` parameter; pass the signal as it is";
const SUGGESTION: &str = "remove the `.map(..)` conversion";
const SUGGESTION_CLONE: &str = "replace the `.map(..)` conversion with `.clone()`";

/// `SignalExt::map` — the blanket `impl<C: Signal> SignalExt for C` puts it
/// on every signal, so a `MethodCall` resolving here is a signal transform.
const SIGNAL_EXT_MAP: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "map"];

/// `SignalExt` adapters that keep the mapped value — `.with(..)` attaches
/// metadata, `.cached()`/`.computed()` rewrap the same output — plus
/// `.clone()`, which copies the cheap handle. A `map`/`zip`/transform is
/// absent: it changes the value, so the cast stays observable through it.
const KEEPING_ADAPTERS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "with"],
    &["nami", "reactive_core", "ext", "SignalExt", "cached"],
    &["nami", "reactive_core", "ext", "SignalExt", "computed"],
    CLONE,
];

/// `waterui_core::state::computed_f32::IntoSignalF32` — the bound whose
/// parameter performs the conversion itself.
const INTO_SIGNAL_F32: &[&str] = &["waterui_core", "state", "computed_f32", "IntoSignalF32"];
const INTO_SIGNAL_F32_BOUNDS: &[&[&str]] = &[INTO_SIGNAL_F32];

/// `IntoF32`, resolved by path — the impl set (`f32`, `f64`, `i8`–`i64`,
/// `isize`, `u8`–`u64`, `usize`) a removed `map` leaves the parameter to
/// perform.
const INTO_F32: &str = "waterui_core::state::computed_f32::IntoF32";

/// `IntoF32::into_f32` — the function `into_signal_f32` maps through.
const INTO_F32_METHOD: &[&str] = &[
    "waterui_core",
    "state",
    "computed_f32",
    "IntoF32",
    "into_f32",
];

/// `ToPrimitive::to_f32` — the `|v| v.to_f32().unwrap()` shape. The trait is
/// defined in `num_traits::cast` and re-exported at the root.
const TO_F32: &[&str] = &["num_traits", "cast", "ToPrimitive", "to_f32"];

/// `Option::unwrap` — the tail of `|v| v.to_f32().unwrap()`.
const OPTION_UNWRAP: &[&str] = &["core", "option", "Option", "unwrap"];

/// `ty` is `f32`.
fn is_f32(ty: Ty<'_>) -> bool {
    matches!(ty.kind(), TyKind::Float(FloatTy::F32))
}

/// Whether the closure-body `body` is `|v| <conversion to f32>` and nothing
/// more: `v as f32`, `f32::from(v)`/`IntoF32::into_f32(v)`, `v.into()` when
/// `f32` is inferred, `v.to_f32().unwrap()`. Arithmetic, a `.clamp(..)`, or a
/// second call over the conversion fails the match — those read the cast
/// value, which the parameter does not reproduce. `cty` is the closure
/// body's `TypeckResults`.
fn pure_f32_conversion<'hir>(
    cx: &LateContext<'hir>,
    cty: &TypeckResults<'hir>,
    body: &'hir Expr<'hir>,
    param: HirId,
) -> bool {
    if !is_f32(cty.expr_ty(body)) {
        return false;
    }
    match body.kind {
        // `v as f32`
        ExprKind::Cast(inner, _) => inner.res_local_id() == Some(param),
        // `f32::from(v)` / `IntoF32::into_f32(v)`
        ExprKind::Call(callee, [arg]) => {
            arg.res_local_id() == Some(param)
                && match callee.kind {
                    ExprKind::Path(qpath) => match cty.qpath_res(&qpath, callee.hir_id) {
                        Res::Def(DefKind::AssocFn, did) => {
                            let item = implemented_trait_item(cx.tcx, did);
                            [FROM, INTO_F32_METHOD]
                                .iter()
                                .any(|path| def_path_eq(cx, item, path))
                        }
                        _ => false,
                    },
                    _ => false,
                }
        }
        ExprKind::MethodCall(_, receiver, [], _) => {
            // `v.into()` — the `f32` target checked above is what `into`
            // inferred.
            if resolves_to(cx, cty, body, &[INTO]) {
                return receiver.res_local_id() == Some(param);
            }
            // `v.to_f32().unwrap()`
            if resolves_to(cx, cty, body, &[OPTION_UNWRAP])
                && let ExprKind::MethodCall(_, inner, [], _) = receiver.kind
            {
                return inner.res_local_id() == Some(param)
                    && resolves_to(cx, cty, receiver, &[TO_F32]);
            }
            false
        }
        _ => false,
    }
}

/// The input `func` — a function path like `f32::from` or
/// `IntoF32::into_f32` given straight to `map` — converts from, when `func`
/// is one of the named conversions and its signature maps one argument to
/// `f32`. `typeck` must be the `TypeckResults` of the body containing
/// `func`.
fn conversion_fn_input<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    func: &'tcx Expr<'tcx>,
) -> Option<Ty<'tcx>> {
    let ExprKind::Path(qpath) = func.kind else {
        return None;
    };
    let Res::Def(DefKind::AssocFn, did) = typeck.qpath_res(&qpath, func.hir_id) else {
        return None;
    };
    let item = implemented_trait_item(cx.tcx, did);
    if ![FROM, INTO_F32_METHOD]
        .iter()
        .any(|path| def_path_eq(cx, item, path))
    {
        return None;
    }
    let sig = cx
        .tcx
        .fn_sig(did)
        .instantiate(cx.tcx, typeck.node_args(func.hir_id))
        .skip_norm_wip()
        .skip_binder();
    let [input] = sig.inputs() else { return None };
    is_f32(sig.output()).then_some(*input)
}

/// Where the signal value `expr` ends up: an argument position of a call,
/// the initializer of a `let`, or neither. `DropTemps` and the value-keeping
/// adapters (`with`, `cached`, `computed`, `clone` on the receiver) are
/// climbed; everything else — a transform like a second `.map`, a field, a
/// scrutinee, a `let` `expr` only partly initializes — ends the walk.
enum Sink<'hir> {
    /// `expr` (through adapters) sits at argument `index` of `call`.
    Arg(&'hir Expr<'hir>, usize),
    /// `expr` (through adapters) initializes `local`.
    Let(&'hir LetStmt<'hir>),
    /// Consumed any other way.
    End,
}

fn sink<'hir>(cx: &LateContext<'hir>, expr: &'hir Expr<'hir>) -> Sink<'hir> {
    let mut current = expr;
    loop {
        match cx.tcx.parent_hir_node(current.hir_id) {
            Node::Expr(parent) => match parent.kind {
                ExprKind::DropTemps(inner) if inner.hir_id == current.hir_id => current = parent,
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    let args = call_args(parent);
                    let Some(index) = args.iter().position(|arg| arg.hir_id == current.hir_id)
                    else {
                        // `current` is the callee, not an operand.
                        return Sink::End;
                    };
                    if index == 0 {
                        let typeck = cx
                            .tcx
                            .typeck(cx.tcx.hir_enclosing_body_owner(parent.hir_id));
                        if resolves_to(cx, typeck, parent, KEEPING_ADAPTERS) {
                            current = parent;
                            continue;
                        }
                    }
                    return Sink::Arg(parent, index);
                }
                _ => return Sink::End,
            },
            Node::LetStmt(local)
                if local.init.is_some_and(|init| init.hir_id == current.hir_id) =>
            {
                return Sink::Let(local);
            }
            _ => return Sink::End,
        }
    }
}

/// `call`'s parameter at `index` is bounded by `IntoSignalF32`.
fn reaches_f32_param<'hir>(cx: &LateContext<'hir>, call: &'hir Expr<'hir>, index: usize) -> bool {
    let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(call.hir_id));
    arg_has_bound(cx, typeck, call, &[INTO_SIGNAL_F32_BOUNDS], index)
}

/// Whether every use of the `local` pattern's binding lands at an
/// `IntoSignalF32` parameter — the only shape under which dropping the `map`
/// keeps every consumer compiling. Uses inside nested closures count too.
/// `None` when the binding is never used — a dead `map` is not this lint's
/// case.
fn let_only_feeds_f32_params<'tcx>(cx: &LateContext<'tcx>, local: &LetStmt<'tcx>) -> Option<usize> {
    let PatKind::Binding(_, pat, _, None) = local.pat.kind else {
        return None;
    };
    let owner = cx.tcx.hir_enclosing_body_owner(local.hir_id);
    let body = cx.tcx.hir_body_owned_by(owner);
    let mut uses = Vec::new();
    for_each_expr(cx, body.value, |expr| {
        if expr.res_local_id() == Some(pat) {
            uses.push(expr);
        }
        ControlFlow::<(), Descend>::Continue(Descend::Yes)
    });
    if uses.is_empty() {
        return None;
    }
    uses.iter()
        .all(|&use_expr| {
            matches!(sink(cx, use_expr), Sink::Arg(call, index) if reaches_f32_param(cx, call, index))
        })
        .then_some(uses.len())
}

/// Flag `map_call` (`receiver.map(..)`): the suggestion replaces the
/// `.map(..)` segment — from the `.` after `receiver` to the call's closing
/// paren — with `fix`: `""` deletes it, `".clone()"` keeps the receiver's
/// value for its other uses. A comment inside the segment would be lost, so
/// it downgrades `app` to `MaybeIncorrect`.
fn report(
    cx: &LateContext<'_>,
    map_call: &Expr<'_>,
    receiver: &Expr<'_>,
    fix: &'static str,
    mut app: Applicability,
) {
    let segment = map_call.span.with_lo(receiver.span.hi());
    comment_guard(cx, segment, &mut app);
    let help = if fix.is_empty() {
        SUGGESTION
    } else {
        SUGGESTION_CLONE
    };
    span_lint_and_then(cx, NEEDLESS_SIGNAL_CAST, map_call.span, MESSAGE, |diag| {
        diag.span_suggestion(segment, help, fix, app);
    });
}

impl<'tcx> LateLintPass<'tcx> for NeedlessSignalCast {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::MethodCall(_, receiver, [func], _) = expr.kind else {
            return;
        };
        // The suggestion deletes from `receiver`'s end — it must be callsite
        // text, not expansion-internal.
        if receiver.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        if !resolves_to(cx, typeck, expr, &[SIGNAL_EXT_MAP]) {
            return;
        }
        let Some(&into_f32) = lookup_path_str(cx.tcx, PathNS::Type, INTO_F32).first() else {
            return;
        };
        let input = match body_expr(func).kind {
            ExprKind::Closure(closure) => {
                let body = cx.tcx.hir_body(closure.body);
                let [param] = body.params else { return };
                let PatKind::Binding(_, v, _, None) = param.pat.kind else {
                    return;
                };
                let cty = cx.tcx.typeck_body(closure.body);
                if !pure_f32_conversion(cx, cty, body_expr(body.value), v) {
                    return;
                }
                cty.pat_ty(param.pat)
            }
            ExprKind::Path(..) => match conversion_fn_input(cx, typeck, body_expr(func)) {
                Some(input) => input,
                None => return,
            },
            _ => return,
        };
        // The fix hands the unmapped signal to `impl IntoSignalF32` — only
        // valid when the signal's `Output` is itself `IntoF32`.
        if input.has_infer() || !implements_trait(cx, input, into_f32, &[]) {
            return;
        }
        match sink(cx, expr) {
            Sink::Arg(call, index) if reaches_f32_param(cx, call, index) => {
                // Direct when the map call itself is the argument — no
                // keeping adapter borrowed the receiver on the way.
                let direct = call_args(call)
                    .get(index)
                    .is_some_and(|arg| body_expr(arg).hir_id == expr.hir_id);
                let fix = if receiver_needs_clone(cx, expr, receiver, direct) {
                    ".clone()"
                } else {
                    ""
                };
                report(cx, expr, receiver, fix, Applicability::MachineApplicable);
            }
            Sink::Let(local) => {
                // An annotated `let` pins the mapped signal's type.
                if local.ty.is_some() {
                    return;
                }
                let Some(uses) = let_only_feeds_f32_params(cx, local) else {
                    return;
                };
                let app = if uses == 1 {
                    Applicability::MachineApplicable
                } else {
                    Applicability::MaybeIncorrect
                };
                let direct = local
                    .init
                    .is_some_and(|init| body_expr(init).hir_id == expr.hir_id);
                let fix = if receiver_needs_clone(cx, expr, receiver, direct) {
                    ".clone()"
                } else {
                    ""
                };
                report(cx, expr, receiver, fix, app);
            }
            _ => {}
        }
    }
}
