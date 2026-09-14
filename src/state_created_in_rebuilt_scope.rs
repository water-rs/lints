use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::visitors::{Descend, for_each_expr_without_closures};
use rustc_hir::{Closure, Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TypeckResults};
use rustc_session::declare_lint_pass;
use rustc_span::Span;
use std::ops::ControlFlow;

use crate::anyview::peel;
use crate::param_bounds::{
    VIEW_BUILDER, call_arg_bounds, call_args, call_def_id, implemented_trait_item,
};
use crate::watch::watch_call;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags reactive-state constructors — `binding(..)`, the typed
    /// `Binding::<ty>(..)` constructors (`i32`, `bool`, …),
    /// `Binding::container`/`custom`, `Container::new`,
    /// `reactive::collection::List::new`/`from(..)`, and
    /// `Default::default`/`From::from`/`.into(..)` producing a `Binding`,
    /// `Container`, or `List` — inside a closure that is rebuilt on every
    /// change: the `f` of `watch`/`Dynamic::watch`, or a closure passed to a
    /// `ViewBuilder` parameter such as `when`'s `then`, `.or(..)`, or
    /// `.otherwise(..)`.
    ///
    /// ### Why is this bad?
    ///
    /// The closure runs again each time the watched value or condition
    /// changes, so the state it creates is recreated — and reset — on every
    /// rebuild. The classic symptom is a text field whose typed input resets
    /// on every keystroke.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// watch(count.clone(), |v| {
    ///     let draft = binding(String::new());
    ///     vstack((text(draft), ..))
    /// })
    /// ```
    ///
    /// Own the state one level up and move it into the closure:
    ///
    /// ```rust,ignore
    /// let draft = binding(String::new());
    /// watch(count.clone(), move |v| vstack((text(draft.clone()), ..)))
    /// ```
    pub STATE_CREATED_IN_REBUILT_SCOPE,
    suspicious,
    "reactive state created inside a scope that is rebuilt on every change"
}

/// `STATE_CREATED_IN_ROW_BUILDER` shares this module with the lint above —
/// one pass recognizes both scopes — so its declaration lives in a submodule
/// to keep the two `LINT_INFO`s apart.
pub(crate) mod row_builder {
    declare_waterui_lint! {
        /// ### What it does
        ///
        /// Flags the same reactive-state constructors
        /// `state_created_in_rebuilt_scope` covers, inside the generator
        /// closure of a row builder: `ForEach::new`, `Lazy::for_each`,
        /// `List::for_each`, or `VStack`/`HStack`/`ZStack::for_each`.
        ///
        /// ### Why is this bad?
        ///
        /// The generator runs again whenever its row is rebuilt, so state
        /// created inside it is recreated and reset. Row-local state is
        /// sometimes exactly what is wanted — the row may genuinely own a
        /// scratch binding — which is why this lint is allow-by-default.
        ///
        /// ### Example
        ///
        /// ```rust,ignore
        /// Lazy::for_each(list, |item| {
        ///     let selected = binding(false);
        ///     text(item.id.to_string()).visible(selected)
        /// })
        /// ```
        ///
        /// If the state must survive a rebuild, keep it on the item and pass
        /// it in:
        ///
        /// ```rust,ignore
        /// Lazy::for_each(list, |item| {
        ///     text(item.id.to_string()).visible(item.selected.clone())
        /// })
        /// ```
        pub STATE_CREATED_IN_ROW_BUILDER,
        pedantic,
        "reactive state created inside a `for_each` row builder"
    }
}

declare_lint_pass!(StateCreatedInRebuiltScope => [
    STATE_CREATED_IN_REBUILT_SCOPE,
    row_builder::STATE_CREATED_IN_ROW_BUILDER,
]);

