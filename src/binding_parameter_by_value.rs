use clippy_utils::source::snippet_opt;
use rustc_data_structures::fx::FxHashMap;
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{
    BodyId, ByRef, CaptureBy, Expr, ExprKind, HirId, ImplItemImplKind, ImplItemKind, ItemKind,
    Node, PatKind, QPath, StructTailExpr, TraitItemKind,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter::OnlyBodies;
use rustc_middle::ty::adjustment::{Adjust, AutoBorrow};
use rustc_middle::ty::{self, TyKind, TypeckResults};
use rustc_session::impl_lint_pass;
use rustc_span::Span;

use crate::binding::BINDING;
use crate::carriers::{CLONE, is_call_to};
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_hir_and_then;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};
use crate::signature::{collectable, normalized_inputs, public_signature, signature_of};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a function or method whose signature takes `nami::Binding<T>`
    /// by value where `&Binding<T>` would do. "Public" is read
    /// syntactically — a written qualifier (`pub`, `pub(crate)`,
    /// `pub(super)`, `pub(in ..)`; `pub(self)` counts as private), a member
    /// of a trait carrying one, or an inherent method of a type carrying
    /// one — plus a private signature that already has a call site passing
    /// `f(b.clone())` (one is enough: the caller clones only because the
    /// signature takes the handle; a call through a function-item alias —
    /// `let f = hidden; f(b.clone())` — does not count, the callee path
    /// must resolve to the item). `extern`, `const`, and exported
    /// functions, `impl Trait for Type` members, and parameters whose
    /// pattern is not a plain `name`/`mut name`/`_` binding stay silent,
    /// as do wrapped forms like `&Binding<T>`, `Option<Binding<T>>`,
    /// `Vec<Binding<T>>`, and `State<Binding<T>>`.
    ///
    /// ### Why is this bad?
    ///
    /// A `Binding<T>` parameter by value makes every caller write
    /// `f(b.clone())` — or hand over a handle it still needs and cannot
    /// use afterwards without having cloned first. `Binding` is a cheap
    /// reference-counted handle; the function's contract is to read or
    /// write through it, not to own it — the shape the framework's own
    /// APIs have (`Picker::new(&selection)`, `Binding::mapping(&source)`).
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// pub fn counter(count: Binding<i32>) -> impl View { .. }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// pub fn counter(count: &Binding<i32>) -> impl View { .. }
    /// ```
    pub BINDING_PARAMETER_BY_VALUE,
    style,
    "a `Binding<T>` parameter taken by value instead of `&Binding<T>`"
}

const MSG: &str = "a public function takes `Binding<T>` by value; take `&Binding<T>`";
const SUGGESTION: &str = "the callee clones if it keeps the handle; callers pass `&b`";
const TRAIT_NOTE: &str = "every implementor of this trait must make the same change";

/// One `f(.., e, ..)` call-site rewrite at a flagged `Binding` position:
/// `arg` is the argument's span — the part's target — and `src` the span
/// whose snippet becomes the borrowed operand (the `.clone()` receiver for
/// a clone call, the argument itself otherwise, so `f(b.clone())` becomes
/// `f(&b)` and any other `e` becomes `f(&e)`). `borrow` is the `&`/`*`
/// prefix (`borrow_prefix`); `cloned` marks `f(b.clone())` sites — the
/// trigger for a private signature.
struct CallSite {
    arg: Span,
    src: Span,
    borrow: String,
    cloned: bool,
}

/// `sigs` records every local function with a bare `Binding<T>` parameter
/// (visit order, so `check_crate_post` reports deterministically);
/// `call_sites` maps a callee to the `CallSite`s for its `Binding`
/// positions — the call-site half of its fix, computed while the call
/// site's `TypeckResults` is still live.
#[derive(Default)]
pub(crate) struct BindingParameterByValue {
    sigs: Vec<LocalDefId>,
    call_sites: FxHashMap<LocalDefId, Vec<CallSite>>,
}

impl_lint_pass!(BindingParameterByValue => [BINDING_PARAMETER_BY_VALUE]);

/// The indices of `did`'s parameters typed `Binding<T>` — bare, so
/// `&Binding<T>` and wrappers like `Option`/`Vec`/`State` stay silent; type
/// aliases normalize away first.
fn binding_params(cx: &LateContext<'_>, did: DefId) -> Vec<u32> {
    normalized_inputs(cx, did)
        .into_iter()
        .filter(|&(_, ty)| is_binding(cx, ty))
        .map(|(index, _)| index)
        .collect()
}

