//! `needless_computed` — `.computed()`, `.into_computed()`, and
//! `Computed::new(..)` erasures whose result only ever reaches a parameter
//! that already accepts any signal.

use clippy_utils::diagnostics::{span_lint, span_lint_and_then};
use clippy_utils::res::MaybeResPath;
use clippy_utils::sugg::Sugg;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_errors::Applicability;
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind, HirId, Mutability, Node, Pat, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{
    self, AssocContainer, Ty, TyCtxt, TyKind, TypeSuperVisitable, TypeVisitable, TypeVisitableExt,
    TypeVisitor, TypeckResults,
};
use rustc_session::declare_lint_pass;
use std::ops::ControlFlow;

use crate::applicability::comment_guard;
use crate::binding::COMPUTED;
use crate::carriers::CARRIER_CALLS;
use crate::color::SIZED;
use crate::computed::{COMPUTED_NEW, INTO_COMPUTED, erases_signal};
use crate::def_path::def_path_eq;
use crate::param_bounds::{
    call_arg_all_bounds_in, call_arg_bounds_in, call_args, call_def_id, implemented_trait_item,
};
use crate::receiver::receiver_needs_clone;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `signal.computed()`, `signal.into_computed()`, and
    /// `Computed::new(signal)` — the spellings that erase a signal into
    /// `Computed<T>` — when the result only ever reaches a parameter that
    /// already accepts any signal (`impl IntoComputed<T>`,
    /// `impl IntoSignal<T>`, `impl Signal<Output = T>`,
    /// `impl IntoSignalF32`): as the direct argument, or through a `let`
    /// binding — including an `Option<Computed<T>>` produced by an
    /// `Option::map` closure or `Some(..)` and consumed through
    /// `Some(x)`/`if let`/`match` patterns — whose every use is such an
    /// argument or a `&self` `Signal`/`SignalExt` method whose return type
    /// does not embed `Self` (`get`, `identity`; adapters like `map` keep
    /// the erasure load-bearing).
    ///
    /// ### Why is this bad?
    ///
    /// `Computed<T>` exists for positions that must name a concrete type —
    /// a struct field, a return type shared by branches, an `AnyView`-like
    /// boundary. At a signal-accepting parameter it adds a boxing layer and
    /// a type-erased identity for nothing: the un-erased signal serves the
    /// same consumers.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let unavailable = channel.map(|channel| {
    ///     available.map(move |a| !a.contains(channel)).computed()
    /// });
    /// row.opacity(unavailable.select(0.45, 1.0)).disabled(unavailable)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let unavailable = channel.map(|channel| {
    ///     available.map(move |a| !a.contains(channel))
    /// });
    /// ```
    pub NEEDLESS_COMPUTED,
    style,
    "a `computed()` erasure feeding consumers that already accept any signal"
}

declare_lint_pass!(NeedlessComputed => [NEEDLESS_COMPUTED]);

const MESSAGE: &str = "this `computed()` erases a signal that every consumer already accepts";

/// `SignalExt::computed` — the method spelling of the erasure, and the only
/// one that borrows its receiver (`&self`); the fix may need `.clone()`.
const SIGNAL_EXT_COMPUTED: &[&str] = &["nami", "reactive_core", "ext", "SignalExt", "computed"];

/// `nami_core::Signal::Output` — the associated type `erases_signal` pins
/// to `T`; the only `Self`-projection a safe receiver method may return.
const SIGNAL_OUTPUT: &[&str] = &["nami_core", "Signal", "Output"];

/// `x.<method>()` erasures.
const METHOD_ERASURES: &[&[&str]] = &[SIGNAL_EXT_COMPUTED, INTO_COMPUTED];

/// `<f>(e)` erasures — `Computed::new(e)` and the UFCS spellings
/// `SignalExt::computed(&e)`/`IntoComputed::into_computed(e)`.
const CALL_ERASURES: &[&[&str]] = &[COMPUTED_NEW, SIGNAL_EXT_COMPUTED, INTO_COMPUTED];

/// `Option::map` — `opt.map(|_| <computed>)` wraps the erasure's result in
/// `Option`; the produced `Option<Computed<T>>` is what flows onward.
const OPTION_MAP: &[&str] = &["core", "option", "Option", "map"];

/// `Option::Some` — `Some(<computed>)` likewise.
const OPTION_SOME: &[&str] = &["core", "option", "Option", "Some"];

