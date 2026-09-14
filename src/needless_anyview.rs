use clippy_utils::diagnostics::{span_lint_and_help, span_lint_and_sugg, span_lint_and_then};
use clippy_utils::source::snippet_opt;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::LocalDefId;
use rustc_hir::intravisit::{self, FnKind, Visitor, nested_filter};
use rustc_hir::{
    Body, BodyId, Expr, ExprKind, FnDecl, FnRetTy, GenericBound, HirId, ImplItemImplKind, Node,
    QPath, TyKind,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter::OnlyBodies;
use rustc_middle::ty::{self, TyCtxt, TypeckResults};
use rustc_session::declare_lint_pass;
use rustc_span::Span;
use std::mem;

use crate::anyview::{ANYVIEW, erased_inner, is_anyview, peel};
use crate::param_bounds::{BoundTarget, call_arg_bounds_in, call_args};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `AnyView::new(v)` and `v.anyview()` calls that type-erase a view
    /// where the surrounding position already accepts `impl View`: arguments
    /// to parameters bounded by `View`, `TupleViews`, `Views`, or
    /// `ViewBuilder`; tail expressions and `return`s of `-> impl View`
    /// functions; and `match`/`if` arms whose erased values all share one
    /// pre-erasure type. A `-> AnyView` signature whose body neither branches
    /// nor keeps an `AnyView` anywhere else gets a second message suggesting
    /// `-> impl View`.
    ///
    /// ### Why is this bad?
    ///
    /// `AnyView` boxes the view and dispatches `body` through a vtable.
    /// Erasure is required in exactly four places — a struct field typed
    /// `AnyView`, a `-> AnyView` signature, a `Vec<AnyView>`/`collect`, and
    /// `match`/`if` arms that unify different types — and everywhere else the
    /// box and the dispatch hop are paid for nothing.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// scroll(AnyView::new(text("hello")))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// scroll(text("hello"))
    /// ```
    pub NEEDLESS_ANYVIEW,
    style,
    "erasing to `AnyView` where `impl View` is already accepted"
}

declare_lint_pass!(NeedlessAnyview => [NEEDLESS_ANYVIEW]);

/// `View` — a bound that makes an argument position accept any view.
const VIEW: &[&str] = &["waterui_core", "ui", "view", "View"];

/// `ViewExt` — `-> impl ViewExt` also accepts `AnyView`.
const VIEW_EXT: &[&str] = &["waterui_internal", "view", "ViewExt"];

/// `TupleViews` — the parameter is a tuple/array of views, so each element is
/// a view position.
const TUPLE_VIEWS: &[&str] = &["waterui_core", "ui", "view", "TupleViews"];

/// `Views` — likewise for a collection of views.
const VIEWS: &[&str] = &["waterui_core", "ui", "views", "Views"];

/// `ViewBuilder` — the parameter produces a view, so a closure argument's
/// return positions are view positions.
const VIEW_BUILDER: &[&str] = &["waterui_core", "foundation", "handler", "ViewBuilder"];

/// Bounds that make an argument position accept views.
const VIEW_POSITION_BOUNDS: &[&[&str]] = &[VIEW, TUPLE_VIEWS, VIEWS, VIEW_BUILDER];

/// Bounds on a `-> impl ..` return type that `AnyView` itself satisfies.
const VIEW_RET_BOUNDS: &[&[&str]] = &[VIEW, VIEW_EXT];

/// The `TupleViews`/`Views` family — bounds whose argument is a collection of
/// views.
const COLLECTION_BOUNDS: &[&[&str]] = &[TUPLE_VIEWS, VIEWS];

const ERASURE_MSG: &str =
    "erasing to `AnyView` is unnecessary — the position already accepts `impl View`";
const IMPL_VIEW_MSG: &str = "the return type can be `impl View`";

/// The `TypeckResults` of the body containing `hir_id`.
fn typeck_of<'tcx>(tcx: TyCtxt<'tcx>, hir_id: HirId) -> &'tcx TypeckResults<'tcx> {
    let owner = tcx.hir_enclosing_body_owner(hir_id);
    tcx.typeck_body(tcx.hir_body_owned_by(owner).id())
}

