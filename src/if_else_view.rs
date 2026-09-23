use clippy_utils::paths::{PathNS, lookup_path};
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr};
use rustc_data_structures::fx::FxHashSet;
use rustc_errors::Applicability;
use rustc_hir::def::Namespace;
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind, HirId, ImplItemKind, ItemKind, Node, TraitItemKind, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyCtxt, TyKind};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, Symbol};
use std::cell::OnceCell;
use std::ops::ControlFlow;

use crate::diagnostics::span_lint_and_then;
use crate::imports::{Bare, bare_status, use_insertion};
use crate::param_bounds::{TUPLE_VIEWS_BOUNDS, VIEW_PARAM_BOUNDS, arg_has_bound, call_args};
use crate::signature::returns_impl_view;
use crate::snapshot_get::{get_receiver, is_live_signal, is_snapshot_get};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags an `if`/`else` in view position — the tail of a `-> impl View`
    /// body or an argument where a view is expected — whose arms unify to a
    /// WaterUI view (`AnyView`, `Text`, `VStack`, an opaque `impl View`, …).
    /// An `else if` chain reports once, on the outer `if`. An `if` in data
    /// position — a `let` initializer, a statement, a struct field — stays
    /// silent, as does one whose condition is a plain `bool` (`when` takes
    /// `impl IntoComputed<bool>`; only a signal-backed condition gets the
    /// `when(..)` suggestion).
    ///
    /// ### Why is this bad?
    ///
    /// `when(cond, || ..).otherwise(|| ..)` renders the same choice without
    /// erasing the arms — each closure keeps its own type — and a signal
    /// condition keeps the view switching. A `.get()` in the condition is
    /// worse: the branch is chosen once at build time and never
    /// re-evaluates.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// if logged_in.get() { text("hi") } else { text("login") }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// when(logged_in.clone(), || text("hi")).otherwise(|| text("login"))
    /// ```
    pub IF_ELSE_VIEW,
    style,
    "`if`/`else` choosing between views instead of a reactive `when` chain"
}

/// `waterui_core::ui::view::View` — the trait a flagged `if` type must
/// implement.
const VIEW: &[&str] = &["waterui_core", "ui", "view", "View"];

/// `waterui_internal::widget::condition::when` — the function a bare `when`
/// must already mean for the fix to skip the `use`.
const WHEN: &[&str] = &["waterui_internal", "widget", "condition", "when"];

/// `nami`'s `Signal` — `bool` implements it too (`impl_constant!`, a
/// no-op `watch`), so the suggestion gate is [`is_live_signal`], not the
/// trait alone.
const SIGNAL: &[&str] = &["nami_core", "Signal"];

/// The `use` line the fix writes — spelled through the `waterui` facade,
/// which is how users import it.
const WHEN_USE: &str = "waterui::widget::condition::when";

const PLAIN_MSG: &str = "`if`/`else` on views — `when(cond, || ..).otherwise(|| ..)` needs no erasure and stays reactive";
const SNAPSHOT_MSG: &str =
    "this branch is chosen once; `when(x, || ..).otherwise(|| ..)` switches reactively";
const SNAPSHOT_NOTE: &str =
    "the `.get()` reads the signal once — the chosen branch never re-evaluates";
const SUGGESTION: &str = "rewrite as `when(cond, || ..).otherwise(|| ..)`";
const DEEP_GET_HELP: &str = "derive a `Computed<bool>` with `map`/`zip` and pass it to `when`";
const GENERIC_HELP: &str = "import `waterui::widget::condition::when` and rewrite the branch as `when(cond, || ..).otherwise(|| ..)`";

/// The lint pass. `chained` records the HirIds of `else if` links an outer
/// `if` already covers, so a chain reports once; `view`/`when`/`signal`
/// cache the resolved `DefId`s, which do not change within a crate.
#[derive(Default)]
pub struct IfElseView {
    chained: FxHashSet<HirId>,
    view: OnceCell<Option<DefId>>,
    when: OnceCell<Option<DefId>>,
    signal: OnceCell<Option<DefId>>,
}