/// Parameter bounds that already accept any signal. `IntoText`/`IntoLabel`
/// are absent: they are implemented for `Computed`/`Binding` and the text
/// leaf types, not for arbitrary signals, so the erasure is load-bearing in
/// those positions.
const ANY_SIGNAL: &[&[&str]] = &[
    &["nami", "reactive_core", "signal", "IntoComputed"],
    &["nami", "reactive_core", "signal", "IntoSignal"],
    &["nami_core", "Signal"],
    &["waterui_core", "state", "computed_f32", "IntoSignalF32"],
];
const ANY_SIGNAL_BOUNDS: &[&[&[&str]]] = &[ANY_SIGNAL];

/// The signal traits — a `Computed` local used only through `Signal`/
/// `SignalExt` `&self` methods whose return types do not embed `Self`
/// ([`return_keeps_type`]) does not need the erasure.
const SIGNAL_TRAITS: &[&[&str]] = &[
    &["nami_core", "Signal"],
    &["nami", "reactive_core", "ext", "SignalExt"],
];

/// A `Computed`-bound local may reach inherent `&self` methods of the
/// erased type itself — `Computed` has none today, but the entry keeps the
/// check honest if it grows one.
const INHERENT_SIGNAL_HOMES: &[&[&str]] = &[COMPUTED];

/// Where `expr`'s value flows: an argument position of a call, the locals a
/// pattern binds it to, or neither.
enum Flow<'hir> {
    /// `expr` (through value-preserving wrappers) sits at argument `index`
    /// of `call` — index 0 is the method receiver.
    Arg(&'hir Expr<'hir>, usize),
    /// `expr` (through wrappers) is bound by the given `PatKind::Binding`
    /// ids — a `let`/`let-else` initializer, or the patterns of an
    /// `if let`/`while let`/`match`/`?` scrutinee.
    Bound(Vec<HirId>),
    /// Consumed any other way — a field, a branch, a return, the callee.
    End,
}

/// The `PatKind::Binding` ids anywhere in `pat`.
fn pat_bindings(pat: &Pat<'_>) -> Vec<HirId> {
    let mut bindings = Vec::new();
    pat.each_binding(|_, id, _, _| bindings.push(id));
    bindings
}

/// The `TypeckResults` of the body containing `expr` — `expr` may sit inside
/// a nested closure relative to the pass's current body.
fn typeck_of<'tcx>(cx: &LateContext<'tcx>, expr: &Expr<'tcx>) -> &'tcx TypeckResults<'tcx> {
    cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(expr.hir_id))
}

/// `did`'s first parameter is `&self` — not `self`, `&mut self`, or absent.
fn takes_ref_self(cx: &LateContext<'_>, did: DefId) -> bool {
    cx.tcx
        .fn_sig(did)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder()
        .inputs()
        .first()
        .is_some_and(|input| matches!(input.kind(), TyKind::Ref(_, _, Mutability::Not)))
}

/// `call` is a `&self` `Signal`/`SignalExt` method call or an inherent
/// `&self` method of `Computed` — a use every signal supports, so the
/// receiver's `Computed` erasure is not load-bearing — provided its return
/// type does not change with `Self` ([`return_keeps_type`]).
fn ref_self_signal_method<'tcx>(cx: &LateContext<'tcx>, call: &Expr<'tcx>) -> bool {
    if !matches!(call.kind, ExprKind::MethodCall(..)) {
        return false;
    }
    let Some(did) = call_def_id(typeck_of(cx, call), call) else {
        return false;
    };
    if !takes_ref_self(cx, did) {
        return false;
    }
    let item = implemented_trait_item(cx.tcx, did);
    let Some(assoc) = cx.tcx.opt_associated_item(item) else {
        return false;
    };
    match assoc.container {
        AssocContainer::Trait => SIGNAL_TRAITS
            .iter()
            .any(|path| def_path_eq(cx, cx.tcx.parent(item), path)),
        AssocContainer::InherentImpl => {
            let self_ty = cx
                .tcx
                .type_of(cx.tcx.parent(item))
                .instantiate_identity()
                .skip_norm_wip();
            match self_ty.kind() {
                TyKind::Adt(adt, _) => INHERENT_SIGNAL_HOMES
                    .iter()
                    .any(|path| def_path_eq(cx, adt.did(), path)),
                _ => false,
            }
        }
        AssocContainer::TraitImpl(_) => false,
    }
}