/// Where an expression's value lands, from this lint's point of view.
enum Position {
    /// The position already accepts `impl View` — a view-bounded argument
    /// slot, or return flow of a `-> impl View` body. Stripping the erasure
    /// is machine-applicable.
    ViewSlot,
    /// Return flow of a `-> AnyView` signature the lint may rewrite — free
    /// functions and inherent methods only — with the `AnyView` type's span.
    /// A strip alone would break the signature, so single erasures stay
    /// silent here; `check_fn` and the branch check emit the combined fix.
    AnyViewSig { sig: Span },
    /// `AnyView` is genuinely needed (a field, a collection element, an
    /// annotated binding, a concrete-`AnyView` parameter, an unrewritable
    /// signature) or the lint cannot tell — never report. Inside a
    /// `-> AnyView` body it also suppresses the signature message: the body
    /// demonstrably keeps an `AnyView`.
    Kept,
    /// An unannotated `let` initializer — never report, but not evidence the
    /// body needs `AnyView`.
    Transparent,
}

/// Where `expr`'s value lands — decides whether erasing to `AnyView` is
/// required by the position rather than by the expression.
fn position<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Position {
    let mut cur = expr.hir_id;
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        match node {
            Node::Expr(parent) => match parent.kind {
                ExprKind::Ret(_) | ExprKind::Closure(_) | ExprKind::Become(_) => {
                    return ret_position(cx, expr.hir_id);
                }
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    return if in_view_slot(cx, parent, expr) {
                        Position::ViewSlot
                    } else {
                        Position::Kept
                    };
                }
                // Tuple elements ride with their container — where the tuple
                // lands decides whether erasing them was needed.
                ExprKind::Tup(_) | ExprKind::DropTemps(_) => cur = parent.hir_id,
                // A `Node::Block` was verified on the previous step; the block
                // *expression* is transparent — its value is its tail's.
                ExprKind::Block(block, _) if block.hir_id == cur => cur = parent.hir_id,
                // Array elements are handled by the array's own check: a strip
                // must keep every element the same type, so elements are never
                // their own diagnostic.
                ExprKind::Array(_) => return Position::Kept,
                _ => return Position::Kept,
            },
            // A block's tail expression has the `Block` node as parent; the
            // tail's value flows out of the block.
            Node::Block(block) => {
                if !block.expr.is_some_and(|tail| tail.hir_id == cur) {
                    return Position::Kept;
                }
                cur = block.hir_id;
            }
            Node::LetStmt(local) => {
                return if local.ty.is_none() {
                    Position::Transparent
                } else {
                    Position::Kept
                };
            }
            Node::Item(_) | Node::ImplItem(_) | Node::TraitItem(_) | Node::ForeignItem(_) => {
                return ret_position(cx, expr.hir_id);
            }
            _ => return Position::Kept,
        }
    }
    Position::Kept
}

/// The declared return type of a function or closure, as this lint cares
/// about it.
enum DeclaredRet {
    /// `-> impl ..` with a `View`/`ViewExt` bound — the position accepts any
    /// view, including the concrete one that was erased.
    ImplView,
    /// `-> AnyView`, with the `AnyView` type's span.
    AnyView(Span),
    /// Inferred or anything else.
    Other,
}

fn declared_ret(cx: &LateContext<'_>, decl: &FnDecl<'_>) -> DeclaredRet {
    let FnRetTy::Return(ty) = decl.output else {
        return DeclaredRet::Other;
    };
    match ty.kind {
        TyKind::OpaqueDef(opaque)
            if opaque.bounds.iter().any(|bound| match bound {
                GenericBound::Trait(poly) => match poly.trait_ref.path.res {
                    Res::Def(DefKind::Trait, did) => VIEW_RET_BOUNDS
                        .iter()
                        .any(|path| crate::def_path::def_path_eq(cx, did, path)),
                    _ => false,
                },
                _ => false,
            }) =>
        {
            DeclaredRet::ImplView
        }
        TyKind::Path(QPath::Resolved(_, path)) => match path.res {
            Res::Def(_, did) if crate::def_path::def_path_eq(cx, did, ANYVIEW) => {
                DeclaredRet::AnyView(ty.span)
            }
            _ => DeclaredRet::Other,
        },
        _ => DeclaredRet::Other,
    }
}