impl<'tcx> LateLintPass<'tcx> for BindingParameterByValue {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if let ItemKind::Fn { sig, .. } = item.kind
            && collectable(
                cx,
                item.owner_id.def_id,
                cx.tcx.hir_attrs(item.hir_id()),
                sig.header,
                |cx, did| !binding_params(cx, did).is_empty(),
            )
        {
            self.sigs.push(item.owner_id.def_id);
        }
    }

    fn check_impl_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::ImplItem<'tcx>) {
        // `impl Trait for Type` signatures are fixed by the trait; calls to
        // them count toward the trait item instead.
        if let ImplItemKind::Fn(sig, _) = item.kind
            && matches!(item.impl_kind, ImplItemImplKind::Inherent { .. })
            && collectable(
                cx,
                item.owner_id.def_id,
                cx.tcx.hir_attrs(item.hir_id()),
                sig.header,
                |cx, did| !binding_params(cx, did).is_empty(),
            )
        {
            self.sigs.push(item.owner_id.def_id);
        }
    }

    fn check_trait_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::TraitItem<'tcx>) {
        if let TraitItemKind::Fn(sig, _) = item.kind
            && collectable(
                cx,
                item.owner_id.def_id,
                cx.tcx.hir_attrs(item.hir_id()),
                sig.header,
                |cx, did| !binding_params(cx, did).is_empty(),
            )
        {
            self.sigs.push(item.owner_id.def_id);
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        let typeck = cx.typeck_results();
        let Some(callee) = call_def_id(typeck, expr) else {
            return;
        };
        let norm = implemented_trait_item(cx.tcx, callee);
        let Some(local) = norm.as_local() else {
            return;
        };
        let params = binding_params(cx, norm);
        if params.is_empty() {
            return;
        }
        let args = call_args(expr);
        for index in params {
            let Some(arg) = args.get(index as usize) else {
                continue;
            };
            // A `.clone()` call borrows its receiver — `f(b.clone())` →
            // `f(&b)` — and is the trigger for a private signature; any
            // other argument is borrowed as written — `f(e)` → `f(&e)`.
            let site = if is_call_to(cx, typeck, arg, &[CLONE])
                && let Some(receiver) = call_args(arg).first()
            {
                CallSite {
                    arg: arg.span,
                    src: receiver.span,
                    borrow: borrow_prefix(cx, typeck, receiver),
                    cloned: true,
                }
            } else {
                CallSite {
                    arg: arg.span,
                    src: arg.span,
                    borrow: borrow_prefix(cx, typeck, arg),
                    cloned: false,
                }
            };
            self.call_sites.entry(local).or_default().push(site);
        }
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        for &did in &self.sigs {
            let sites = self.call_sites.get(&did);
            if public_signature(cx, did) || sites.is_some_and(|s| s.iter().any(|s| s.cloned)) {
                report(cx, did, sites);
            }
        }
    }
}

/// The `&`/`*` prefix turning `expr` — a `Binding` call argument or
/// `.clone()` receiver — into a `&Binding`: `&` for a `Binding` value,
/// `&*` for a `&Binding` one (`&` more `*`s the deeper the reference),
/// `&*` for anything else that derefs to `Binding` (`Box`, …).
fn borrow_prefix(cx: &LateContext<'_>, typeck: &TypeckResults<'_>, expr: &Expr<'_>) -> String {
    let mut ty = typeck.expr_ty(expr);
    let mut stars = String::new();
    while let TyKind::Ref(_, inner, _) = *ty.kind() {
        stars.push('*');
        ty = inner;
    }
    if is_binding(cx, ty) {
        format!("&{stars}")
    } else {
        "&*".to_owned()
    }
}

/// Whether `ty` is `Binding<T>` itself.
fn is_binding(cx: &LateContext<'_>, ty: ty::Ty<'_>) -> bool {
    matches!(*ty.kind(), TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), BINDING))
}

/// Whether the `hir_id` expression is borrowed (auto-referenced) rather than
/// moved — a `&self` receiver or `==` operand shows a `Borrow` adjustment.
fn borrowed(cx: &LateContext<'_>, typeck: &TypeckResults<'_>, hir_id: HirId) -> bool {
    typeck
        .expr_adjustments(cx.tcx.hir_expect_expr(hir_id))
        .iter()
        .any(|adj| matches!(adj.kind, Adjust::Borrow(AutoBorrow::Ref(_))))
}