/// Follow `expr`'s value through value-preserving wrappers — drop-temps,
/// `&`/`*`, block tails, `clone`/`to_owned`/`to_string` carriers,
/// `Some(..)`, and the `Option::map` closure it is the tail of — to where it
/// is consumed: an argument position, a set of bound locals, or an end the
/// lint does not understand.
fn flow<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Flow<'tcx> {
    let mut current = expr;
    loop {
        match cx.tcx.parent_hir_node(current.hir_id) {
            Node::Expr(parent) => match parent.kind {
                ExprKind::DropTemps(inner)
                | ExprKind::AddrOf(.., inner)
                | ExprKind::Unary(UnOp::Deref, inner)
                    if inner.hir_id == current.hir_id =>
                {
                    current = parent;
                }
                ExprKind::Closure(closure)
                    if cx.tcx.hir_body(closure.body).value.hir_id == current.hir_id =>
                {
                    current = parent;
                }
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    let args = call_args(parent);
                    let Some(index) = args.iter().position(|arg| arg.hir_id == current.hir_id)
                    else {
                        // `current` is the callee, not an operand.
                        return Flow::End;
                    };
                    let item = call_def_id(typeck_of(cx, parent), parent)
                        .map(|did| implemented_trait_item(cx.tcx, did));
                    let carried = item.is_some_and(|item| {
                        // `x.clone()`/`.to_owned()`/`.to_string()` rewrap the
                        // value; `Some(x)` wraps it in `Option`;
                        // `opt.map(closure)` wraps the closure's return.
                        (index == 0 && CARRIER_CALLS.iter().any(|path| def_path_eq(cx, item, path)))
                            || def_path_eq(cx, item, OPTION_SOME)
                            || (index == 1
                                && matches!(current.kind, ExprKind::Closure(..))
                                && def_path_eq(cx, item, OPTION_MAP))
                    });
                    if carried {
                        current = parent;
                        continue;
                    }
                    return Flow::Arg(parent, index);
                }
                ExprKind::Match(scrutinee, arms, _) if scrutinee.hir_id == current.hir_id => {
                    return Flow::Bound(
                        arms.iter().flat_map(|arm| pat_bindings(arm.pat)).collect(),
                    );
                }
                ExprKind::Let(let_expr) if let_expr.init.hir_id == current.hir_id => {
                    return Flow::Bound(pat_bindings(let_expr.pat));
                }
                _ => return Flow::End,
            },
            Node::Block(block) if block.expr.is_some_and(|tail| tail.hir_id == current.hir_id) => {
                // A block tail's value is the block expression's — the
                // `Expr` carrying `ExprKind::Block` is the block's parent.
                match cx.tcx.parent_hir_node(block.hir_id) {
                    Node::Expr(parent) if matches!(parent.kind, ExprKind::Block(..)) => {
                        current = parent;
                    }
                    _ => return Flow::End,
                }
            }
            Node::LetStmt(local)
                if local.init.is_some_and(|init| init.hir_id == current.hir_id) =>
            {
                // An annotation pins the `Computed` type the rewrite drops.
                if local.ty.is_some() {
                    return Flow::End;
                }
                return Flow::Bound(pat_bindings(local.pat));
            }
            _ => return Flow::End,
        }
    }
}

/// `call`'s parameter at `index` accepts any signal: it carries an
/// `ANY_SIGNAL` bound — and every positive bound on it is in `ANY_SIGNAL`
/// or `Sized`, since an unchecked extra bound (`impl IntoComputed<T> +
/// Into<Computed<T>>`) could reject the un-erased signal.
fn reaches_signal_param<'tcx>(
    cx: &LateContext<'tcx>,
    call: &'tcx Expr<'tcx>,
    index: usize,
) -> bool {
    let typeck = typeck_of(cx, call);
    let Some((_, per_arg)) = call_arg_bounds_in(cx, typeck, call, ANY_SIGNAL_BOUNDS) else {
        return false;
    };
    let Some((_, per_arg_all)) = call_arg_all_bounds_in(cx, typeck, call) else {
        return false;
    };
    let Some(targets) = per_arg.get(index) else {
        return false;
    };
    let Some(all) = per_arg_all.get(index) else {
        return false;
    };
    !targets.is_empty()
        && all.iter().all(|target| {
            ANY_SIGNAL
                .iter()
                .any(|path| def_path_eq(cx, target.trait_did, path))
                || def_path_eq(cx, target.trait_did, SIZED)
        })
}