/// The position of a value exiting the enclosing body — a `return`, a tail
/// expression, or a closure body's value.
fn ret_position(cx: &LateContext<'_>, hir_id: HirId) -> Position {
    let owner = cx.tcx.hir_enclosing_body_owner(hir_id);
    let Some(decl) = cx
        .tcx
        .hir_fn_decl_by_hir_id(cx.tcx.local_def_id_to_hir_id(owner))
    else {
        return Position::Kept;
    };
    match declared_ret(cx, decl) {
        DeclaredRet::ImplView => Position::ViewSlot,
        DeclaredRet::AnyView(sig) => match cx.tcx.hir_node_by_def_id(owner) {
            Node::Item(_) => Position::AnyViewSig { sig },
            Node::ImplItem(item) => match item.impl_kind {
                // A trait-implementing method must keep the trait's
                // signature; only inherent methods may move to `impl View`.
                ImplItemImplKind::Inherent { .. } => Position::AnyViewSig { sig },
                ImplItemImplKind::Trait { .. } => Position::Kept,
            },
            _ => Position::Kept,
        },
        // Inferred return — when the body is a closure passed to a
        // `ViewBuilder` parameter its return positions are still view slots.
        DeclaredRet::Other => match cx.tcx.hir_node_by_def_id(owner) {
            Node::Expr(
                closure @ Expr {
                    kind: ExprKind::Closure(..),
                    ..
                },
            ) if closure_is_builder_arg(cx, closure) => Position::ViewSlot,
            _ => Position::Kept,
        },
    }
}

/// Whether `needle` sits in a view position among `call`'s arguments — a
/// `View`-bounded argument itself, an element of a `TupleViews`/`Views`
/// argument, or a return position of a `ViewBuilder` closure argument.
fn in_view_slot<'tcx>(cx: &LateContext<'tcx>, call: &'tcx Expr<'tcx>, needle: &Expr<'tcx>) -> bool {
    let typeck = typeck_of(cx.tcx, call.hir_id);
    let Some((_, bounds)) = call_arg_bounds_in(cx, typeck, call, &[VIEW_POSITION_BOUNDS]) else {
        return false;
    };
    call_args(call)
        .into_iter()
        .zip(bounds)
        .any(|(arg, targets)| {
            targets.iter().any(|target| {
                if is_bound(cx, target, VIEW) {
                    peel(arg).hir_id == needle.hir_id
                } else if is_bound(cx, target, TUPLE_VIEWS) || is_bound(cx, target, VIEWS) {
                    collection_slots(cx, typeck, peel(arg))
                        .iter()
                        .any(|slot| slot.hir_id == needle.hir_id)
                } else if is_bound(cx, target, VIEW_BUILDER) {
                    builder_slots(cx.tcx, peel(arg))
                        .iter()
                        .any(|slot| slot.hir_id == needle.hir_id)
                } else {
                    false
                }
            })
        })
}