/// Whether `e` — a use of a `Binding` parameter — sits in a position that
/// took the handle by value in the original body: a field initializer, a
/// call argument or by-value receiver, an array/tuple element, a `..base`,
/// an assignment right side, a `return`/`break`/`yield`, or a tail that
/// yields `Binding`. Borrowed positions (`&e`, `e.get()`-style `&self`
/// receivers, field reads, `let` initializers) keep working on `&Binding`
/// and stay untouched. `Block`/`if`/`match` tails propagate the parent's
/// context downward.
fn consumed<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    e: &'tcx Expr<'tcx>,
) -> bool {
    let mut cur = e.hir_id;
    loop {
        let parent_id = cx.tcx.parent_hir_id(cur);
        match cx.tcx.hir_node(parent_id) {
            // `S { f: e }` — the initializer stores the handle.
            Node::ExprField(field) => return field.expr.hir_id == cur,
            // `match .. { _ => e }` — adopt the match's own context.
            Node::Arm(arm) if arm.body.hir_id == cur => cur = parent_id,
            // `e` is a block's tail expression — adopt the block's own
            // context. `Block` is a HIR node: the tail's parent is the
            // block, whose parent is the `ExprKind::Block` expression.
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
                ExprKind::If(_, then, otherwise)
                    if then.hir_id == cur
                        || otherwise.is_some_and(|branch| branch.hir_id == cur) =>
                {
                    cur = parent_id;
                }
                ExprKind::Match(_, arms, _) if arms.iter().any(|arm| arm.hir_id == cur) => {
                    cur = parent_id;
                }
                ExprKind::Ret(_)
                | ExprKind::Break(..)
                | ExprKind::Yield(..)
                | ExprKind::Become(..) => return true,
                // `e` is the closure body's value — returned, so captured
                // by value.
                ExprKind::Closure(_) => return true,
                ExprKind::Call(_, args) => {
                    return args.iter().any(|arg| arg.hir_id == cur);
                }
                ExprKind::MethodCall(_, receiver, args, _) => {
                    return if receiver.hir_id == cur {
                        !borrowed(cx, typeck, cur)
                    } else {
                        args.iter().any(|arg| arg.hir_id == cur)
                    };
                }
                ExprKind::Assign(_, rhs, _) | ExprKind::AssignOp(_, _, rhs) => {
                    return rhs.hir_id == cur;
                }
                ExprKind::Struct(_, _, StructTailExpr::Base(base)) => return base.hir_id == cur,
                ExprKind::Array(elems) | ExprKind::Tup(elems) => {
                    return elems.iter().any(|elem| elem.hir_id == cur);
                }
                ExprKind::Repeat(elem, _) => return elem.hir_id == cur,
                // `v[e]` — the index is taken by value; `e[i]` auto-borrows
                // the base like a `&self` receiver.
                ExprKind::Index(_, index, _) if index.hir_id == cur => return true,
                ExprKind::Index(..) => return !borrowed(cx, typeck, cur),
                ExprKind::Unary(..) | ExprKind::Binary(..) => return !borrowed(cx, typeck, cur),
                _ => return false,
            },
            // `cur` is the body's tail — the function returns it, so it is
            // consumed when the return type is `Binding`.
            Node::Item(_) | Node::ImplItem(_) | Node::TraitItem(_) | Node::ForeignItem(_) => {
                return matches!(cx.tcx.hir_node(cur), Node::Expr(tail) if is_binding(cx, typeck.expr_ty(tail)));
            }
            _ => return false,
        }
    }
}

/// The `use` sites of the flagged parameters inside one body that need
/// `.clone()`: every use that consumed the handle, and every use inside a
/// `move`/`use` closure — the capture itself is a move. `typeck` follows
/// the body being visited so nested closures resolve against their own
/// `TypeckResults`.
struct ParamUses<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    /// Pattern `HirId`s of the flagged parameters.
    params: &'a [HirId],
    typeck: &'tcx TypeckResults<'tcx>,
    /// Whether the innermost body captures by value (`move`/`use`
    /// closure, `async move {}`); the outermost function body starts
    /// unstacked, so `.last()` misses mean `false`.
    owned_capture: Vec<bool>,
    /// `(span, replacement)` for each use that becomes `x.clone()`.
    edits: Vec<(Span, String)>,
}