/// `t` mentions the erased receiver type `self_ty` — directly or through a
/// projection on it other than `Signal::Output`, the one associated type
/// `erases_signal` pins equal across the rewrite.
struct ReceiverMention<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    self_ty: Ty<'tcx>,
}

impl<'tcx> TypeVisitor<TyCtxt<'tcx>> for ReceiverMention<'_, 'tcx> {
    type Result = ControlFlow<()>;

    fn visit_ty(&mut self, t: Ty<'tcx>) -> Self::Result {
        if t == self.self_ty {
            return ControlFlow::Break(());
        }
        if let TyKind::Alias(alias) = t.kind()
            && alias.self_ty() == self.self_ty
        {
            return if matches!(alias.kind, ty::AliasTyKind::Projection { .. })
                && def_path_eq(self.cx, alias.kind.def_id(), SIGNAL_OUTPUT)
            {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            };
        }
        t.super_visit_with(self)
    }
}

/// `call`'s return type is the same whether its receiver is the erased
/// `Computed<T>` or the un-erased signal — true for `get` (`Self::Output`,
/// pinned to `T`) and `identity` (no `Self`); false for `watch`
/// (`Self::Guard`) and every `SignalExt` adapter (`Map<Self, ..>`,
/// `WithMetadata<Self, ..>`, …), whose result embeds `Self` and would
/// change type under the fix.
fn return_keeps_type<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    call: &'tcx Expr<'tcx>,
) -> bool {
    let Some(did) = call_def_id(typeck, call) else {
        return false;
    };
    let Some(&receiver) = call_args(call).first() else {
        return false;
    };
    let self_ty = typeck.expr_ty(receiver);
    let output = cx
        .tcx
        .fn_sig(did)
        .instantiate(cx.tcx, typeck.node_args(call.hir_id))
        .skip_binder()
        .output();
    if self_ty.has_infer() || output.has_infer() {
        return false;
    }
    output
        .visit_with(&mut ReceiverMention { cx, self_ty })
        .is_continue()
}

/// A use of the erased value at `call`'s parameter `index` keeps compiling
/// after the rewrite: a signal-accepting parameter — or, for a method
/// receiver, a `&self` signal method whose return type does not change with
/// `Self`.
fn arg_use_ok<'tcx>(cx: &LateContext<'tcx>, call: &'tcx Expr<'tcx>, index: usize) -> bool {
    if index == 0 && matches!(call.kind, ExprKind::MethodCall(..)) {
        ref_self_signal_method(cx, call) && return_keeps_type(cx, typeck_of(cx, call), call)
    } else {
        reaches_signal_param(cx, call, index)
    }
}

/// Every use of the `binding` local is a signal-accepting position — the
/// only shape under which dropping the erasure keeps every consumer
/// compiling. Uses inside nested closures count too. `false` when the local
/// is never used: a dead erasure is not this lint's case.
fn binding_uses_ok<'tcx>(cx: &LateContext<'tcx>, binding: HirId) -> bool {
    let owner = cx.tcx.hir_enclosing_body_owner(binding);
    let body = cx.tcx.hir_body_owned_by(owner);
    let mut uses = Vec::new();
    for_each_expr(cx, body.value, |expr| {
        if expr.res_local_id() == Some(binding) {
            uses.push(expr);
        }
        ControlFlow::<(), Descend>::Continue(Descend::Yes)
    });
    if uses.is_empty() {
        return false;
    }
    uses.iter().all(|&use_expr| match flow(cx, use_expr) {
        Flow::Arg(call, index) => arg_use_ok(cx, call, index),
        Flow::Bound(bindings) => {
            !bindings.is_empty() && bindings.iter().all(|&b| binding_uses_ok(cx, b))
        }
        Flow::End => false,
    })
}

