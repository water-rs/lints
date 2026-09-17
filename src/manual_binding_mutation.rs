//! `manual_binding_mutation` — a `Binding` mutation or `set` conversion
//! written by hand where `Binding` has the named method.

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::{MaybeDef, MaybeQPath, MaybeResPath};
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::is_local_used;
use rustc_errors::Applicability;
use rustc_hir::{BinOpKind, ByRef, Expr, ExprKind, HirId, LangItem, Node, PatKind, StmtKind, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyKind, TypeVisitableExt, TypeckResults};
use rustc_session::declare_lint_pass;
use rustc_span::{Span, SyntaxContext, sym};

use crate::anyview::peel;
use crate::binding::{
    BINDING_GET_MUT, BINDING_SET, BINDING_WITH_MUT, binding_value_ty, coerced_operand,
    extend_accepts, named_op_assign, op_assign_method,
};
use crate::carriers::{CLONE, FROM, INTO, TO_OWNED, TO_STRING, strip_wraps};
use crate::def_path::def_path_eq;
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};
use crate::snapshot_get::reads_binding;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `Binding` mutation or conversion written by hand where nami's
    /// `Binding` has the named method:
    ///
    /// - `b.with_mut(|v| *v op= x)`, `b.with_mut(|v| *v = *v op x)`,
    ///   `b.with_mut(|v| *v = v.clone() op x)`, and `*b.get_mut() op= x` for
    ///   `b.<op>_assign(x)` — or `b.append(x)` for `+` when `T: Extend<X>`
    ///   and `<op>_assign` does not apply
    /// - `b.with_mut(|v| v.push(x))` / `v.push_str(x)` /
    ///   `v.extend(core::iter::once(x))` / `v.extend([x])` / `v.extend(Some(x))`
    ///   for `b.append(x)` when `T: Extend<X>`
    /// - `b.with_mut(|v| *v = !*v)` on `Binding<bool>` for `b.toggle()`
    /// - `b.with_mut(|v| *v = x)` and `*b.get_mut() = x` for `b.set(x)`
    /// - `b.set(x.into())` / `b.set(Into::into(x))` / `b.set(T::from(x))` /
    ///   `b.set(From::from(x))` / `b.set(Str::from_static(x))` /
    ///   `b.set(String::from(x))` / `b.set(x.to_string())` /
    ///   `b.set(x.to_owned())` for `b.set_from(x)` — or `b.set(x)` when `x`
    ///   is already `T`
    ///
    /// The `with_mut` closure must be a single expression (or a block with
    /// that one statement), `x` must not mention the closure parameter, and
    /// the call's result must be unused. An argument that reads the same
    /// binding back through `get()`/`get_mut()` is `set_with_own_get`'s case
    /// and stays silent here.
    ///
    /// ### Why is this bad?
    ///
    /// The named method is one expression, is greppable, and — for
    /// `set_from` — keeps the call site free of a conversion that names the
    /// target type.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// count.with_mut(|v| *v += 1);
    /// title.set(Str::from_static("Untitled"));
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// count.add_assign(1);
    /// title.set_from("Untitled");
    /// ```
    pub MANUAL_BINDING_MUTATION,
    style,
    "a `Binding` mutation written by hand that restates a named `Binding` method"
}

declare_lint_pass!(ManualBindingMutation => [MANUAL_BINDING_MUTATION]);

const SUGGESTION_LABEL: &str = "use the named method";

/// `Str::from_static` — the verbatim `Str` conversion `set` callers spell
/// where `set_from` takes the `&'static str` directly.
const STR_FROM_STATIC: &[&str] = &["suiteki", "Str", "from_static"];

/// `Vec::push` — `v.push(x)` restates `extend(once(x))`.
const VEC_PUSH: &[&str] = &["alloc", "vec", "Vec", "push"];

/// `String::push` — `v.push(c)` appends a `char`.
const STRING_PUSH: &[&str] = &["alloc", "string", "String", "push"];

/// `String::push_str` — `v.push_str(x)` appends a `&str` one element at a
/// time, which `Extend` already covers.
const STRING_PUSH_STR: &[&str] = &["alloc", "string", "String", "push_str"];

/// Conversions a `b.set(<conv>(x))` rewrites through `set_from`:
/// `x.into()`/`Into::into(x)`, `T::from(x)`/`From::from(x)` (which
/// `String::from` resolves to), `Str::from_static(x)`, `x.to_string()`,
/// `x.to_owned()`.
const SET_CONVERSIONS: &[&[&str]] = &[INTO, FROM, STR_FROM_STATIC, TO_STRING, TO_OWNED];