impl_lint_pass!(IfElseView => [IF_ELSE_VIEW]);

/// Resolves `["crate", "module", .., "Item"]` — the first segment names a
/// dependency crate — to a `DefId`. Expensive; callers cache the result.
fn resolve(tcx: TyCtxt<'_>, ns: PathNS, path: &[&'static str]) -> Option<DefId> {
    let path: Vec<Symbol> = path.iter().map(|seg| Symbol::intern(seg)).collect();
    lookup_path(tcx, ns, &path).first().copied()
}

/// Whether `cond` snapshots a signal anywhere inside it — the branch is
/// then chosen once, at build time.
fn contains_get<'a>(cx: &LateContext<'a>, cond: &'a Expr<'a>) -> bool {
    for_each_expr(cx, cond, |expr| {
        if is_snapshot_get(cx, expr) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(Descend::Yes)
        }
    })
    .is_some()
}

/// `expr` with drop-temps stripped, when it is exactly `x.get()` on a signal
/// and `x` is a path — the receiver the fix clones into `when(..)` — and a
/// marker that the condition is signal-backed, which a `when` argument must
/// be.
fn path_get<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    let mut expr = expr;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    if !is_snapshot_get(cx, expr) {
        return None;
    }
    get_receiver(expr).filter(|receiver| matches!(receiver.kind, ExprKind::Path(..)))
}

/// The `when`/`or` argument a condition rewrites to — `signal` marks
/// whether the condition's value is signal-backed, the only condition
/// `when(cond, ..)` accepts.
struct CondText {
    text: String,
    signal: bool,
}

/// The `when`/`or` argument a condition rewrites to: `x.get()` becomes
/// `x.clone()`, `!x.get()` becomes `!x.clone()`, a plain `bool` stays
/// verbatim but is marked non-signal so no `when` suggestion is emitted.
/// `None` when the condition resists — a `.get()` deeper than the outer
/// call, or unreadable source.
fn cond_text<'a>(cx: &LateContext<'a>, lint: &IfElseView, cond: &'a Expr<'a>) -> Option<CondText> {
    if let Some(receiver) = path_get(cx, cond) {
        return snippet_opt(cx, receiver.span).map(|x| CondText {
            text: format!("{x}.clone()"),
            signal: true,
        });
    }
    if let ExprKind::Unary(UnOp::Not, operand) = cond.kind
        && let Some(receiver) = path_get(cx, operand)
    {
        return snippet_opt(cx, receiver.span).map(|x| CondText {
            text: format!("!{x}.clone()"),
            signal: true,
        });
    }
    if contains_get(cx, cond) {
        return None;
    }
    let signal = lint.is_signal_ty(cx, cx.typeck_results().expr_ty(cond));
    snippet_opt(cx, cond.span).map(|text| CondText { text, signal })
}

/// `|| <arm>` — an arm that is just one expression loses its braces
/// (`|| view(..)`); an arm with statements keeps them verbatim (`|| { .. }`).
fn arm_text<'a>(cx: &LateContext<'a>, arm: &Expr<'a>) -> Option<String> {
    let body = match arm.kind {
        ExprKind::Block(block, None) if block.stmts.is_empty() => block.expr.unwrap_or(arm),
        _ => arm,
    };
    Some(format!("|| {}", snippet_opt(cx, body.span)?))
}

/// Whether `closure` — an `if`'s enclosing closure expression — is passed
/// where a `View`/`ViewBuilder` parameter sits (`when(..)`'s `then`,
/// `.otherwise(..)`), making its tail a view position. The call lives in
/// the enclosing body, so its `TypeckResults` come from
/// `hir_enclosing_body_owner`, not `cx.typeck_results()`.
fn view_builder_arg<'tcx>(cx: &LateContext<'tcx>, closure: &'tcx Expr<'tcx>) -> bool {
    let Node::Expr(call) = cx.tcx.hir_node(cx.tcx.parent_hir_id(closure.hir_id)) else {
        return false;
    };
    if !matches!(call.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
        return false;
    }
    let Some(index) = call_args(call)
        .iter()
        .position(|arg| arg.hir_id == closure.hir_id)
    else {
        return false;
    };
    let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(call.hir_id));
    arg_has_bound(cx, typeck, call, &[VIEW_PARAM_BOUNDS], index)
}