/// Free functions and inherent associated functions that construct fresh
/// reactive state: `binding(..)`, `Binding::container`/`custom`, the
/// `impl_binding!` typed constructors
/// (`nami-0.11.2/src/reactive_core/binding.rs`), `Container::new`, and
/// `List::new`/`from_vec` (`nami-0.11.2/src/data/collection.rs`).
const STATE_CTORS: &[&[&str]] = &[
    &["nami", "reactive_core", "binding", "binding"],
    &["nami", "reactive_core", "binding", "Binding", "container"],
    &["nami", "reactive_core", "binding", "Binding", "custom"],
    &["nami", "reactive_core", "binding", "Binding", "u32"],
    &["nami", "reactive_core", "binding", "Binding", "u64"],
    &["nami", "reactive_core", "binding", "Binding", "usize"],
    &["nami", "reactive_core", "binding", "Binding", "i32"],
    &["nami", "reactive_core", "binding", "Binding", "i64"],
    &["nami", "reactive_core", "binding", "Binding", "isize"],
    &["nami", "reactive_core", "binding", "Binding", "f32"],
    &["nami", "reactive_core", "binding", "Binding", "f64"],
    &["nami", "reactive_core", "binding", "Binding", "bool"],
    &["nami", "reactive_core", "binding", "Container", "new"],
    &["nami", "data", "collection", "List", "new"],
    &["nami", "data", "collection", "List", "from_vec"],
];

/// Trait constructors that produce fresh reactive state when the result type
/// is a state type: `Binding::default()`, `ReactiveList::from(vec)`, or
/// `value.into()` into one. The impl's method normalizes onto the trait item
/// through `implemented_trait_item`.
const CONVERSION_CTORS: &[&[&str]] = &[
    &["core", "default", "Default", "default"],
    &["core", "convert", "From", "from"],
    &["core", "convert", "Into", "into"],
];

/// The reactive-state types a `CONVERSION_CTORS` call must produce to count.
const STATE_TYPES: &[&[&str]] = &[
    &["nami", "reactive_core", "binding", "Binding"],
    &["nami", "reactive_core", "binding", "Container"],
    &["nami", "data", "collection", "List"],
];

/// Calls whose generator closure — always the second argument — is a row
/// builder. The stack `for_each` functions are emitted by the
/// `impl_stack_for_each!` macro inside each per-stack module
/// (`waterui-layout-0.3.2/src/stack.rs`), so their def paths carry the
/// `vstack`/`hstack`/`zstack` module segment.
const ROW_BUILDER_CALLS: &[&[&str]] = &[
    &["waterui_core", "ui", "views", "ForEach", "new"],
    &["waterui_internal", "component", "lazy", "Lazy", "for_each"],
    &["waterui_internal", "component", "list", "List", "for_each"],
    &["waterui_layout", "stack", "vstack", "VStack", "for_each"],
    &["waterui_layout", "stack", "hstack", "HStack", "for_each"],
    &["waterui_layout", "stack", "zstack", "ZStack", "for_each"],
];

const REBUILT_MSG: &str = "reactive state created inside a scope that is rebuilt on every change";
const ROW_MSG: &str = "reactive state created inside a row builder";
const CTOR_LABEL: &str =
    "this `Binding`/`List` is recreated — and reset — each time the closure runs";
const REBUILT_SCOPE_LABEL: &str = "this closure runs again whenever the watched value changes";
const ROW_SCOPE_LABEL: &str = "this closure runs again each time the row is rebuilt";
const REBUILT_HELP: &str = "own the state one level up — in the enclosing body or the component — and move it into the closure";
const ROW_HELP: &str = "row-local state is recreated when the row is rebuilt; if it must survive, keep it on the item and pass it in";

/// Whether `expr` constructs fresh reactive state — see `STATE_CTORS` and
/// `CONVERSION_CTORS`. `typeck` must be the `TypeckResults` of the body
/// containing `expr`.
fn is_state_ctor<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    expr: &Expr<'tcx>,
) -> bool {
    if !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
        return false;
    }
    let Some(did) = call_def_id(typeck, expr).map(|did| implemented_trait_item(cx.tcx, did)) else {
        return false;
    };
    if STATE_CTORS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, did, path))
    {
        return true;
    }
    CONVERSION_CTORS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, did, path))
        && is_state_ty(cx, typeck.expr_ty(expr))
}