/// `expr` reads the `with_mut` closure parameter `param` — the `&mut T`
/// slot — either by deref (`*v`, `&*v`) or by cloning it (`v.clone()`,
/// `(*v).clone()`). Those are the reads the named `Binding` methods perform
/// themselves.
fn is_param_read<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    param: HirId,
) -> bool {
    let expr = peel(expr);
    match expr.kind {
        ExprKind::Unary(UnOp::Deref, inner) => peel(inner).res_local_id() == Some(param),
        ExprKind::MethodCall(..) | ExprKind::Call(..)
            if call_def_id(typeck, expr)
                .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), CLONE)) =>
        {
            call_args(expr).first().is_some_and(|&recv| {
                peel(recv).res_local_id() == Some(param) || is_param_read(cx, typeck, recv, param)
            })
        }
        _ => false,
    }
}

/// `expr` is the assignment place `*v` — a deref of the `with_mut` closure
/// parameter.
fn is_deref_of_param(expr: &Expr<'_>, param: HirId) -> bool {
    matches!(
        peel(expr).kind,
        ExprKind::Unary(UnOp::Deref, inner) if peel(inner).res_local_id() == Some(param)
    )
}

/// The single expression a `with_mut` closure evaluates: `body` itself, a
/// `{ expr }` tail-only block, or a `{ expr; }` one-statement block. A
/// closure with more statements — or a tail on top of a statement — is
/// `None`.
fn single_mutation<'hir>(body: &'hir Expr<'hir>) -> Option<&'hir Expr<'hir>> {
    let mut expr = body;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    let ExprKind::Block(block, _) = expr.kind else {
        return Some(expr);
    };
    let mut mutation = match (block.stmts, block.expr) {
        ([], Some(tail)) => tail,
        ([stmt], None) => match stmt.kind {
            StmtKind::Expr(e) | StmtKind::Semi(e) => e,
            _ => return None,
        },
        _ => return None,
    };
    while let ExprKind::DropTemps(inner) = mutation.kind {
        mutation = inner;
    }
    Some(mutation)
}

/// Whether `expr`'s value is dropped where it stands: a `;` statement, or
/// the tail of a block/`if`/`match`/`loop`/`while` whose own value is
/// dropped — climbing out to a body root counts, since every mutation shape
/// here produces `()`. `let r = b.with_mut(..)` consumes the `with_mut`
/// return — the closure's `R` — and stays silent.
fn value_dropped(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let mut id = expr.hir_id;
    loop {
        match cx.tcx.parent_hir_node(id) {
            Node::Stmt(stmt) => match stmt.kind {
                StmtKind::Semi(_) | StmtKind::Expr(_) => return true,
                _ => return false,
            },
            Node::LetStmt(..) => return false,
            Node::Arm(arm) => id = arm.hir_id,
            Node::Block(block) => {
                // A block child outside `block.expr` is a statement — but
                // statements' parents are `Stmt` nodes, so landing here off
                // the tail means the value is discarded.
                if !block.expr.is_some_and(|e| e.hir_id == id) {
                    return true;
                }
                id = block.hir_id;
            }
            Node::Expr(e) => match e.kind {
                ExprKind::Block(..) | ExprKind::DropTemps(..) | ExprKind::Loop(..) => {
                    id = e.hir_id;
                }
                ExprKind::If(cond, ..) if cond.hir_id != id => id = e.hir_id,
                ExprKind::Match(scrutinee, ..) if scrutinee.hir_id != id => id = e.hir_id,
                _ => return false,
            },
            // Body boundary — the call is a fn/const body's value; every
            // shape here produces `()`, so this is the `()`-returning tail.
            Node::Item(..) | Node::TraitItem(..) | Node::ImplItem(..) => return true,
            _ => return false,
        }
    }
}

/// The named `Binding` method for `op` over `rhs_ty`, or `append` for `+`
/// when `T: Extend<rhs_ty>` — `None` when neither applies.
fn op_method<'tcx>(
    cx: &LateContext<'tcx>,
    value_ty: Ty<'tcx>,
    op: BinOpKind,
    rhs_ty: Ty<'tcx>,
) -> Option<&'static str> {
    named_op_assign(cx, value_ty, op, rhs_ty).or_else(|| {
        (op == BinOpKind::Add && extend_accepts(cx, value_ty, rhs_ty)).then_some("append")
    })
}