fn is_bound(cx: &LateContext<'_>, target: &BoundTarget<'_>, path: &[&'static str]) -> bool {
    crate::def_path::def_path_eq(cx, target.trait_did, path)
}

/// Whether `closure` — a `Fn() -> V` value — is an argument to a
/// `ViewBuilder`-bounded parameter, making its return positions view slots.
fn closure_is_builder_arg<'tcx>(cx: &LateContext<'tcx>, closure: &Expr<'tcx>) -> bool {
    let mut cur = closure.hir_id;
    for (_, node) in cx.tcx.hir_parent_iter(closure.hir_id) {
        match node {
            Node::Expr(parent) => match parent.kind {
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    let typeck = typeck_of(cx.tcx, parent.hir_id);
                    return call_arg_bounds_in(cx, typeck, parent, &[&[VIEW_BUILDER]]).is_some_and(
                        |(_, bounds)| {
                            call_args(parent)
                                .into_iter()
                                .zip(bounds)
                                .any(|(arg, targets)| {
                                    !targets.is_empty() && peel(arg).hir_id == closure.hir_id
                                })
                        },
                    );
                }
                ExprKind::DropTemps(_) => cur = parent.hir_id,
                ExprKind::Block(block, _) if block.hir_id == cur => cur = parent.hir_id,
                _ => return false,
            },
            Node::Block(block) if block.expr.is_some_and(|tail| tail.hir_id == cur) => {
                cur = block.hir_id;
            }
            _ => return false,
        }
    }
    false
}

/// Whether `expr` — an array literal — is itself a `TupleViews`/`Views`-
/// bounded argument: the collection sits in a view position.
fn views_collection_arg<'tcx>(cx: &LateContext<'tcx>, expr: &Expr<'tcx>) -> bool {
    let mut cur = expr.hir_id;
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        match node {
            Node::Expr(parent) => match parent.kind {
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    let typeck = typeck_of(cx.tcx, parent.hir_id);
                    return call_arg_bounds_in(cx, typeck, parent, &[COLLECTION_BOUNDS])
                        .is_some_and(|(_, bounds)| {
                            call_args(parent)
                                .into_iter()
                                .zip(bounds)
                                .any(|(arg, targets)| {
                                    !targets.is_empty() && peel(arg).hir_id == expr.hir_id
                                })
                        });
                }
                ExprKind::DropTemps(_) => cur = parent.hir_id,
                ExprKind::Block(block, _) if block.hir_id == cur => cur = parent.hir_id,
                _ => return false,
            },
            Node::Block(block) if block.expr.is_some_and(|tail| tail.hir_id == cur) => {
                cur = block.hir_id;
            }
            _ => return false,
        }
    }
    false
}

/// The view-typed positions inside a `TupleViews`/`Views` argument — tuple
/// elements, the repeat element, or array elements when every element erases
/// the same pre-erasure type (a strip must keep the array homogeneous).
fn collection_slots<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Vec<&'tcx Expr<'tcx>> {
    match expr.kind {
        ExprKind::Tup(elements) => elements.iter().map(|e| peel(e)).collect(),
        ExprKind::Array(elements) if uniformly_erased(cx, typeck, elements.iter()).is_some() => {
            elements.iter().map(|e| peel(e)).collect()
        }
        ExprKind::Repeat(element, _) => vec![peel(element)],
        _ => Vec::new(),
    }
}

/// The return positions of a `Fn() -> V` closure argument under a
/// `ViewBuilder` bound — its tail and its `return` operands.
fn builder_slots<'tcx>(tcx: TyCtxt<'tcx>, expr: &'tcx Expr<'tcx>) -> Vec<&'tcx Expr<'tcx>> {
    let ExprKind::Closure(closure) = expr.kind else {
        return Vec::new();
    };
    let body = tcx.hir_body(closure.body);
    let mut slots = vec![body.value];
    ReturnExprs { slots: &mut slots }.visit_expr(body.value);
    slots
}

/// Collects `return <expr>` operands. Nested bodies are not entered
/// (`nested_filter::None`), so only the visited body's own `return`s land in
/// `slots`.
struct ReturnExprs<'a, 'tcx> {
    slots: &'a mut Vec<&'tcx Expr<'tcx>>,
}

impl<'tcx> Visitor<'tcx> for ReturnExprs<'_, 'tcx> {
    type NestedFilter = nested_filter::None;

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if let ExprKind::Ret(Some(value)) = expr.kind {
            self.slots.push(value);
        }
        intravisit::walk_expr(self, expr);
    }
}

