use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::higher::ForLoop;
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::source::snippet_with_applicability;
use rustc_errors::Applicability;
use rustc_hir::def::Namespace;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyKind, TypeckResults};
use rustc_session::declare_lint_pass;
use rustc_span::{Symbol, sym};

use crate::def_path::def_path_eq;
use crate::imports::{Bare, bare_status};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `list.snapshot()` (nami `reactive::collection::List`) whose
    /// result feeds exactly one operation `List` exposes itself:
    /// `.len()`, `.is_empty()`, `.get(i).cloned()`, `.first().cloned()`,
    /// `.into_iter()`, or a `for` loop head.
    ///
    /// ### Why is this bad?
    ///
    /// For the reads, `snapshot()` clones the whole vector to answer what
    /// `List` answers directly — a length, one element — and the `List`
    /// method is the spelling. For `into_iter` and a `for` head, `List`
    /// iterates directly (`List::iter` returns the same owned
    /// `vec::IntoIter<T>`), so writing out the snapshot is noise. A
    /// `let`-bound, mutated, returned, or otherwise retained snapshot is
    /// the reader holding a stable copy on purpose and stays silent.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let n = items.snapshot().len();
    /// for item in items.snapshot() { .. }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let n = items.len();
    /// for item in items.iter() { .. }
    /// ```
    ///
    /// ### Known problems
    ///
    /// `list.len()`/`list.is_empty()`/`list.get(i)` need `nami`'s
    /// `Collection` trait in scope, and the `waterui` prelude does not
    /// re-export it. The `use` is never auto-inserted: importing
    /// `waterui::reactive::collection::Collection` into a module
    /// re-resolves every `vec.get(i)` there to `Collection::get` on `Vec`
    /// (`Option<T>` where `[T]::get` returned `Option<&T>`) — a
    /// module-wide side effect on unrelated code, including the
    /// `snapshot().get(i).cloned()` sites this lint rewrites. When
    /// `Collection` already resolves to the trait at the site the
    /// suggestion is `MachineApplicable`; otherwise the diagnostic is a
    /// `help` naming the rewrite and the import.
    ///
    /// `snapshot().iter()` is not flagged: `[T]::iter` yields `&T` while
    /// `List::iter` yields owned `T`, so no rewrite is type-preserving.
    pub NEEDLESS_LIST_SNAPSHOT,
    style,
    "a `snapshot()` consumed by an operation `List` exposes directly"
}

declare_lint_pass!(NeedlessListSnapshot => [NEEDLESS_LIST_SNAPSHOT]);

/// `nami::data::collection::List::snapshot` — the inherent `Vec<T>` clone.
const LIST_SNAPSHOT: &[&str] = &["nami", "data", "collection", "List", "snapshot"];

/// `nami_core::collection::Collection` — the trait `len`/`is_empty`/`get`
/// come from. The `waterui` prelude does not re-export it.
const COLLECTION: &str = "nami_core::collection::Collection";
const COLLECTION_USE: &str = "waterui::reactive::collection::Collection";

const COPY_MESSAGE: &str = "this `snapshot` copies the list to read what `List` exposes directly";
const ITER_MESSAGE: &str = "`List` iterates directly; `snapshot()` before the loop is spelled out";
const SUGGESTION_LABEL: &str = "use the `List` method";

/// `expr` is `x.snapshot()` resolving to `List::snapshot` — returns `x`.
fn snapshot_receiver<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    let ExprKind::MethodCall(segment, receiver, [], _) = expr.kind else {
        return None;
    };
    if expr.span.from_expansion() || segment.ident.name.as_str() != "snapshot" {
        return None;
    }
    let did = typeck.type_dependent_def_id(expr.hir_id)?;
    def_path_eq(cx, did, LIST_SNAPSHOT).then_some(receiver)
}

/// `ty` is `Option<&T>` over an element — the shape `Vec::get`/`Vec::first`
/// hand `.cloned()`. A ranged `get` yields `Option<&[T]>`, which
/// `Collection::get` cannot spell.
fn is_option_ref(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    let TyKind::Adt(def, args) = ty.kind() else {
        return false;
    };
    cx.tcx.is_diagnostic_item(sym::Option, def.did())
        && matches!(
            args[0].expect_ty().kind(),
            TyKind::Ref(_, inner, _) if !inner.is_slice()
        )
}

/// `expr` is `x.snapshot().get(i)`/`x.snapshot().first()` returning
/// `Option<&T>` — returns `(x, index)` where `index` is `Some(arg)` for
/// `get` and `None` for `first`.
fn snapshot_option_ref<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<(&'hir Expr<'hir>, Option<&'hir Expr<'hir>>)> {
    let ExprKind::MethodCall(segment, snapshot, args, _) = expr.kind else {
        return None;
    };
    let index = match segment.ident.name.as_str() {
        "first" if args.is_empty() => None,
        "get" => match args {
            [arg] => Some(arg),
            _ => return None,
        },
        _ => return None,
    };
    let list = snapshot_receiver(cx, typeck, snapshot)?;
    is_option_ref(cx, typeck.expr_ty(expr)).then_some((list, index))
}

