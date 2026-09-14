use clippy_utils::diagnostics::span_lint_and_then;
use rustc_hir::{Closure, Expr};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;
use rustc_session::declare_lint_pass;
use rustc_span::{Span, Symbol};

use crate::binding::BINDING;
use crate::def_path::def_path_eq;
use crate::param_bounds::handler_closures;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a closure passed to a `Handler`/`HandlerOnce` parameter — the
    /// `f` of `button(..).action(..)`/`action_async(..)`, `ViewExt`
    /// modifiers such as `.on_tap(..)`, `.gesture(..)`, `.on_appear(..)`,
    /// `.on_disappear(..)`, `.on_hover(..)`, and the
    /// `GestureObserver`/`EventHandler`/`DropTarget` constructors — whose
    /// captures include a `Binding<_>` or a reactive `List<_>`.
    ///
    /// `ViewExt::on_change` is exempt by construction: its handler parameter
    /// is a plain `Fn(T)`, not a `Handler`, so a `Binding` captured there is
    /// an ordinary change callback, not an extractor handler.
    ///
    /// ### Why is this bad?
    ///
    /// A handler that closes over a reactive handle pins itself to that one
    /// `Binding` and can only ever be an anonymous closure at the call site.
    /// Handlers take extractors: `.state(&count)` injects the handle into
    /// the view's environment and a `State(count): State<Binding<T>>`
    /// parameter reads it back, so the handler can be a named function and
    /// the data flow stays explicit.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let count: Binding<i32> = binding(0);
    /// button("+").action(move || count.set(1));
    /// ```
    ///
    /// Inject the handle and take it as a `State` parameter instead:
    ///
    /// ```rust,ignore
    /// button("+")
    ///     .action(|State(count): State<Binding<i32>>| count.set(1))
    ///     .state(&count);
    /// ```
    pub HANDLER_CAPTURES_BINDING,
    style,
    "this handler captures a reactive handle instead of taking it as `State<T>`"
}

declare_lint_pass!(HandlerCapturesBinding => [HANDLER_CAPTURES_BINDING]);

/// The reactive-handle types a handler must not capture, paired with the
/// name the diagnostic calls them by (`nami`'s `List` is `ReactiveList` in
/// the `waterui` facade).
const CAPTURED_HANDLES: &[(&[&str], &str)] = &[
    (BINDING, "Binding"),
    (&["nami", "data", "collection", "List"], "ReactiveList"),
];

/// The `CAPTURED_HANDLES` name of `ty` — references peeled — or `None` for
/// any other type.
fn handle_name(cx: &LateContext<'_>, ty: Ty<'_>) -> Option<&'static str> {
    let adt = ty.peel_refs().ty_adt_def()?;
    CAPTURED_HANDLES
        .iter()
        .find_map(|(path, name)| def_path_eq(cx, adt.did(), path).then_some(*name))
}

/// Reports `closure` once if it captures a reactive handle: the primary span
/// is the closure's parameter list, with one label per offending capture at
/// the captured variable's binding site (`var_ident`).
fn check_handler_closure(cx: &LateContext<'_>, closure: &Closure<'_>) {
    let mut captures: Vec<(Span, Symbol, &'static str)> = Vec::new();
    for captured in cx
        .typeck_results()
        .closure_min_captures_flattened(closure.def_id)
    {
        if let Some(handle) = handle_name(cx, captured.place.ty())
            && !captures.iter().any(|&(span, name, _)| {
                span == captured.var_ident.span && name == captured.var_ident.name
            })
        {
            captures.push((captured.var_ident.span, captured.var_ident.name, handle));
        }
    }
    let Some(&(_, first, _)) = captures.first() else {
        return;
    };
    span_lint_and_then(
        cx,
        HANDLER_CAPTURES_BINDING,
        closure.fn_decl_span,
        "this handler captures a reactive handle instead of taking it as `State<T>`",
        |diag| {
            for (span, name, handle) in &captures {
                diag.span_label(*span, format!("captures `{name}`, a `{handle}`"));
            }
            diag.help(format!(
                "inject it with `.state(&{first})` on the view and take `State({first}): State<Binding<T>>` as a handler parameter, so the handler can be a named function"
            ));
        },
    );
}

impl<'tcx> LateLintPass<'tcx> for HandlerCapturesBinding {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        for closure in handler_closures(cx, expr) {
            check_handler_closure(cx, closure);
        }
    }
}
