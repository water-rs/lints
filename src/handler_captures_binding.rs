use clippy_utils::visitors::is_local_used;
use rustc_hir::{Block, Closure, Expr, ExprKind, PatKind, StmtKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;
use rustc_session::declare_lint_pass;
use rustc_span::{Ident, Span, Symbol};

use crate::binding::{BINDING, COMPUTED};
use crate::carriers::CLONE;
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;
use crate::param_bounds::{call_def_id, handler_args, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a closure passed to a `Handler`/`HandlerOnce` parameter — the
    /// `f` of `button(..).action(..)`/`action_async(..)`, `ViewExt`
    /// modifiers such as `.on_tap(..)`, `.gesture(..)`, `.on_appear(..)`,
    /// `.on_disappear(..)`, `.on_hover(..)`, and the
    /// `GestureObserver`/`EventHandler`/`DropTarget` constructors — whose
    /// captures include a `Binding<_>`, a `Computed<_>`, or a reactive
    /// `List<_>`. The clone-then-move block spelling
    /// `.action({ let b = b.clone(); move || … })` is the same capture.
    ///
    /// `ViewExt::on_change` is exempt by construction: its handler parameter
    /// is a plain `Fn(T)`, not a `Handler`, so a `Binding` captured there is
    /// an ordinary change callback, not an extractor handler.
    ///
    /// ### Why is this bad?
    ///
    /// A handler that closes over a reactive handle pins itself to that one
    /// handle and can only ever be an anonymous closure at the call site.
    /// Handlers take extractors: `.state(&count)` injects the handle into
    /// the view's environment and a `State(count): State<Binding<T>>`
    /// parameter reads it back, so the handler can be a named function and
    /// the data flow stays explicit.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let count: Binding<i32> = binding(0);
    /// button("+").action({
    ///     let count = count.clone();
    ///     move || count.set(1)
    /// });
    /// ```
    ///
    /// Delete the `let` line and the `move`, then inject the handle and take
    /// it as a `State` parameter:
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
    (COMPUTED, "Computed"),
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

/// The closure `arg` evaluates to, plus every peeled block wrapper
/// (outermost first) — the clone-then-move idiom's `let x = x.clone()`
/// lines live in those blocks' statements.
fn closure_and_wrapper<'tcx>(
    arg: &'tcx Expr<'tcx>,
) -> Option<(&'tcx Closure<'tcx>, Vec<&'tcx Block<'tcx>>)> {
    let mut expr = arg;
    let mut blocks = Vec::new();
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(inner, _) => {
                blocks.push(inner);
                inner.expr?
            }
            ExprKind::Closure(closure) => return Some((closure, blocks)),
            _ => return None,
        };
    }
}

/// The span of the `let <ident> = ….clone()` statement in the peeled wrapper
/// `blocks` — the clone-then-move idiom's per-handle line, searched
/// outermost-in — or `None` when no block binds `ident` through a
/// `Clone::clone` call, or when the clone binding is read anywhere else in
/// the wrapper (the `let` is then not only for the capture).
fn clone_let_span<'tcx>(
    cx: &LateContext<'tcx>,
    blocks: &[&'tcx Block<'tcx>],
    ident: Ident,
) -> Option<Span> {
    for (i, block) in blocks.iter().enumerate() {
        for (j, stmt) in block.stmts.iter().enumerate() {
            let StmtKind::Let(local) = stmt.kind else {
                continue;
            };
            let PatKind::Binding(_, hir_id, pat_ident, _) = local.pat.kind else {
                continue;
            };
            if pat_ident.span != ident.span {
                continue;
            }
            let did = call_def_id(cx.typeck_results(), local.init?)?;
            if !def_path_eq(cx, implemented_trait_item(cx.tcx, did), CLONE) {
                return None;
            }
            // Uses of the binding outside the tail closure are the rest of
            // the let's block plus every inner block's statements — the
            // blocks' tails are the peel chain itself, ending at `closure`.
            if is_local_used(cx, &block.stmts[j + 1..], hir_id)
                || blocks[i + 1..]
                    .iter()
                    .any(|inner| is_local_used(cx, inner.stmts, hir_id))
            {
                return None;
            }
            return Some(stmt.span);
        }
    }
    None
}

/// A reactive handle `closure` captures: `var_span`/`name` locate the
/// captured binding, `handle` is its `CAPTURED_HANDLES` name, and
/// `clone_let` is the `let` line that clones it for the capture when the
/// handler argument is spelled as the clone-then-move block.
struct Capture {
    var_span: Span,
    name: Symbol,
    handle: &'static str,
    clone_let: Option<Span>,
}

/// Reports `closure` once if it captures a reactive handle: the primary span
/// is the closure's parameter list, with one label per offending capture at
/// the captured variable's binding site (`var_ident`). In the clone-then-move
/// idiom the `let x = x.clone()` lines are labelled too, and the help names
/// deleting them together with the `move`.
fn check_handler_closure<'tcx>(
    cx: &LateContext<'tcx>,
    closure: &Closure<'tcx>,
    blocks: &[&'tcx Block<'tcx>],
) {
    let mut captures: Vec<Capture> = Vec::new();
    for captured in cx
        .typeck_results()
        .closure_min_captures_flattened(closure.def_id)
    {
        if let Some(handle) = handle_name(cx, captured.place.ty())
            && !captures.iter().any(|capture| {
                capture.var_span == captured.var_ident.span
                    && capture.name == captured.var_ident.name
            })
        {
            captures.push(Capture {
                var_span: captured.var_ident.span,
                name: captured.var_ident.name,
                handle,
                clone_let: clone_let_span(cx, blocks, captured.var_ident),
            });
        }
    }
    let Some(first) = captures.first() else {
        return;
    };
    let all_cloned = captures.iter().all(|capture| capture.clone_let.is_some());
    span_lint_and_then(
        cx,
        HANDLER_CAPTURES_BINDING,
        closure.fn_decl_span,
        "this handler captures a reactive handle instead of taking it as `State<T>`",
        |diag| {
            for capture in &captures {
                diag.span_label(
                    capture.var_span,
                    format!("captures `{}`, a `{}`", capture.name, capture.handle),
                );
                if let Some(span) = capture.clone_let {
                    diag.span_label(span, "cloned here only to be captured");
                }
            }
            let remedy = if all_cloned {
                let inject = captures
                    .iter()
                    .map(|capture| format!(".state(&{})", capture.name))
                    .collect::<String>();
                let params = captures
                    .iter()
                    .map(|capture| {
                        format!("`State({0}): State<{1}<T>>`", capture.name, capture.handle)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let (them, params_word) = if captures.len() == 1 {
                    ("it", "a handler parameter")
                } else {
                    ("them", "handler parameters")
                };
                format!(
                    "delete those `let` lines together with the `move`, then inject {them} with `{inject}` on the view and take {params} as {params_word}, so the handler can be a named function"
                )
            } else {
                format!(
                    "inject it with `.state(&{0})` on the view and take `State({0}): State<{1}<T>>` as a handler parameter, so the handler can be a named function",
                    first.name, first.handle
                )
            };
            diag.help(remedy);
        },
    );
}

impl<'tcx> LateLintPass<'tcx> for HandlerCapturesBinding {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        for arg in handler_args(cx, expr) {
            if let Some((closure, blocks)) = closure_and_wrapper(arg) {
                check_handler_closure(cx, closure, &blocks);
            }
        }
    }
}