impl<'tcx> ParamUses<'_, 'tcx> {
    /// The `.clone()` edit for a use of a flagged parameter — `None` when
    /// the use only borrows. A shorthand field initializer is rewritten
    /// `f: f.clone()`, not `f.clone()` which would not parse.
    fn edit(&self, e: &'tcx Expr<'tcx>) -> Option<String> {
        if !self.owned_capture.last().copied().unwrap_or(false)
            && !consumed(self.cx, self.typeck, e)
        {
            return None;
        }
        let text = snippet_opt(self.cx, e.span)?;
        let field = match self.cx.tcx.hir_node(self.cx.tcx.parent_hir_id(e.hir_id)) {
            Node::ExprField(field) if field.is_shorthand => Some(field.ident.name),
            _ => None,
        };
        Some(match field {
            Some(name) => format!("{name}: {text}.clone()"),
            None => format!("{text}.clone()"),
        })
    }
}

impl<'tcx> Visitor<'tcx> for ParamUses<'_, 'tcx> {
    type NestedFilter = OnlyBodies;

    fn maybe_tcx(&mut self) -> rustc_middle::ty::TyCtxt<'tcx> {
        self.cx.tcx
    }

    fn visit_nested_body(&mut self, id: BodyId) {
        let owned = matches!(
            self.cx
                .tcx
                .hir_node_by_def_id(self.cx.tcx.hir_body_owner_def_id(id)),
            Node::Expr(expr)
                if matches!(expr.kind, ExprKind::Closure(closure)
                    if matches!(closure.capture_clause, CaptureBy::Value { .. } | CaptureBy::Use { .. }))
        );
        let typeck = std::mem::replace(&mut self.typeck, self.cx.tcx.typeck_body(id));
        self.owned_capture.push(owned);
        self.visit_body(self.cx.tcx.hir_body(id));
        self.owned_capture.pop();
        self.typeck = typeck;
    }

    fn visit_expr(&mut self, e: &'tcx Expr<'tcx>) {
        if let ExprKind::Path(QPath::Resolved(None, path)) = e.kind
            && let Res::Local(local) = path.res
            && self.params.contains(&local)
            && !e.span.from_expansion()
            && let Some(edit) = self.edit(e)
        {
            self.edits.push((e.span, edit));
        }
        intravisit::walk_expr(self, e);
    }
}

fn report(cx: &LateContext<'_>, did: LocalDefId, call_sites: Option<&Vec<CallSite>>) {
    let hir_id = cx.tcx.local_def_id_to_hir_id(did);
    let Some((decl, body, trait_member)) = signature_of(cx.tcx, did) else {
        return;
    };
    let params: Vec<(u32, &rustc_hir::Ty<'_>)> = binding_params(cx, did.to_def_id())
        .into_iter()
        .filter_map(|index| {
            let written = decl.inputs.get(index as usize)?;
            (!written.span.from_expansion()).then_some((index, written))
        })
        .collect();
    if params.is_empty() {
        return;
    }
    // Every rewritten parameter must be a plain `name`/`mut name`/`_`
    // pattern — a destructuring, `ref`, or `@` pattern has no binding the
    // `&Binding` parameter can feed, so the item stays silent rather than
    // emit a fix that breaks the body.
    let mut locals: Vec<HirId> = Vec::with_capacity(params.len());
    if let Some(body) = body {
        for &(index, _) in &params {
            let Some(param) = body.params.get(index as usize) else {
                return;
            };
            match param.pat.kind {
                PatKind::Binding(mode, local, _, None) if mode.0 == ByRef::No => {
                    locals.push(local);
                }
                PatKind::Wild => {}
                _ => return,
            }
        }
    }
    let mut parts: Vec<(Span, String)> = params
        .iter()
        .map(|&(_, written)| (written.span.shrink_to_lo(), "&".to_owned()))
        .collect();
    if let Some(body) = body
        && !locals.is_empty()
    {
        let mut uses = ParamUses {
            cx,
            params: &locals,
            typeck: cx.tcx.typeck_body(body.id()),
            owned_capture: Vec::new(),
            edits: Vec::new(),
        };
        uses.visit_body(body);
        parts.extend(uses.edits);
    }
    for site in call_sites.into_iter().flatten() {
        if let Some(text) = snippet_opt(cx, site.src) {
            parts.push((site.arg, format!("{}{text}", site.borrow)));
        }
    }
    span_lint_hir_and_then(
        cx,
        BINDING_PARAMETER_BY_VALUE,
        hir_id,
        params[0].1.span,
        MSG,
        |diag| {
            diag.multipart_suggestion(SUGGESTION, parts, Applicability::MaybeIncorrect);
            if trait_member {
                diag.note(TRAIT_NOTE);
            }
        },
    );
}