/// `lhs` as the mutation place `*b.get_mut()` — `(b, T)` of `Binding<T>`,
/// with carriers stripped off `b`.
fn get_mut_target<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    lhs: &'hir Expr<'hir>,
) -> Option<(&'hir Expr<'hir>, Ty<'hir>)> {
    let ExprKind::Unary(UnOp::Deref, call) = peel(lhs).kind else {
        return None;
    };
    if call.span.from_expansion() {
        return None;
    }
    let did = call_def_id(typeck, call)?;
    if !def_path_eq(cx, implemented_trait_item(cx.tcx, did), BINDING_GET_MUT) {
        return None;
    }
    let binding = strip_wraps(cx, typeck, call_args(call).first()?);
    Some((binding, binding_value_ty(cx, typeck, binding)?))
}

/// The single element `v.extend(..)` appends — `core::iter::once(x)`,
/// `[x]`, or `Some(x)` — or `None` when the argument iterates more (or
/// fewer) than one.
fn extend_element<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    arg: &'hir Expr<'hir>,
) -> Option<&'hir Expr<'hir>> {
    let arg = peel(arg);
    if let ExprKind::Array([x]) = arg.kind {
        return Some(x);
    }
    let ExprKind::Call(func, [x]) = arg.kind else {
        return None;
    };
    if func
        .res(typeck)
        .ctor_parent(cx)
        .is_lang_item(cx, LangItem::OptionSome)
    {
        return Some(x);
    }
    let did = call_def_id(typeck, arg)?;
    lookup_path_str(cx.tcx, PathNS::Value, "core::iter::once")
        .contains(&did)
        .then_some(x)
}

/// `v.push(x)` / `v.push_str(x)` / `v.extend(<one-element>)` on the
/// `with_mut` parameter → the element `b.append` takes. `None` for any
/// other method on `v`.
fn append_element<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    call: &'hir Expr<'hir>,
    param: HirId,
) -> Option<&'hir Expr<'hir>> {
    let ExprKind::MethodCall(_, receiver, [arg], _) = call.kind else {
        return None;
    };
    if peel(receiver).res_local_id() != Some(param) && !is_param_read(cx, typeck, receiver, param) {
        return None;
    }
    let did = implemented_trait_item(cx.tcx, typeck.type_dependent_def_id(call.hir_id)?);
    if def_path_eq(cx, did, VEC_PUSH)
        || def_path_eq(cx, did, STRING_PUSH)
        || def_path_eq(cx, did, STRING_PUSH_STR)
    {
        return Some(arg);
    }
    // `Extend::extend` — resolved to the trait item whether the impl is a
    // collection's or a custom type's.
    let is_extend =
        lookup_path_str(cx.tcx, PathNS::Type, "core::iter::Extend").contains(&cx.tcx.parent(did));
    is_extend.then(|| extend_element(cx, typeck, arg)).flatten()
}

/// The `Binding` method `mutation` restates and the argument expressions
/// the suggestion carries — `None` when the `with_mut` body is not a named
/// shape, `x` mentions `param`, `x` reads the binding back, or the method's
/// bounds do not hold.
fn mutation_method<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    binding: &'tcx Expr<'tcx>,
    value_ty: Ty<'tcx>,
    mutation: &'tcx Expr<'tcx>,
    param: HirId,
    ctxt: SyntaxContext,
) -> Option<(&'static str, Vec<&'tcx Expr<'tcx>>)> {
    match mutation.kind {
        // `*v op= x` → `b.<op>_assign(x)` or `b.append(x)` for `+`.
        ExprKind::AssignOp(op, lhs, rhs)
            if is_deref_of_param(lhs, param)
                && !is_local_used(cx, rhs, param)
                && !reads_binding(cx, ctxt, binding, rhs) =>
        {
            op_method(cx, value_ty, op.node.into(), typeck.expr_ty_adjusted(rhs))
                .map(|method| (method, vec![rhs]))
        }
        ExprKind::Assign(lhs, rhs, _) if is_deref_of_param(lhs, param) => {
            if !is_local_used(cx, rhs, param) {
                // `*v = x` → `b.set(x)` — unless `x` reads the binding back,
                // which is `set_with_own_get`'s case.
                return (!reads_binding(cx, ctxt, binding, rhs)).then_some(("set", vec![rhs]));
            }
            match rhs.kind {
                // `*v = !*v` → `b.toggle()`.
                ExprKind::Unary(UnOp::Not, inner)
                    if value_ty.is_bool() && is_param_read(cx, typeck, inner, param) =>
                {
                    Some(("toggle", Vec::new()))
                }
                // `*v = *v op x` / `*v = v.clone() op x` → `<op>_assign`.
                ExprKind::Binary(op, l, x)
                    if is_param_read(cx, typeck, l, param)
                        && op_assign_method(op.node).is_some()
                        && !is_local_used(cx, x, param)
                        && !reads_binding(cx, ctxt, binding, x) =>
                {
                    op_method(cx, value_ty, op.node, typeck.expr_ty_adjusted(x))
                        .map(|method| (method, vec![x]))
                }
                _ => None,
            }
        }
        ExprKind::MethodCall(..) => {
            let ele = append_element(cx, typeck, mutation, param)?;
            if is_local_used(cx, ele, param) || reads_binding(cx, ctxt, binding, ele) {
                return None;
            }
            extend_accepts(cx, value_ty, typeck.expr_ty_adjusted(ele))
                .then(|| ("append", vec![ele]))
        }
        _ => None,
    }
}

