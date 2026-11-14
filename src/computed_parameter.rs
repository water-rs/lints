use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::source::{snippet_indent, snippet_opt};
use rustc_data_structures::fx::FxHashMap;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::{
    Body, ByRef, Expr, ExprKind, HirId, ImplItemImplKind, ImplItemKind, ItemKind, Mutability,
    PatKind, QPath, TraitItemKind, TyKind, UnOp,
};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::{self, Ty, TypeckResults};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::Symbol;
use rustc_span::{BytePos, Span};

use crate::binding::{COMPUTED, binding_krate};
use crate::carriers::{CLONE, is_call_to};
use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_hir_and_then;
use crate::imports::{Bare, bare_status, use_insertion};
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};
use crate::signature::{collectable, normalized_inputs, public_signature, signature_of};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a function or method whose signature takes `Computed<T>` or
    /// `&Computed<T>` where `impl IntoComputed<T>` would do. "Public" is
    /// read syntactically — a written qualifier (`pub`, `pub(crate)`,
    /// `pub(super)`, `pub(in ..)`; `pub(self)` counts as private), a member
    /// of a trait carrying one, or an inherent method of a type carrying
    /// one — plus a private signature whose callers already erase a signal
    /// into `Computed` at more than one call site. `const fn` (the fix's
    /// `.into_computed()` call is not const), `extern` and exported
    /// functions, `impl Trait for Type` members, and parameters whose
    /// pattern is not a plain `name`/`mut name` binding stay silent.
    ///
    /// ### Why is this bad?
    ///
    /// A `Computed<T>` parameter forces every caller to erase first —
    /// `f(count.map(|v| v * 2).computed())`, `f(Computed::constant(3))` —
    /// while `impl IntoComputed<T>` accepts any signal and a plain `T`
    /// alike and erases once, inside the function. That is the shape every
    /// WaterUI component constructor and modifier already has.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// pub fn gauge(value: Computed<f32>) -> impl View { .. }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// pub fn gauge(value: impl IntoComputed<f32>) -> impl View {
    ///     let value = value.into_computed();
    ///     ..
    /// }
    /// ```
    pub COMPUTED_PARAMETER,
    style,
    "a `Computed<T>` parameter where callers could pass any signal"
}

/// `IntoComputed`'s `use` path, prefixed with the nameable crate root
/// (`waterui` or `nami`): `waterui::signal` and `nami::signal` name the same
/// re-exported `nami::signal` module.
const INTO_COMPUTED_SUFFIX: &str = "signal::IntoComputed";

const PUBLIC_MSG: &str =
    "a public function takes `Computed<T>` where callers would pass any signal";
const SUGGESTION: &str = "take `impl IntoComputed<T>` and erase inside with `.into_computed()`";
const TRAIT_NOTE: &str = "every implementor of this trait must make the same change";
const MACRO_NOTE: &str =
    "the body is macro-generated — make the same change in the macro definition";

/// `sigs` records every local function with a `Computed`/`&Computed`
/// parameter (visit order, so `check_crate_post` reports deterministically);
/// `erasing` counts call sites whose argument at such a parameter's
/// position is an expression rather than a place.
#[derive(Default)]
pub(crate) struct ComputedParameter {
    sigs: Vec<LocalDefId>,
    erasing: FxHashMap<LocalDefId, u32>,
}

impl_lint_pass!(ComputedParameter => [COMPUTED_PARAMETER]);

/// `(index, T)` for each of `did`'s parameters typed `Computed<T>` or
/// `&Computed<T>` — type aliases normalized away first.
fn computed_params<'tcx>(cx: &LateContext<'tcx>, did: DefId) -> Vec<(u32, Ty<'tcx>)> {
    normalized_inputs(cx, did)
        .into_iter()
        .filter_map(|(index, ty)| {
            let inner = match *ty.kind() {
                ty::TyKind::Ref(_, inner, _) => inner,
                _ => ty,
            };
            let ty::TyKind::Adt(adt, args) = *inner.kind() else {
                return None;
            };
            def_path_eq(cx, adt.did(), COMPUTED).then(|| (index, args.type_at(0)))
        })
        .collect()
}