impl IfElseView {
    /// Whether the signature of body owner `did` returns a view — `impl
    /// View`, or a concrete `V: View` (`-> Str`, `-> AnyView`, `-> T` on a
    /// `T: View` generic): its body's tail is a view position.
    fn returns_view_ty<'a>(
        &self,
        cx: &LateContext<'a>,
        did: rustc_hir::def_id::LocalDefId,
    ) -> bool {
        let output = cx
            .tcx
            .fn_sig(did)
            .instantiate_identity()
            .skip_norm_wip()
            .skip_binder()
            .output();
        self.is_view_ty(cx, output)
    }
}

/// Whether `expr` — an `if`/`else` that unifies to a view — feeds a view
/// position: the tail of a body whose signature returns a view, or an
/// argument (or a tuple/array element of one) whose parameter is bound by
/// `View`/`ViewBuilder`/`TupleViews`. Everywhere else — a `let`
/// initializer, a statement, a struct field, the `if`'s own condition —
/// the value is data, and picking between views there is the `if`'s job.
fn in_view_position<'tcx>(
    cx: &LateContext<'tcx>,
    lint: &IfElseView,
    expr: &'tcx Expr<'tcx>,
) -> bool {
    let mut cur = expr.hir_id;
    // Whether the climb passed through a tuple/array element — an element
    // of a `TupleViews` argument is a view position even though the
    // parameter's own bound is `TupleViews`, not `View`.
    let mut element = false;
    loop {
        let parent_id = cx.tcx.parent_hir_id(cur);
        match cx.tcx.hir_node(parent_id) {
            // `match .. { _ => if .. }` — adopt the match's own context.
            Node::Arm(arm) if arm.body.hir_id == cur => cur = parent_id,
            // The `if` is a block's tail — adopt the block's own context.
            // `Block` is a HIR node: the tail's parent is the block, whose
            // parent is the `ExprKind::Block` expression.
            Node::Block(block) if block.expr.is_some_and(|tail| tail.hir_id == cur) => {
                cur = parent_id;
            }
            Node::Expr(parent) => match parent.kind {
                ExprKind::DropTemps(inner) if inner.hir_id == cur => cur = parent_id,
                ExprKind::Block(block, _)
                    if block.hir_id == cur || block.expr.is_some_and(|tail| tail.hir_id == cur) =>
                {
                    cur = parent_id;
                }
                ExprKind::Match(_, arms, _) if arms.iter().any(|arm| arm.hir_id == cur) => {
                    cur = parent_id;
                }
                ExprKind::Ret(Some(inner)) | ExprKind::Become(inner) if inner.hir_id == cur => {
                    cur = parent_id;
                }
                ExprKind::Tup(elems) | ExprKind::Array(elems)
                    if elems.iter().any(|elem| elem.hir_id == cur) =>
                {
                    element = true;
                    cur = parent_id;
                }
                ExprKind::Repeat(elem, _) if elem.hir_id == cur => {
                    element = true;
                    cur = parent_id;
                }
                // `|| .. { if .. }` — the tail is a view position when the
                // closure itself declares `-> impl View` or is passed to a
                // view-builder parameter.
                ExprKind::Closure(closure) => {
                    return returns_impl_view(cx, closure.fn_decl) || view_builder_arg(cx, parent);
                }
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    let Some(index) = call_args(parent).iter().position(|arg| arg.hir_id == cur)
                    else {
                        return false;
                    };
                    let tables: &[&[&[&'static str]]] = if element {
                        &[VIEW_PARAM_BOUNDS, TUPLE_VIEWS_BOUNDS]
                    } else {
                        &[VIEW_PARAM_BOUNDS]
                    };
                    return arg_has_bound(cx, cx.typeck_results(), parent, tables, index);
                }
                _ => return false,
            },
            // `cur` is the body's tail — a view position only when the
            // item's signature returns a view.
            Node::Item(item) => {
                return match item.kind {
                    ItemKind::Fn { sig, .. } => {
                        returns_impl_view(cx, sig.decl)
                            || lint.returns_view_ty(cx, item.owner_id.def_id)
                    }
                    _ => false,
                };
            }
            Node::ImplItem(item) => {
                return match item.kind {
                    ImplItemKind::Fn(sig, _) => {
                        returns_impl_view(cx, sig.decl)
                            || lint.returns_view_ty(cx, item.owner_id.def_id)
                    }
                    _ => false,
                };
            }
            Node::TraitItem(item) => {
                return match item.kind {
                    TraitItemKind::Fn(sig, _) => {
                        returns_impl_view(cx, sig.decl)
                            || lint.returns_view_ty(cx, item.owner_id.def_id)
                    }
                    _ => false,
                };
            }
            _ => return false,
        }
    }
}

/// One `if`/`else if` link: the guarded arm, the `when`/`or` argument its
/// condition rewrites to (`None` when it resists), and whether the
/// condition snapshots a signal.
struct Branch<'tcx> {
    arm: &'tcx Expr<'tcx>,
    cond: Option<CondText>,
    get: bool,
}

