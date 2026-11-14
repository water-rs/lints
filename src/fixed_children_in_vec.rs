use clippy_utils::higher::{ForLoop, VecArgs};
use clippy_utils::res::MaybeResPath;
use clippy_utils::source::snippet_with_applicability;
use clippy_utils::span_contains_comment;
use rustc_data_structures::fx::FxHashMap;
use rustc_errors::Applicability;
use rustc_hir::{Block, Expr, ExprKind, HirId, StmtKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TypeckResults;
use rustc_session::declare_lint_pass;
use rustc_span::Span;

use crate::def_path::def_path_eq;
use crate::diagnostics::{span_lint_and_sugg, span_lint_and_then};
use crate::param_bounds::{call_args, call_def_id};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `vstack`/`hstack`/`zstack` and `VStack::new`/`HStack::new`/
    /// `ZStack::new` calls whose contents argument is a literal `vec![..]`.
    ///
    /// ### Why is this bad?
    ///
    /// Stack contents are `TupleViews`: a tuple is the framework's shape for
    /// a fixed set of children and `Vec` is the shape for a dynamic one. A
    /// literal `vec![..]` allocates a vector the layout immediately repacks,
    /// and reads as if the children were computed rather than fixed.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// vstack(vec![text("a"), text("b"), text("c")])
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// vstack((text("a"), text("b"), text("c")))
    /// ```
    pub FIXED_CHILDREN_IN_VEC,
    pedantic,
    "a fixed set of children as `vec!`"
}

/// `PUSH_LOOP_SEED` shares this module with the lint above — one pass
/// recognizes both shapes — so its declaration lives in a submodule to keep
/// the two `LINT_INFO`s apart.
pub(crate) mod push_loop_seed {
    declare_waterui_lint! {
        /// ### What it does
        ///
        /// Flags a `reactive::collection::List::new()` (`ReactiveList::new()`)
        /// whose local is seeded by a `for` loop in the same block whose body
        /// is a single `list.push(..)` over an existing sequence.
        ///
        /// ### Why is this bad?
        ///
        /// `ReactiveList::from(vec)` moves the vector into the list in one
        /// step; `new()` plus a push loop allocates an empty list and grows
        /// it an element at a time, and the loop reads as if the seeding were
        /// conditional.
        ///
        /// ### Example
        ///
        /// ```rust,ignore
        /// let list = ReactiveList::new();
        /// for item in items {
        ///     list.push(item);
        /// }
        /// ```
        ///
        /// Use instead:
        ///
        /// ```rust,ignore
        /// let list = ReactiveList::from(items);
        /// ```
        pub PUSH_LOOP_SEED,
        pedantic,
        "a `ReactiveList` seeded by a push loop"
    }
}

declare_lint_pass!(FixedChildrenInVec => [
    FIXED_CHILDREN_IN_VEC,
    push_loop_seed::PUSH_LOOP_SEED,
]);

/// Stack constructors whose contents parameter is `impl TupleViews`: the
/// `vstack`/`hstack`/`zstack` free functions (sole argument) and the
/// `VStack`/`HStack`/`ZStack` `new` constructors (last argument) —
/// `waterui-layout-0.3.2/src/stack/{vstack,hstack,zstack}.rs`.
const STACK_CTORS: &[&[&str]] = &[
    &["waterui_layout", "stack", "vstack", "vstack"],
    &["waterui_layout", "stack", "hstack", "hstack"],
    &["waterui_layout", "stack", "zstack", "zstack"],
    &["waterui_layout", "stack", "vstack", "VStack", "new"],
    &["waterui_layout", "stack", "hstack", "HStack", "new"],
    &["waterui_layout", "stack", "zstack", "ZStack", "new"],
];

/// `nami::data::collection::List::new` — `ReactiveList::new()` in facade
/// spelling (`nami-0.11.2/src/data/collection.rs`).
const LIST_NEW: &[&str] = &["nami", "data", "collection", "List", "new"];

/// `nami::data::collection::List::push`.
const LIST_PUSH: &[&str] = &["nami", "data", "collection", "List", "push"];

const VEC_MSG: &str = "a fixed set of children as `vec!`";
const VEC_HELP: &str = "use a tuple for a fixed set of children";
const SEED_MSG: &str = "a `ReactiveList` seeded by a push loop";
const SEED_LET_LABEL: &str = "the `ReactiveList` is created empty here";
const SEED_HELP: &str = "`ReactiveList::from(vec)` seeds in one move; map first, then convert: `ReactiveList::from(items.into_iter().map(f).collect::<Vec<_>>())`";

/// The expression of a single-expression block — `{ e }`, `{ e; }`, nested
/// single-expression blocks, drop-temps peeled. `None` for any other shape:
/// a two-statement or conditional body is more than one effect.
fn sole_expr<'hir>(mut expr: &'hir Expr<'hir>) -> Option<&'hir Expr<'hir>> {
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(block, _) => match (block.stmts, block.expr) {
                ([], Some(tail)) => tail,
                ([stmt], None) => match stmt.kind {
                    StmtKind::Expr(e) | StmtKind::Semi(e) => e,
                    StmtKind::Let(_) | StmtKind::Item(_) => return None,
                },
                _ => return None,
            },
            _ => return Some(expr),
        };
    }
}