/// Whether the expression passed for a `Computed` parameter is a plain
/// place — a bare local or field chain, its `.clone()`, or a borrow/deref
/// of one — rather than an erasing expression like `Computed::constant(..)`,
/// `signal.map(..).computed()`, or `x.into()`.
fn is_place<'hir>(cx: &LateContext<'hir>, typeck: &TypeckResults<'hir>, expr: &Expr<'hir>) -> bool {
    let mut expr = expr;
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner)
            | ExprKind::AddrOf(.., inner)
            | ExprKind::Unary(UnOp::Deref, inner) => inner,
            _ => break,
        };
    }
    match expr.kind {
        ExprKind::Path(QPath::Resolved(None, path)) => matches!(
            path.res,
            Res::Local(_) | Res::Def(DefKind::Const { .. } | DefKind::Static { .. }, _)
        ),
        ExprKind::Field(base, _) | ExprKind::Index(base, ..) => is_place(cx, typeck, base),
        ExprKind::Call(..) | ExprKind::MethodCall(..) if is_call_to(cx, typeck, expr, &[CLONE]) => {
            call_args(expr)
                .first()
                .is_some_and(|receiver| is_place(cx, typeck, receiver))
        }
        _ => false,
    }
}

impl<'tcx> LateLintPass<'tcx> for ComputedParameter {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if let ItemKind::Fn { sig, .. } = item.kind
            && collectable(
                cx,
                item.owner_id.def_id,
                cx.tcx.hir_attrs(item.hir_id()),
                sig.header,
                |cx, did| !computed_params(cx, did).is_empty(),
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
                |cx, did| !computed_params(cx, did).is_empty(),
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
                |cx, did| !computed_params(cx, did).is_empty(),
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
        let params = computed_params(cx, norm);
        if params.is_empty() {
            return;
        }
        let args = call_args(expr);
        if params.iter().any(|&(index, _)| {
            args.get(index as usize)
                .is_some_and(|arg| !is_place(cx, typeck, arg))
        }) {
            *self.erasing.entry(local).or_default() += 1;
        }
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        for &did in &self.sigs {
            let erasing = self.erasing.get(&did).copied().unwrap_or(0);
            if public_signature(cx, did) {
                report(cx, did, PUBLIC_MSG.to_owned());
            } else if erasing >= 2 {
                report(
                    cx,
                    did,
                    format!(
                        "{erasing} call sites erase a signal into `Computed<T>` for this function"
                    ),
                );
            }
        }
    }
}

/// The `T` to spell inside `IntoComputed<T>`: the written `Computed<T>`/`&Computed<T>`
/// argument when the signature names `Computed` directly, else the resolved
/// type's display form (a renamed import or an alias).
fn inner_text(cx: &LateContext<'_>, written: &rustc_hir::Ty<'_>, inner: Ty<'_>) -> String {
    let ty = match written.kind {
        TyKind::Ref(_, mut_ty) => mut_ty.ty,
        _ => written,
    };
    if let TyKind::Path(QPath::Resolved(None, path)) = ty.kind
        && matches!(path.res, Res::Def(_, did) if def_path_eq(cx, did, COMPUTED))
        && let Some(arg) = path
            .segments
            .last()
            .and_then(|segment| segment.args)
            .and_then(|args| {
                args.args.iter().find_map(|arg| match arg {
                    rustc_hir::GenericArg::Type(ty) => Some(ty),
                    _ => None,
                })
            })
        && let Some(text) = snippet_opt(cx, arg.span)
    {
        return text;
    }
    format!("{inner}")
}

/// The `impl IntoComputed` signature spelling — bare `IntoComputed` when
/// the name already resolves to the trait or a `use` can be placed (the
/// `use` part is pushed onto `parts`); crate-qualified when the bare name
/// is taken, ambiguous, or unplaceable. A qualified spelling needs no
/// import for `.into_computed()`: an `impl Trait` parameter's bounds put
/// the trait's methods in scope for that parameter, so the inserted
/// `let x = x.into_computed();` always resolves — and a
/// `use <path> as _;` would be dead code rustc flags `unused_imports`.
/// `None` when neither `waterui` nor `nami` is nameable.
fn into_computed_spelling(
    cx: &LateContext<'_>,
    hir_id: HirId,
    at: BytePos,
    parts: &mut Vec<(Span, String)>,
) -> Option<String> {
    let target = lookup_path_str(
        cx.tcx,
        PathNS::Type,
        &format!("nami::{INTO_COMPUTED_SUFFIX}"),
    )
    .first()
    .copied();
    let status = bare_status(
        cx,
        hir_id,
        at,
        Symbol::intern("IntoComputed"),
        Namespace::TypeNS,
        target,
    );
    match status {
        Bare::Same => Some("IntoComputed".to_owned()),
        Bare::Free => {
            let use_path = format!("{}::{INTO_COMPUTED_SUFFIX}", binding_krate(cx)?);
            match use_insertion(cx, hir_id, &use_path) {
                Some((point, before, after)) => {
                    parts.push((point, format!("{before}use {use_path};{after}")));
                    Some("IntoComputed".to_owned())
                }
                None => Some(use_path),
            }
        }
        Bare::Conflict | Bare::Unknown => {
            Some(format!("{}::{INTO_COMPUTED_SUFFIX}", binding_krate(cx)?))
        }
    }
}