/// `b.<method>(<args..>)` as source text — `None` when a snippet cannot be
/// read. Each argument is rendered by [`coerced_operand`]: the named methods
/// take generic arguments, so a coercion the original call site applied —
/// `&String` reaching `&str` — has to be written out (`&*s`) for the
/// emitted code to have the type the bound checks proved.
fn method_call_sugg<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    binding: &Expr<'hir>,
    method: &str,
    args: &[&'hir Expr<'hir>],
) -> Option<String> {
    let binding = snippet_opt(cx, binding.span)?;
    let args = args
        .iter()
        .map(|arg| coerced_operand(typeck, arg, &snippet_opt(cx, arg.span)?))
        .collect::<Option<Vec<_>>>()?
        .join(", ");
    Some(format!("{binding}.{method}({args})"))
}

/// `this mutation restates `Binding::<method>`` on `span`, with the
/// machine-applicable rewrite when its source could be read.
fn suggest(cx: &LateContext<'_>, span: Span, method: &str, sugg: Option<String>) {
    span_lint_and_then(
        cx,
        MANUAL_BINDING_MUTATION,
        span,
        format!("this mutation restates `Binding::{method}`"),
        |diag| match sugg {
            Some(code) => {
                diag.span_suggestion(
                    span,
                    SUGGESTION_LABEL,
                    code,
                    Applicability::MachineApplicable,
                );
            }
            None => {
                diag.help(format!("use `Binding::{method}`"));
            }
        },
    );
}

/// `b.with_mut(|v| ..)` — the closure's one mutation restates a named
/// `Binding` method.
fn check_with_mut<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    expr: &'tcx Expr<'tcx>,
) {
    let args = call_args(expr);
    let &[receiver, func] = args.as_slice() else {
        return;
    };
    let Some(value_ty) = binding_value_ty(cx, typeck, receiver) else {
        return;
    };
    if func.span.from_expansion() {
        return;
    }
    let ExprKind::Closure(closure) = peel(func).kind else {
        return;
    };
    let body = cx.tcx.hir_body(closure.body);
    let [param] = body.params else {
        return;
    };
    // `|ref v|` binds `v: &&mut T` — `*v` is `&mut T`, not the value; a
    // by-value (or `mut`) binding is the `&mut T` the shapes read.
    let PatKind::Binding(mode, param, _, None) = param.pat.kind else {
        return;
    };
    if !matches!(mode.0, ByRef::No) {
        return;
    }
    let Some(mutation) = single_mutation(body.value) else {
        return;
    };
    if !value_dropped(cx, expr) {
        return;
    }
    let binding = strip_wraps(cx, typeck, receiver);
    let ctxt = expr.span.ctxt();
    // The closure's expressions are typed by the closure's own body.
    let typeck = cx.tcx.typeck_body(closure.body);
    let Some((method, args)) =
        mutation_method(cx, typeck, binding, value_ty, mutation, param, ctxt)
    else {
        return;
    };
    suggest(
        cx,
        expr.span,
        method,
        method_call_sugg(cx, typeck, binding, method, &args),
    );
}