/// Whether `expr`, references and drop-temps peeled, is a local or an
/// `iter()`/`iter_mut()`/`into_iter()` on one — an in-memory sequence the
/// loop could equally well have moved into `ReactiveList::from`.
fn iterates_local(expr: &Expr<'_>) -> bool {
    fn peel<'hir>(expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
        let mut expr = expr;
        loop {
            expr = match expr.kind {
                ExprKind::AddrOf(.., inner) | ExprKind::DropTemps(inner) => inner,
                _ => return expr,
            };
        }
    }
    let head = peel(expr);
    if head.res_local_id().is_some() {
        return true;
    }
    matches!(head.kind, ExprKind::MethodCall(segment, receiver, ..)
        if matches!(segment.ident.as_str(), "iter" | "iter_mut" | "into_iter")
            && peel(receiver).res_local_id().is_some())
}

/// The `fixed_children_in_vec` half: `expr` is a stack-constructor call whose
/// contents argument is a literal `vec![a, b, c]` — suggest the tuple.
fn check_stack_contents<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    if expr.span.from_expansion() || !matches!(expr.kind, ExprKind::Call(..)) {
        return;
    }
    let Some(did) = call_def_id(cx.typeck_results(), expr) else {
        return;
    };
    if !STACK_CTORS.iter().any(|path| def_path_eq(cx, did, path)) {
        return;
    }
    let Some(&contents) = call_args(expr).last() else {
        return;
    };
    let Some(VecArgs::Vec(children)) = VecArgs::hir(cx, contents) else {
        return;
    };
    // `contents` is the `vec!` expansion's root node; `parent_callsite` gives
    // the `vec![..]` invocation in the caller's syntax context.
    let vec_span = contents.span.parent_callsite().unwrap_or(contents.span);
    let mut applicability = Applicability::MachineApplicable;
    let children: Vec<_> = children
        .iter()
        .map(|child| {
            snippet_with_applicability(cx, child.span.source_callsite(), "..", &mut applicability)
        })
        .collect();
    let tuple = match children.as_slice() {
        [] => "()".to_owned(),
        [one] => format!("({one},)"),
        many => format!(
            "({})",
            many.iter()
                .map(|s| s.as_ref())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    if span_contains_comment(cx, vec_span) {
        applicability = Applicability::Unspecified;
    }
    span_lint_and_sugg(
        cx,
        FIXED_CHILDREN_IN_VEC,
        vec_span,
        VEC_MSG,
        VEC_HELP,
        tuple,
        applicability,
    );
}

/// The `push_loop_seed` half: `expr` is a statement-position `for` loop in
/// `block`; fire when its body is a single `list.push(..)` on a local that an
/// earlier `let` in `seeds` created with `ReactiveList::new()` and its head
/// iterates a local sequence.
fn check_seed_loop<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    expr: &'tcx Expr<'tcx>,
    seeds: &FxHashMap<HirId, Span>,
) {
    let Some(for_loop) = ForLoop::hir(expr) else {
        return;
    };
    if for_loop.span.from_expansion() {
        return;
    }
    let Some(push) = sole_expr(for_loop.body) else {
        return;
    };
    if !matches!(push.kind, ExprKind::MethodCall(..)) {
        return;
    }
    let Some(did) = call_def_id(typeck, push) else {
        return;
    };
    if !def_path_eq(cx, did, LIST_PUSH) {
        return;
    }
    let &[receiver, _value] = call_args(push).as_slice() else {
        return;
    };
    let Some(local) = receiver.res_local_id() else {
        return;
    };
    let Some(&let_span) = seeds.get(&local) else {
        return;
    };
    if !iterates_local(for_loop.arg) {
        return;
    }
    span_lint_and_then(
        cx,
        push_loop_seed::PUSH_LOOP_SEED,
        for_loop.span,
        SEED_MSG,
        |diag| {
            diag.span_label(let_span, SEED_LET_LABEL);
            diag.help(SEED_HELP);
        },
    );
}

impl<'tcx> LateLintPass<'tcx> for FixedChildrenInVec {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        check_stack_contents(cx, expr);
    }

    fn check_block(&mut self, cx: &LateContext<'tcx>, block: &'tcx Block<'tcx>) {
        if block.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        // `let <pat> = ReactiveList::new();` seen so far in this block:
        // bound local -> the let's span. A `for` further down that pushes to
        // one of these locals is seeding it.
        let mut seeds: FxHashMap<HirId, Span> = FxHashMap::default();
        for stmt in block.stmts {
            match stmt.kind {
                StmtKind::Let(local) => {
                    if let Some(init) = local.init
                        && call_def_id(typeck, init)
                            .is_some_and(|did| def_path_eq(cx, did, LIST_NEW))
                    {
                        local.pat.each_binding(|_, hir_id, _, _| {
                            seeds.insert(hir_id, local.span);
                        });
                    }
                }
                StmtKind::Expr(expr) | StmtKind::Semi(expr) => {
                    check_seed_loop(cx, typeck, expr, &seeds);
                }
                StmtKind::Item(_) => {}
            }
        }
        if let Some(tail) = block.expr {
            check_seed_loop(cx, typeck, tail, &seeds);
        }
    }
}