/// Whether `ty`, references peeled, is `Binding<_>`, `Container<_>`, or
/// `List<_>` — the types a conversion constructor must produce.
fn is_state_ty(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    ty.peel_refs().ty_adt_def().is_some_and(|adt| {
        STATE_TYPES
            .iter()
            .any(|path| crate::def_path::def_path_eq(cx, adt.did(), path))
    })
}

/// The span the scope label hangs on: the callee path for a plain call
/// (`watch`, `when`, `Lazy::for_each`), the method name for a method call
/// (`.or(..)`, `.otherwise(..)`).
fn callee_span(call: &Expr<'_>) -> Span {
    match call.kind {
        ExprKind::Call(func, _) => func.span,
        ExprKind::MethodCall(segment, ..) => segment.ident.span,
        _ => call.span,
    }
}

/// Which lint a scope reports under.
#[derive(Clone, Copy)]
enum Scope {
    /// The `f` of `watch`/`Dynamic::watch`, or a closure passed to a
    /// `ViewBuilder`-bounded parameter (`when`, `.or`, `.otherwise`) — the
    /// callee invokes it again on every change.
    Rebuilt,
    /// The generator of a `for_each`/`ForEach::new` call — invoked once per
    /// row per rebuild.
    RowBuilder,
}

/// Reports every state constructor in `closure`'s body. Nested closures are
/// not entered: a `binding(..)` inside a nested `.action(..)` handler runs at
/// event time, not at rebuild, and a nested `watch`/`when` closure is its own
/// scope — checked when the pass reaches its call.
fn check_scope<'tcx>(cx: &LateContext<'tcx>, closure: &Closure<'tcx>, scope: Scope, callee: Span) {
    let typeck = cx.tcx.typeck_body(closure.body);
    let body = cx.tcx.hir_body(closure.body);
    for_each_expr_without_closures(body.value, |expr| {
        if expr.span.from_expansion() || !is_state_ctor(cx, typeck, expr) {
            return ControlFlow::<(), Descend>::Continue(Descend::Yes);
        }
        let (lint, msg, scope_label, help) = match scope {
            Scope::Rebuilt => (
                &STATE_CREATED_IN_REBUILT_SCOPE,
                REBUILT_MSG,
                REBUILT_SCOPE_LABEL,
                REBUILT_HELP,
            ),
            Scope::RowBuilder => (
                &row_builder::STATE_CREATED_IN_ROW_BUILDER,
                ROW_MSG,
                ROW_SCOPE_LABEL,
                ROW_HELP,
            ),
        };
        span_lint_and_then(cx, lint, expr.span, msg, |diag| {
            diag.span_label(expr.span, CTOR_LABEL);
            diag.span_label(callee, scope_label);
            diag.help(help);
        });
        ControlFlow::<(), Descend>::Continue(Descend::Yes)
    });
}

impl<'tcx> LateLintPass<'tcx> for StateCreatedInRebuiltScope {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        let mut scopes: Vec<(&Closure<'tcx>, Scope)> = Vec::new();
        if let Some(call) = watch_call(cx, expr)
            && let Some(closure) = call.closure()
        {
            scopes.push((closure, Scope::Rebuilt));
        }
        if let Some((_, bounds)) = call_arg_bounds(cx, expr, &[&[VIEW_BUILDER]]) {
            for (arg, targets) in call_args(expr).into_iter().zip(bounds) {
                if !targets.is_empty()
                    && let ExprKind::Closure(closure) = peel(arg).kind
                {
                    scopes.push((closure, Scope::Rebuilt));
                }
            }
        }
        if call_def_id(cx.typeck_results(), expr)
            .map(|did| implemented_trait_item(cx.tcx, did))
            .is_some_and(|did| {
                ROW_BUILDER_CALLS
                    .iter()
                    .any(|path| crate::def_path::def_path_eq(cx, did, path))
            })
            && let Some(generator) = call_args(expr).get(1)
            && let ExprKind::Closure(closure) = peel(generator).kind
        {
            scopes.push((closure, Scope::RowBuilder));
        }
        for (closure, scope) in scopes {
            check_scope(cx, closure, scope, callee_span(expr));
        }
    }
}