/// The suggestion's applicability for the erasure `expr`, and whether the
/// result lands somewhere it moves by value — `false` for a `&self` method
/// receiver, which keeps the signal borrowed. `MachineApplicable` when the
/// result is a direct signal-accepting argument, `MaybeIncorrect` when it
/// reaches one through a local (whose inferred type changes), `None` when
/// the result escapes some other way.
fn verdict<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Option<(Applicability, bool)> {
    match flow(cx, expr) {
        Flow::Arg(call, index) if arg_use_ok(cx, call, index) => Some((
            Applicability::MachineApplicable,
            !(index == 0 && matches!(call.kind, ExprKind::MethodCall(..))),
        )),
        Flow::Bound(bindings) => (!bindings.is_empty()
            && bindings.iter().all(|&b| binding_uses_ok(cx, b)))
        .then_some((Applicability::MaybeIncorrect, true)),
        _ => None,
    }
}

/// The signal a call-form erasure wraps: `e` for `Computed::new(e)` and
/// `IntoComputed::into_computed(e)`; the referent for `&e`/`&mut e` in UFCS
/// `SignalExt::computed(&e)`.
fn unwrap_receiver_refs<'hir>(mut expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    while let ExprKind::AddrOf(.., inner) = expr.kind {
        expr = inner;
    }
    expr
}

impl<'tcx> LateLintPass<'tcx> for NeedlessComputed {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        match expr.kind {
            ExprKind::MethodCall(segment, receiver, [], _) => {
                // The suggestion deletes from `receiver`'s end — it must be
                // callsite text, not expansion-internal.
                if receiver.span.from_expansion() {
                    return;
                }
                let Some(did) = call_def_id(typeck, expr) else {
                    return;
                };
                let item = implemented_trait_item(cx.tcx, did);
                if !METHOD_ERASURES
                    .iter()
                    .any(|path| def_path_eq(cx, item, path))
                {
                    return;
                }
                if !erases_signal(cx, typeck, expr, receiver) {
                    return;
                }
                let Some((mut app, moves)) = verdict(cx, expr) else {
                    return;
                };
                // `computed(&self)` only borrows the receiver — deleting
                // the call hands it to the consumer by value, so a place
                // that must not move is passed `.clone()` instead.
                // `into_computed` takes `self` and already moves.
                let fix = if def_path_eq(cx, item, SIGNAL_EXT_COMPUTED)
                    && receiver_needs_clone(cx, expr, receiver, moves)
                {
                    ".clone()"
                } else {
                    ""
                };
                let segment_span = expr.span.with_lo(receiver.span.hi());
                comment_guard(cx, segment_span, &mut app);
                span_lint_and_then(cx, NEEDLESS_COMPUTED, expr.span, MESSAGE, |diag| {
                    diag.span_suggestion(
                        segment_span,
                        if fix.is_empty() {
                            format!("drop the `.{}()`", segment.ident.name)
                        } else {
                            format!("replace the `.{}()` with `.clone()`", segment.ident.name)
                        },
                        fix,
                        app,
                    );
                });
            }
            ExprKind::Call(_, [arg]) => {
                let Some(did) = call_def_id(typeck, expr) else {
                    return;
                };
                let item = implemented_trait_item(cx.tcx, did);
                if !CALL_ERASURES.iter().any(|path| def_path_eq(cx, item, path)) {
                    return;
                }
                let signal = unwrap_receiver_refs(arg);
                if signal.span.from_expansion() {
                    return;
                }
                if !erases_signal(cx, typeck, expr, signal) {
                    return;
                }
                let Some((mut app, moves)) = verdict(cx, expr) else {
                    return;
                };
                let Some(sugg) = Sugg::hir_opt(cx, signal) else {
                    span_lint(cx, NEEDLESS_COMPUTED, expr.span, MESSAGE);
                    return;
                };
                // UFCS `SignalExt::computed(&e)` borrowed `e`; the rewrite
                // hands it over by value, so a place that must not move is
                // passed `.clone()`. `Computed::new`/`into_computed` take
                // their argument by value already.
                let fix = if def_path_eq(cx, item, SIGNAL_EXT_COMPUTED)
                    && receiver_needs_clone(cx, expr, signal, moves)
                {
                    format!("{}.clone()", sugg.maybe_paren())
                } else {
                    sugg.maybe_paren().to_string()
                };
                comment_guard(cx, expr.span, &mut app);
                span_lint_and_then(cx, NEEDLESS_COMPUTED, expr.span, MESSAGE, |diag| {
                    diag.span_suggestion(expr.span, "use the signal directly", fix, app);
                });
            }
            _ => {}
        }
    }
}