/// `b.set(<conversion>(x))` → `b.set_from(x)` — or `b.set(x)` when `x` is
/// already `T` and the conversion was redundant.
fn check_set_conversion<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    expr: &'tcx Expr<'tcx>,
) {
    let args = call_args(expr);
    let &[receiver, arg] = args.as_slice() else {
        return;
    };
    let Some(value_ty) = binding_value_ty(cx, typeck, receiver) else {
        return;
    };
    let arg = peel(arg);
    if arg.span.from_expansion() {
        return;
    }
    let Some(did) = call_def_id(typeck, arg) else {
        return;
    };
    let did = implemented_trait_item(cx.tcx, did);
    if !SET_CONVERSIONS
        .iter()
        .any(|path| def_path_eq(cx, did, path))
    {
        return;
    }
    let x = match arg.kind {
        ExprKind::MethodCall(_, receiver, [], _) => receiver,
        ExprKind::Call(_, [x]) => x,
        _ => return,
    };
    // `to_owned`/`to_string` borrow `x`; the rewrite passes `x` itself —
    // for an owned `x` that is a move the original never performed (`s`
    // would be consumed where `s.to_owned()` kept it alive). Only a
    // reference receiver keeps the rewrite a borrow.
    if (def_path_eq(cx, did, TO_STRING) || def_path_eq(cx, did, TO_OWNED))
        && !matches!(typeck.expr_ty(x).kind(), TyKind::Ref(..))
    {
        return;
    }
    let binding = strip_wraps(cx, typeck, receiver);
    if reads_binding(cx, expr.span.ctxt(), binding, x) {
        return;
    }
    let x_ty = typeck.expr_ty_adjusted(x);
    if x_ty.has_infer() || value_ty.has_infer() {
        return;
    }
    let method = if x_ty == value_ty {
        "set"
    } else {
        let Some(into) = cx.tcx.get_diagnostic_item(sym::Into) else {
            return;
        };
        if !implements_trait(cx, x_ty, into, &[value_ty.into()]) {
            return;
        }
        "set_from"
    };
    suggest(
        cx,
        expr.span,
        method,
        method_call_sugg(cx, typeck, binding, method, &[x]),
    );
}

impl<'tcx> LateLintPass<'tcx> for ManualBindingMutation {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        // `expr` may live in a nested body — resolve it against the typeck
        // of the body that owns it.
        let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(expr.hir_id));
        match expr.kind {
            // `*b.get_mut() = x` → `b.set(x)`; a right-hand side that reads
            // `b` back belongs to `set_with_own_get`.
            ExprKind::Assign(lhs, rhs, _) => {
                let Some((binding, _)) = get_mut_target(cx, typeck, lhs) else {
                    return;
                };
                // The guard's `Drop` publishes at the end of the enclosing
                // statement — `foo(*b.get_mut() = 9, b.get())` would observe
                // `9` early after `b.set(9)`. Only a dropped-value position
                // keeps the timing.
                if reads_binding(cx, expr.span.ctxt(), binding, rhs) || !value_dropped(cx, expr) {
                    return;
                }
                suggest(
                    cx,
                    expr.span,
                    "set",
                    method_call_sugg(cx, typeck, binding, "set", &[rhs]),
                );
            }
            // `*b.get_mut() op= x` → `b.<op>_assign(x)`/`b.append(x)`.
            ExprKind::AssignOp(op, lhs, rhs) => {
                let Some((binding, value_ty)) = get_mut_target(cx, typeck, lhs) else {
                    return;
                };
                if reads_binding(cx, expr.span.ctxt(), binding, rhs) || !value_dropped(cx, expr) {
                    return;
                }
                let Some(method) =
                    op_method(cx, value_ty, op.node.into(), typeck.expr_ty_adjusted(rhs))
                else {
                    return;
                };
                suggest(
                    cx,
                    expr.span,
                    method,
                    method_call_sugg(cx, typeck, binding, method, &[rhs]),
                );
            }
            ExprKind::MethodCall(..) | ExprKind::Call(..) => {
                let Some(did) = call_def_id(typeck, expr) else {
                    return;
                };
                let did = implemented_trait_item(cx.tcx, did);
                if def_path_eq(cx, did, BINDING_WITH_MUT) {
                    check_with_mut(cx, typeck, expr);
                } else if def_path_eq(cx, did, BINDING_SET) {
                    check_set_conversion(cx, typeck, expr);
                }
            }
            _ => {}
        }
    }
}