/// When every expr in `exprs` is an erasure of the same pre-erasure type: the
/// `(erasure, inner)` pairs.
fn uniformly_erased<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    exprs: impl IntoIterator<Item = &'tcx Expr<'tcx>>,
) -> Option<Vec<(&'tcx Expr<'tcx>, &'tcx Expr<'tcx>)>> {
    let erased = exprs
        .into_iter()
        .map(|expr| {
            let inner = erased_inner(cx, typeck, peel(expr))?;
            // Erasures of an `AnyView` belong to `redundant_anyview`.
            (!is_anyview(cx, typeck.expr_ty(inner))).then_some((expr, inner))
        })
        .collect::<Option<Vec<_>>>()?;
    let (first, rest) = erased.split_first()?;
    let first_ty = typeck.expr_ty(first.1);
    if matches!(first_ty.kind(), ty::TyKind::Error(_))
        || rest
            .iter()
            .any(|(_, inner)| typeck.expr_ty(inner) != first_ty)
    {
        return None;
    }
    Some(erased)
}

/// The `match`/`if` rule: when every arm erases a value of the same
/// pre-erasure type, the erasure unified nothing — one diagnostic covering
/// every arm.
fn check_branches<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    branches: impl IntoIterator<Item = &'tcx Expr<'tcx>>,
) {
    let Some(erased) = uniformly_erased(cx, cx.typeck_results(), branches) else {
        return;
    };
    match position(cx, expr) {
        Position::ViewSlot => {
            report_branches(cx, expr, &erased, None, Applicability::MachineApplicable);
        }
        Position::AnyViewSig { sig } => {
            report_branches(cx, expr, &erased, Some(sig), Applicability::MaybeIncorrect);
        }
        Position::Kept | Position::Transparent => {}
    }
}

/// One diagnostic spanning `expr`, suggesting each arm's erasure be stripped
/// — plus the `-> AnyView` → `-> impl View` rewrite when `sig` is given.
fn report_branches(
    cx: &LateContext<'_>,
    expr: &Expr<'_>,
    erased: &[(&Expr<'_>, &Expr<'_>)],
    sig: Option<Span>,
    applicability: Applicability,
) {
    span_lint_and_then(cx, NEEDLESS_ANYVIEW, expr.span, ERASURE_MSG, |diag| {
        let mut suggestions = Vec::with_capacity(erased.len() + 1);
        for (erasure, inner) in erased {
            let Some(snippet) = snippet_opt(cx, inner.span) else {
                return;
            };
            suggestions.push((erasure.span, snippet));
        }
        let label = match sig {
            Some(sig) => {
                suggestions.push((sig, "impl View".to_owned()));
                "drop the `AnyView` wrappers and return `impl View`"
            }
            None => "drop the `AnyView` wrappers",
        };
        diag.multipart_suggestion(label, suggestions, applicability);
    });
}

/// The strip fix for a single erasure in a `ViewSlot` position.
fn report_strip(cx: &LateContext<'_>, erasure: &Expr<'_>, inner: &Expr<'_>) {
    match snippet_opt(cx, inner.span) {
        Some(snippet) => span_lint_and_sugg(
            cx,
            NEEDLESS_ANYVIEW,
            erasure.span,
            ERASURE_MSG,
            "drop the `AnyView` wrapper",
            snippet,
            Applicability::MachineApplicable,
        ),
        None => span_lint_and_help(
            cx,
            NEEDLESS_ANYVIEW,
            erasure.span,
            ERASURE_MSG,
            None,
            "drop the `AnyView` wrapper",
        ),
    }
}

/// Scans a `-> AnyView` body: collects the return-flow erasures the signature
/// rewrite would strip (`returns`), and detects the two things that keep the
/// signature honest — branching (`match`/`if`) and an `AnyView` that flows
/// into a field, collection, annotation, or concrete-`AnyView` parameter
/// (`kept`).
struct SigRewrite<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    typeck: &'tcx TypeckResults<'tcx>,
    returns: Vec<(&'tcx Expr<'tcx>, &'tcx Expr<'tcx>)>,
    kept: bool,
    branching: bool,
}