impl IfElseView {
    fn view_did(&self, tcx: TyCtxt<'_>) -> Option<DefId> {
        *self.view.get_or_init(|| resolve(tcx, PathNS::Type, VIEW))
    }

    fn when_did(&self, tcx: TyCtxt<'_>) -> Option<DefId> {
        *self.when.get_or_init(|| resolve(tcx, PathNS::Value, WHEN))
    }

    fn signal_did(&self, tcx: TyCtxt<'_>) -> Option<DefId> {
        *self
            .signal
            .get_or_init(|| resolve(tcx, PathNS::Type, SIGNAL))
    }

    /// Whether `ty` — the condition's type — is a live signal
    /// (`Binding<bool>`, `Computed<bool>`, …). `bool` is a `Signal` too
    /// (`impl_constant!`), but a constant never updates, so only a signal
    /// with a real watch guard justifies the `when(..)` suggestion.
    fn is_signal_ty<'a>(&self, cx: &LateContext<'a>, ty: Ty<'a>) -> bool {
        self.signal_did(cx.tcx)
            .is_some_and(|signal| is_live_signal(cx, &[signal], ty))
    }

    /// Whether `ty` — the type an `if`/`else` unifies to — is a WaterUI
    /// view: an ADT defined in a `waterui*` crate (`AnyView`, `Text`,
    /// `VStack`, `When`, …) or an opaque `impl View`, in each case guarded
    /// by an actual `View` impl. `()`, `&str`, `Option`, `Result`, numbers
    /// and `!` never qualify no matter which traits they implement, so
    /// `if c { Some(v) } else { None }` and `if c { "a" } else { "b" }`
    /// stay silent.
    fn is_view_ty<'a>(&self, cx: &LateContext<'a>, ty: Ty<'a>) -> bool {
        let candidate = match ty.kind() {
            // `Str` moved from `waterui-str` to the `suiteki` crate; it is
            // still the framework's string view.
            TyKind::Adt(adt, _) => {
                let name = cx.tcx.crate_name(adt.did().krate);
                name.as_str().starts_with("waterui") || name.as_str() == "suiteki"
            }
            _ => ty.is_opaque(),
        };
        candidate
            && self
                .view_did(cx.tcx)
                .is_some_and(|view| implements_trait(cx, ty, view, &[]))
    }

    /// The `when(..).or(..)*.otherwise(..)` replacement, plus the `use`
    /// the call needs — unless a bare `when` already means the function
    /// (`Same`) or the name is taken (`Conflict`, which drops the
    /// suggestion entirely). `None` when any condition or arm resists a
    /// rewrite.
    fn fix<'a>(
        &self,
        cx: &LateContext<'a>,
        expr: &Expr<'a>,
        branches: &[Branch<'a>],
        last_else: &Expr<'a>,
    ) -> Option<Vec<(Span, String)>> {
        let mut parts = Vec::new();
        match bare_status(
            cx,
            expr.hir_id,
            expr.span.lo(),
            Symbol::intern("when"),
            Namespace::ValueNS,
            self.when_did(cx.tcx),
        ) {
            Bare::Same => {}
            Bare::Free | Bare::Unknown => {
                let (point, before, after) = use_insertion(cx, expr.hir_id, WHEN_USE)?;
                parts.push((point, format!("{before}use {WHEN_USE};{after}")));
            }
            Bare::Conflict => return None,
        }
        let mut text = String::new();
        for (index, branch) in branches.iter().enumerate() {
            let cond = &branch.cond.as_ref()?.text;
            let arm = arm_text(cx, branch.arm)?;
            if index == 0 {
                text.push_str(&format!("when({cond}, {arm})"));
            } else {
                text.push_str(&format!(".or({cond}, {arm})"));
            }
        }
        text.push_str(&format!(".otherwise({})", arm_text(cx, last_else)?));
        parts.push((expr.span, text));
        Some(parts)
    }
}