/// `Collection` resolves to the `nami_core` trait at `expr` — the only
/// case where `list.len()`/`list.is_empty()`/`list.get(i)` spell the same
/// methods.
fn collection_in_scope(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    lookup_path_str(cx.tcx, PathNS::Type, COLLECTION)
        .first()
        .is_some_and(|&collection| {
            matches!(
                bare_status(
                    cx,
                    expr.hir_id,
                    expr.span.lo(),
                    Symbol::intern("Collection"),
                    Namespace::TypeNS,
                    Some(collection),
                ),
                Bare::Same
            )
        })
}

/// Emits the lint on `expr` with `replacement` as the fix. When the
/// rewrite needs `nami`'s `Collection` trait and it does not resolve to
/// the trait at `expr`, the diagnostic is a `help` naming the rewrite and
/// the import — the `use` is never auto-inserted (see the lint's
/// known-problems note).
fn report(
    cx: &LateContext<'_>,
    expr: &Expr<'_>,
    message: &'static str,
    replacement: String,
    needs_collection: bool,
    applicability: Applicability,
) {
    span_lint_and_then(cx, NEEDLESS_LIST_SNAPSHOT, expr.span, message, |diag| {
        if needs_collection && !collection_in_scope(cx, expr) {
            diag.help(format!(
                "use the `List` method — `{replacement}` needs `use {COLLECTION_USE};` in scope"
            ));
            return;
        }
        diag.span_suggestion(expr.span, SUGGESTION_LABEL, replacement, applicability);
    });
}

impl<'tcx> LateLintPass<'tcx> for NeedlessListSnapshot {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        // A `for` loop desugars into a `match` under an expansion mark, so
        // it is recognized before the expansion guard; the head expression
        // keeps its written span.
        if let Some(for_loop) = ForLoop::hir(expr) {
            let typeck = cx.typeck_results();
            let Some(list) = snapshot_receiver(cx, typeck, for_loop.arg) else {
                return;
            };
            let mut applicability = Applicability::MachineApplicable;
            let list = snippet_with_applicability(cx, list.span, "..", &mut applicability);
            // `for x in list.iter()` for every receiver shape —
            // `List::iter(&self)` returns the owned `vec::IntoIter<T>`, so
            // the borrow ends at the call and the body may still move,
            // reassign, or `&mut`-borrow the receiver. `&List`/`&mut List`
            // and `Rc<List>` receivers auto-ref at the call, so the same
            // text applies.
            report(
                cx,
                for_loop.arg,
                ITER_MESSAGE,
                format!("{list}.iter()"),
                false,
                applicability,
            );
            return;
        }
        if expr.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        let ExprKind::MethodCall(segment, receiver, args, _) = expr.kind else {
            return;
        };
        let name = segment.ident.name.as_str();
        // `x.snapshot().get(i).cloned()` / `x.snapshot().first().cloned()` —
        // only the `.cloned()` turns the borrowed `Option<&T>` into the
        // `Option<T>` `Collection::get` returns; a bare `get`/`first` reads
        // the snapshot by reference and stays silent.
        if name == "cloned" && args.is_empty() {
            let Some((list, index)) = snapshot_option_ref(cx, typeck, receiver) else {
                return;
            };
            let mut applicability = Applicability::MachineApplicable;
            let list = snippet_with_applicability(cx, list.span, "..", &mut applicability);
            let replacement = match index {
                Some(arg) => {
                    let arg = snippet_with_applicability(cx, arg.span, "..", &mut applicability);
                    format!("{list}.get({arg})")
                }
                None => format!("{list}.get(0)"),
            };
            report(cx, expr, COPY_MESSAGE, replacement, true, applicability);
            return;
        }
        let Some(list) = snapshot_receiver(cx, typeck, receiver) else {
            return;
        };
        let (method, needs_collection, message) = match name {
            "len" | "is_empty" if args.is_empty() => (name, true, COPY_MESSAGE),
            // `into_iter` is `vec::IntoIter<T>` either way, and `iter()` is
            // the `List` spelling. `snapshot().iter()` stays silent —
            // `[T]::iter` yields `&T` while `List::iter` yields `T`.
            "into_iter" if args.is_empty() => ("iter", false, ITER_MESSAGE),
            _ => return,
        };
        let mut applicability = Applicability::MachineApplicable;
        let list = snippet_with_applicability(cx, list.span, "..", &mut applicability);
        report(
            cx,
            expr,
            message,
            format!("{list}.{method}()"),
            needs_collection,
            applicability,
        );
    }
}