/// `(point, text)` inserting `let <name> = <name>.into_computed();` (or
/// `let mut` for a `mut` binding) per parameter as the first statements of
/// `body` — `report` has already checked every rewritten parameter is a
/// plain binding.
fn let_part(
    cx: &LateContext<'_>,
    body: &Body<'_>,
    named: &[(Symbol, bool)],
) -> Option<(Span, String)> {
    let ExprKind::Block(block, _) = body.value.kind else {
        return None;
    };
    if block.span.from_expansion() {
        return None;
    }
    let line = |&(name, mutable): &(Symbol, bool)| {
        format!(
            "let {}{name} = {name}.into_computed();",
            if mutable { "mut " } else { "" }
        )
    };
    let anchor = block
        .stmts
        .first()
        .map(|stmt| stmt.span)
        .or_else(|| block.expr.map(|expr| expr.span));
    Some(match anchor {
        Some(span) if !span.from_expansion() => {
            let indent = snippet_indent(cx.sess(), span).unwrap_or_default();
            let lets = named
                .iter()
                .map(line)
                .collect::<Vec<_>>()
                .join(&format!("\n{indent}"));
            (span.shrink_to_lo(), format!("{lets}\n{indent}"))
        }
        Some(_) => return None,
        None => {
            // No statement and no tail expression — splice just inside the
            // braces so interior comments survive.
            let outer = snippet_indent(cx.sess(), block.span).unwrap_or_default();
            let lets = named
                .iter()
                .map(line)
                .collect::<Vec<_>>()
                .join(&format!("\n{outer}    "));
            (
                block
                    .span
                    .shrink_to_lo()
                    .with_hi(block.span.lo() + BytePos(1))
                    .shrink_to_hi(),
                format!("\n{outer}    {lets}\n{outer}"),
            )
        }
    })
}

fn report(cx: &LateContext<'_>, did: LocalDefId, msg: String) {
    let hir_id = cx.tcx.local_def_id_to_hir_id(did);
    let Some((decl, body, trait_member)) = signature_of(cx.tcx, did) else {
        return;
    };
    let params: Vec<(u32, Ty<'_>, &rustc_hir::Ty<'_>)> = computed_params(cx, did.to_def_id())
        .into_iter()
        .filter_map(|(index, inner)| {
            let written = decl.inputs.get(index as usize)?;
            (!written.span.from_expansion()).then_some((index, inner, written))
        })
        .collect();
    if params.is_empty() {
        return;
    }
    // Every rewritten parameter must be a plain `name`/`mut name` binding —
    // `_`, `x @ ..`, `ref x`, or a destructuring pattern has no text to
    // rebind (a `ref` binding would hold `&impl IntoComputed`, on which the
    // `self` call cannot resolve), so the item stays silent rather than
    // emit a fix that breaks the body.
    let named: Vec<(Symbol, bool)> = match body {
        Some(body) => {
            let mut named = Vec::with_capacity(params.len());
            for &(index, _, _) in &params {
                let Some(param) = body.params.get(index as usize) else {
                    return;
                };
                let PatKind::Binding(mode, _, ident, None) = param.pat.kind else {
                    return;
                };
                if mode.0 != ByRef::No {
                    return;
                }
                named.push((ident.name, mode.1 == Mutability::Mut));
            }
            named
        }
        None => Vec::new(),
    };
    span_lint_hir_and_then(
        cx,
        COMPUTED_PARAMETER,
        hir_id,
        params[0].2.span,
        msg,
        |diag| {
            let mut parts: Vec<(Span, String)> = Vec::new();
            if let Some(into_computed) =
                into_computed_spelling(cx, hir_id, params[0].2.span.lo(), &mut parts)
            {
                for &(_, inner, written) in &params {
                    parts.push((
                        written.span,
                        format!("impl {into_computed}<{}>", inner_text(cx, written, inner)),
                    ));
                }
                if let Some(body) = body {
                    match let_part(cx, body, &named) {
                        Some(part) => parts.push(part),
                        // The signature can still be edited at the call site's
                        // fragment, but the body belongs to the macro.
                        None if matches!(
                            body.value.kind,
                            ExprKind::Block(block, _) if block.span.from_expansion()
                        ) =>
                        {
                            diag.note(MACRO_NOTE);
                        }
                        None => {}
                    }
                }
                diag.multipart_suggestion(SUGGESTION, parts, Applicability::MaybeIncorrect);
            }
            if trait_member {
                diag.note(TRAIT_NOTE);
            }
        },
    );
}