impl<'tcx> Visitor<'tcx> for SigRewrite<'_, 'tcx> {
    type NestedFilter = OnlyBodies;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.cx.tcx
    }

    fn visit_nested_body(&mut self, id: BodyId) {
        let typeck = mem::replace(&mut self.typeck, self.cx.tcx.typeck_body(id));
        self.visit_body(self.cx.tcx.hir_body(id));
        self.typeck = typeck;
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if self.branching || self.kept {
            return;
        }
        if let ExprKind::Match(..) | ExprKind::If(..) = expr.kind
            && !expr.span.from_expansion()
        {
            self.branching = true;
            return;
        }
        if let Some(inner) = erased_inner(self.cx, self.typeck, expr) {
            match position(self.cx, expr) {
                // Erasures of an `AnyView` belong to `redundant_anyview` —
                // the signature rewrite leaves them to that lint's fix.
                Position::AnyViewSig { .. } if !is_anyview(self.cx, self.typeck.expr_ty(inner)) => {
                    self.returns.push((expr, inner));
                }
                Position::Kept => self.kept = true,
                Position::AnyViewSig { .. } | Position::ViewSlot | Position::Transparent => {}
            }
        }
        intravisit::walk_expr(self, expr);
    }
}

impl<'tcx> LateLintPass<'tcx> for NeedlessAnyview {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        match expr.kind {
            ExprKind::Match(_, arms, _) => {
                check_branches(cx, expr, arms.iter().map(|arm| arm.body))
            }
            ExprKind::If(_, then, Some(else_)) => check_branches(cx, expr, [then, else_]),
            ExprKind::Array(elements) => {
                // Like arms: every element must erase the same type, and one
                // diagnostic covers them all since a strip has to keep the
                // array homogeneous.
                if let Some(erased) = uniformly_erased(cx, typeck, elements.iter())
                    && views_collection_arg(cx, expr)
                {
                    report_branches(cx, expr, &erased, None, Applicability::MachineApplicable);
                }
            }
            _ => {}
        }
        if let Some(inner) = erased_inner(cx, typeck, expr)
            // `redundant_anyview` owns erasures whose inner is already an
            // `AnyView` — this lint would report the same call on one span.
            && !is_anyview(cx, typeck.expr_ty(inner))
            && let Position::ViewSlot = position(cx, expr)
        {
            report_strip(cx, expr, inner);
        }
    }

    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        kind: FnKind<'tcx>,
        decl: &'tcx FnDecl<'tcx>,
        body: &'tcx Body<'tcx>,
        _span: Span,
        id: LocalDefId,
    ) {
        // Only free functions and inherent methods may change their
        // signature — trait-declared and trait-implementing signatures are
        // fixed, and closures have no signature to rewrite.
        let rewritable = !matches!(kind, FnKind::Closure)
            && match cx.tcx.hir_node_by_def_id(id) {
                Node::Item(_) => true,
                Node::ImplItem(item) => matches!(item.impl_kind, ImplItemImplKind::Inherent { .. }),
                _ => false,
            };
        let DeclaredRet::AnyView(sig) = declared_ret(cx, decl) else {
            return;
        };
        if !rewritable || sig.from_expansion() || body.value.span.from_expansion() {
            return;
        }
        let mut scan = SigRewrite {
            cx,
            typeck: cx.typeck_results(),
            returns: Vec::new(),
            kept: false,
            branching: false,
        };
        scan.visit_expr(body.value);
        if scan.branching || scan.kept {
            return;
        }
        span_lint_and_then(cx, NEEDLESS_ANYVIEW, sig, IMPL_VIEW_MSG, |diag| {
            let mut suggestions = Vec::with_capacity(scan.returns.len() + 1);
            for (erasure, inner) in &scan.returns {
                let Some(snippet) = snippet_opt(cx, inner.span) else {
                    return;
                };
                suggestions.push((erasure.span, snippet));
            }
            suggestions.push((sig, "impl View".to_owned()));
            diag.multipart_suggestion(
                "return `impl View` and drop the `AnyView` wrappers",
                suggestions,
                Applicability::MaybeIncorrect,
            );
        });
    }
}