impl<'tcx> LateLintPass<'tcx> for IfElseView {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() || self.chained.contains(&expr.hir_id) {
            return;
        }
        let ExprKind::If(cond, then, Some(mut link)) = expr.kind else {
            return;
        };
        if matches!(cond.kind, ExprKind::Let(..)) {
            return;
        }
        // An `else if` link is another `If` in the `else` slot; the chain is
        // one diagnostic on the outer `if`, so each link's HirId is
        // remembered and skipped when its own `check_expr` runs. A chain
        // without a final `else` has type `()` and is silent anyway; an
        // `else if let` cannot be a `when` condition, so the chain is left
        // alone.
        let mut branches = vec![(cond, then)];
        let last_else = loop {
            match link.kind {
                ExprKind::If(next_cond, next_then, Some(next_else)) => {
                    self.chained.insert(link.hir_id);
                    if matches!(next_cond.kind, ExprKind::Let(..)) {
                        return;
                    }
                    branches.push((next_cond, next_then));
                    link = next_else;
                }
                ExprKind::If(..) => return,
                _ => break link,
            }
        };
        if !self.is_view_ty(cx, cx.typeck_results().expr_ty(expr))
            || !in_view_position(cx, self, expr)
        {
            return;
        }
        let branches: Vec<Branch<'_>> = branches
            .into_iter()
            .map(|(cond, arm)| Branch {
                arm,
                cond: cond_text(cx, self, cond),
                get: contains_get(cx, cond),
            })
            .collect();
        // A verbatim condition that is not a live signal — a plain
        // `bool` — cannot drive `when` at all, and no `.get()` sits in it
        // to unwrap: the `if`/`else` is legitimate static configuration,
        // so the lint stays silent rather than warn with nothing to act
        // on. (A `.get()` cond maps to `None`, never this arm.)
        if branches
            .iter()
            .any(|b| b.cond.as_ref().is_some_and(|c| !c.signal))
        {
            return;
        }
        let snapshot = branches.iter().any(|branch| branch.get);
        span_lint_and_then(
            cx,
            IF_ELSE_VIEW,
            expr.span,
            if snapshot { SNAPSHOT_MSG } else { PLAIN_MSG },
            |diag| {
                if snapshot {
                    diag.note(SNAPSHOT_NOTE);
                }
                match self.fix(cx, expr, &branches, last_else) {
                    Some(parts) => {
                        diag.multipart_suggestion(SUGGESTION, parts, Applicability::MaybeIncorrect);
                    }
                    None if branches.iter().any(|b| b.get && b.cond.is_none()) => {
                        diag.help(DEEP_GET_HELP);
                    }
                    None => {
                        diag.help(GENERIC_HELP);
                    }
                }
            },
        );
    }
}
