use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::ty::{ExprFnSig, expr_sig};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, FnDecl};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyKind};
use rustc_session::declare_lint_pass;

use crate::anyview::peel;
use crate::def_path::def_path_eq;
use crate::param_bounds::handler_args;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a handler — a closure or function passed to a
    /// `Handler`/`HandlerOnce` parameter (`.action(..)`, `ViewExt`'s `on_*`
    /// and `gesture` modifiers, `GestureObserver`/`EventHandler`/`DropTarget`)
    /// — that declares two or more `State<T>` parameters with the same `T`.
    ///
    /// ### Why is this bad?
    ///
    /// `State<T>` extracts from the environment by position: the n-th
    /// `State<T>` parameter binds to the n-th `.state(&..)` call carrying a
    /// `T`, not to a named value. Two parameters of one `T` silently pick up
    /// whichever `.state(..)` happens to sit at their position — the
    /// troubleshooting reference lists "a handler receives the wrong binding"
    /// as a silent bug.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// button("swap")
    ///     .action(|State(a): State<Binding<i32>>, State(b): State<Binding<i32>>| {
    ///         a.set(1);
    ///         b.set(2);
    ///     })
    ///     .state(&a)
    ///     .state(&b);
    /// ```
    ///
    /// Put both values in one `Clone` struct and inject it once instead:
    ///
    /// ```rust,ignore
    /// button("swap")
    ///     .action(|State(app): State<App>| app.a.set(1))
    ///     .state(&app);
    /// ```
    pub POSITIONAL_STATE_AMBIGUITY,
    suspicious,
    "two `State<T>` parameters of the same `T` bind to `.state(..)` calls by position"
}

declare_lint_pass!(PositionalStateAmbiguity => [POSITIONAL_STATE_AMBIGUITY]);

/// `waterui_core::foundation::extract::State` — the extractor whose parameters
/// bind positionally.
const STATE: &[&str] = &["waterui_core", "foundation", "extract", "State"];

/// `T` when `ty` is `State<T>`.
fn state_param<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> Option<Ty<'tcx>> {
    let TyKind::Adt(adt, args) = *ty.kind() else {
        return None;
    };
    def_path_eq(cx, adt.did(), STATE).then(|| args.type_at(0))
}

fn report<'tcx>(
    cx: &LateContext<'tcx>,
    arg: &'tcx Expr<'tcx>,
    decl: Option<&'tcx FnDecl<'tcx>>,
    name: Option<DefId>,
    first: usize,
    second: usize,
    t: Ty<'tcx>,
) {
    // A closure's diagnostic sits on its second `State<T>` parameter; a named
    // function's sits on the argument expression and names the function.
    let primary = match name {
        None => decl.map_or(arg.span, |decl| decl.inputs[second].span),
        Some(_) => arg.span,
    };
    span_lint_and_then(
        cx,
        POSITIONAL_STATE_AMBIGUITY,
        primary,
        format!("two `State<{t}>` parameters bind to `.state(..)` calls by position"),
        |diag| {
            let second_label =
                format!("binds to the second `.state(&..)` of type `{t}`, whichever that is");
            if let Some(decl) = decl {
                diag.span_label(
                    decl.inputs[first].span,
                    format!("the first `State<{t}>` parameter"),
                );
                diag.span_label(decl.inputs[second].span, second_label);
            } else {
                diag.span_label(primary, second_label);
            }
            if let Some(did) = name {
                diag.note(format!(
                    "`{}` declares these parameters",
                    cx.tcx.def_path_str(did)
                ));
            }
            diag.help(
                "put both values in one `Clone` struct and inject it once with \
                 `.state(&app)`; take `State(app): State<App>`",
            );
        },
    );
}

/// The handler passed at `arg` — its parameter types, the `FnDecl` its
/// parameter type spans come from when that declaration is in this crate, and
/// the `DefId` to name when it is a plain function.
fn handler_params<'tcx>(
    cx: &LateContext<'tcx>,
    arg: &'tcx Expr<'tcx>,
) -> Option<(&'tcx [Ty<'tcx>], Option<&'tcx FnDecl<'tcx>>, Option<DefId>)> {
    match expr_sig(cx, peel(arg))? {
        ExprFnSig::Closure(decl, sig) => {
            // A closure signature's single input is the tuple of its
            // parameter types.
            let TyKind::Tuple(params) = sig.skip_binder().inputs()[0].kind() else {
                return None;
            };
            Some((params, decl, None))
        }
        ExprFnSig::Sig(sig, did) => {
            let decl = did
                .and_then(DefId::as_local)
                .and_then(|local| cx.tcx.hir_node_by_def_id(local).fn_decl());
            Some((sig.skip_binder().inputs(), decl, did))
        }
        ExprFnSig::Trait(..) => None,
    }
}

fn check_handler<'tcx>(cx: &LateContext<'tcx>, arg: &'tcx Expr<'tcx>) {
    let Some((params, decl, name)) = handler_params(cx, arg) else {
        return;
    };
    // `(T, index of its first parameter)` per `State<T>` parameter seen so far.
    let mut seen: Vec<(Ty<'tcx>, usize)> = Vec::new();
    let mut reported: Vec<Ty<'tcx>> = Vec::new();
    for (i, ty) in params.iter().enumerate() {
        let Some(t) = state_param(cx, *ty) else {
            continue;
        };
        match seen.iter().find(|(seen_t, _)| *seen_t == t) {
            None => seen.push((t, i)),
            Some(&(_, first)) if !reported.contains(&t) => {
                report(cx, arg, decl, name, first, i, t);
                reported.push(t);
            }
            Some(_) => {}
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for PositionalStateAmbiguity {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        for arg in handler_args(cx, expr) {
            check_handler(cx, arg);
        }
    }
}
