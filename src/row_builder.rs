//! Recognition of row-builder calls — `ForEach::new` and the `for_each`
//! associated functions — whose second argument is a generator closure run
//! once per row per rebuild.

use rustc_hir::{Closure, Expr, ExprKind};
use rustc_lint::LateContext;

use crate::anyview::peel;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};

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

/// The generator closure of a row-builder call — `ForEach::new`,
/// `Lazy::for_each`, `List::for_each`, or `VStack`/`HStack`/`ZStack::for_each`
/// — when `call` is one and its second argument is a closure literal. A
/// non-closure generator — a named function, a variable — yields `None`: what
/// it does with the item is not visible at the call site.
pub(crate) fn row_builder_closure<'tcx>(
    cx: &LateContext<'tcx>,
    call: &'tcx Expr<'tcx>,
) -> Option<&'tcx Closure<'tcx>> {
    if !matches!(call.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        || !call_def_id(cx.typeck_results(), call)
            .map(|did| implemented_trait_item(cx.tcx, did))
            .is_some_and(|did| {
                ROW_BUILDER_CALLS
                    .iter()
                    .any(|path| crate::def_path::def_path_eq(cx, did, path))
            })
    {
        return None;
    }
    let ExprKind::Closure(closure) = peel(call_args(call).get(1)?).kind else {
        return None;
    };
    Some(closure)
}
