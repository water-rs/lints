use clippy_utils::diagnostics::{span_lint_and_sugg, span_lint_and_then};
use clippy_utils::paths::{PathNS, lookup_path_str};
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{AssocContainer, TyKind};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::Symbol;

use crate::anyview::peel;
use crate::applicability::comment_guard;
use crate::color::{INTO, SIZED};
use crate::def_path::def_path_eq;
use crate::imports::{Bare, bare_status, use_insertion};
use crate::param_bounds::{BoundTarget, call_arg_all_bounds, call_args, single_use_param};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags an alignment written as an associated constant —
    /// `HorizontalAlignment::Leading`, `Alignment::TopLeading` — passed to
    /// `.alignment(..)` on `VStack`, `HStack`, `ZStack`, `Frame`, `Grid`, or
    /// `Overlay`, or to any other `impl Into<Alignment>`/
    /// `Into<HorizontalAlignment>`/`Into<VerticalAlignment>` parameter.
    ///
    /// ### Why is this bad?
    ///
    /// The containers carry a method per position — `.leading()`,
    /// `.centered()`, `.top_leading()`, … — and every other `Into` position
    /// takes the token types (`Leading`, `TopLeading`, …) directly, each
    /// token converting into exactly the alignment types it is legal for.
    /// The qualified constant is the long spelling of a name the API
    /// already gives a word.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// vstack((text("a"),)).alignment(HorizontalAlignment::Leading)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// vstack((text("a"),)).leading()
    /// ```
    pub QUALIFIED_ALIGNMENT,
    style,
    "an alignment written `X::CONST` where the position has a method or token name"
}

/// The lint pass — stateless; every check resolves through `typeck`.
pub struct QualifiedAlignment;

impl_lint_pass!(QualifiedAlignment => [QUALIFIED_ALIGNMENT]);

/// `waterui_core`'s alignment types — `ui::layout` is the defining module;
/// `waterui_core::layout` is a re-export of it.
const ALIGNMENT_TYS: &[&[&str]] = &[
    &["waterui_core", "ui", "layout", "Alignment"],
    &["waterui_core", "ui", "layout", "HorizontalAlignment"],
    &["waterui_core", "ui", "layout", "VerticalAlignment"],
];

/// The containers whose `.alignment(impl Into<..>)` has a per-position
/// method — `VStack` aligns horizontally, `HStack` vertically, the rest on
/// both axes.
const CONTAINERS: &[&[&str]] = &[
    &["waterui_layout", "collections", "grid", "Grid"],
    &["waterui_layout", "containers", "frame", "Frame"],
    &["waterui_layout", "modifiers", "overlay", "Overlay"],
    &["waterui_layout", "stack", "hstack", "HStack"],
    &["waterui_layout", "stack", "vstack", "VStack"],
    &["waterui_layout", "stack", "zstack", "ZStack"],
];

/// The container method spelling `name` — `Center` maps to `centered`,
/// every other position to its snake_case name. A constant outside the
/// table has no token either, so it is silent in both rewrites.
fn position_method(name: Symbol) -> Option<&'static str> {
    Some(match name.as_str() {
        "Leading" => "leading",
        "Trailing" => "trailing",
        "Center" => "centered",
        "Top" => "top",
        "Bottom" => "bottom",
        "FirstBaseline" => "first_baseline",
        "LastBaseline" => "last_baseline",
        "TopLeading" => "top_leading",
        "TopTrailing" => "top_trailing",
        "BottomLeading" => "bottom_leading",
        "BottomTrailing" => "bottom_trailing",
        _ => return None,
    })
}

/// `arg` is an `Alignment`/`HorizontalAlignment`/`VerticalAlignment`
/// associated-constant path (`Alignment::TopLeading`) — returns the
/// constant's name and the `DefId` of the alignment type it belongs to.
/// A token (`Leading`) is a unit-struct constructor, not an associated
/// constant, so token arguments are already silent here, as is any
/// non-constant argument.
fn alignment_const(cx: &LateContext<'_>, arg: &Expr<'_>) -> Option<(Symbol, DefId)> {
    let ExprKind::Path(qpath) = arg.kind else {
        return None;
    };
    let Res::Def(
        DefKind::AssocConst {
            is_type_const: false,
        },
        did,
    ) = cx.typeck_results().qpath_res(&qpath, arg.hir_id)
    else {
        return None;
    };
    let assoc = cx.tcx.opt_associated_item(did)?;
    if assoc.container != AssocContainer::InherentImpl {
        return None;
    }
    let self_ty = cx
        .tcx
        .type_of(cx.tcx.parent(did))
        .instantiate_identity()
        .skip_norm_wip();
    let TyKind::Adt(adt, _) = *self_ty.kind() else {
        return None;
    };
    ALIGNMENT_TYS
        .iter()
        .any(|path| def_path_eq(cx, adt.did(), path))
        .then(|| (cx.tcx.item_name(did), adt.did()))
}

/// `target` is an `Into<Alignment>`/`Into<HorizontalAlignment>`/
/// `Into<VerticalAlignment>` bound — returns the alignment type's `DefId`.
fn into_alignment(cx: &LateContext<'_>, target: &BoundTarget<'_>) -> Option<DefId> {
    if !def_path_eq(cx, target.trait_did, INTO) {
        return None;
    }
    target
        .args
        .first()
        .and_then(|arg| arg.as_type())
        .and_then(|ty| ty.ty_adt_def())
        .map(|adt| adt.did())
        .filter(|did| ALIGNMENT_TYS.iter().any(|path| def_path_eq(cx, *did, path)))
}

/// A bound the token rewrite is verified against: `Into<alignment>` or the
/// implicit `Sized` every parameter carries. A parameter carrying any other
/// bound (`impl Into<Alignment> + Debug`) goes unchecked — the token might
/// not satisfy it — so the call stays silent.
fn known_bound(cx: &LateContext<'_>, target: &BoundTarget<'_>) -> bool {
    into_alignment(cx, target).is_some() || def_path_eq(cx, target.trait_did, SIZED)
}

/// `X::CONST` at an `impl Into<..>` position rewrites to the bare token,
/// importing `waterui::layout::<Token>` when the name is not already bound.
fn report_token(cx: &LateContext<'_>, arg: &Expr<'_>, konst: Symbol, bound_ty: DefId) {
    let Some(&token_did) = lookup_path_str(
        cx.tcx,
        PathNS::Type,
        &format!("waterui_layout::alignment::{konst}"),
    )
    .first() else {
        return;
    };
    let bound = cx.tcx.item_name(bound_ty);
    let msg = format!("this parameter takes `impl Into<{bound}>`; pass the `{konst}` token");
    let status = bare_status(
        cx,
        arg.hir_id,
        arg.span.lo(),
        konst,
        Namespace::ValueNS,
        Some(token_did),
    );
    // A `use` binds both namespaces: when the name is free in `ValueNS`
    // but bound to something else in `TypeNS` (a local `struct Center { .. }`,
    // `trait`, or `type` — value-namespace-free spellings), inserting
    // `use waterui::layout::Center;` fails E0255, so only a `TypeNS`-clear
    // position keeps the plain-import arm.
    let status = match status {
        Bare::Free => match bare_status(
            cx,
            arg.hir_id,
            arg.span.lo(),
            konst,
            Namespace::TypeNS,
            Some(token_did),
        ) {
            Bare::Free | Bare::Same => Bare::Free,
            other => other,
        },
        other => other,
    };
    match status {
        Bare::Same => {
            let mut app = Applicability::MachineApplicable;
            comment_guard(cx, arg.span, &mut app);
            span_lint_and_sugg(
                cx,
                QUALIFIED_ALIGNMENT,
                arg.span,
                msg,
                "pass the alignment token",
                konst.to_string(),
                app,
            );
        }
        Bare::Free | Bare::Conflict | Bare::Unknown => {
            let use_path = format!("waterui::layout::{konst}");
            let Some((point, before, after)) = use_insertion(cx, arg.hir_id, &use_path) else {
                return;
            };
            let (label, use_text, arg_text) = if matches!(status, Bare::Free) {
                (
                    format!("import `{use_path}` and pass the token"),
                    format!("{before}use {use_path};{after}"),
                    konst.to_string(),
                )
            } else {
                // A `use` for a name that already binds something else, or a
                // block-level glob whose resolution cannot be enumerated,
                // does not always compile — the alias form is suggested but
                // left for a human to confirm.
                let alias = format!("layout_{konst}");
                (
                    format!("`{konst}` resolves to a different item here — import under an alias"),
                    format!("{before}use {use_path} as {alias};{after}"),
                    alias,
                )
            };
            let mut app = match status {
                Bare::Free => Applicability::MachineApplicable,
                _ => Applicability::MaybeIncorrect,
            };
            comment_guard(cx, arg.span, &mut app);
            span_lint_and_then(cx, QUALIFIED_ALIGNMENT, arg.span, msg, |diag| {
                diag.multipart_suggestion(
                    label,
                    vec![(point, use_text), (arg.span, arg_text)],
                    app,
                );
            });
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for QualifiedAlignment {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        // `.alignment(X::CONST)` on a container — the position method names
        // it, so the whole `alignment(..)` tail is replaced.
        if let ExprKind::MethodCall(seg, receiver, [arg], _) = expr.kind
            && seg.ident.name.as_str() == "alignment"
            && let TyKind::Adt(adt, _) = *cx
                .typeck_results()
                .expr_ty_adjusted(receiver)
                .peel_refs()
                .kind()
            && CONTAINERS
                .iter()
                .any(|path| def_path_eq(cx, adt.did(), path))
            && let Some((konst, _)) = alignment_const(cx, peel(arg))
            && let Some(method) = position_method(konst)
        {
            let span = expr.span.with_lo(seg.ident.span.lo());
            let mut app = Applicability::MachineApplicable;
            comment_guard(cx, span, &mut app);
            span_lint_and_sugg(
                cx,
                QUALIFIED_ALIGNMENT,
                span,
                format!("this container's `.{method}()` method names the position"),
                "use the position method",
                format!("{method}()"),
                app,
            );
            return;
        }
        // Every other `impl Into<alignment>` position takes the token.
        let Some((callee, per_arg)) = call_arg_all_bounds(cx, expr) else {
            return;
        };
        for (index, (arg, targets)) in call_args(expr).into_iter().zip(per_arg).enumerate() {
            let Some(bound_ty) = targets.iter().find_map(|t| into_alignment(cx, t)) else {
                continue;
            };
            // The token is a different type than the constant — if the
            // parameter's type occurs anywhere else in the signature, the
            // rewrite shifts it under the other occurrences and the fix
            // does not compile.
            if !targets.iter().all(|t| known_bound(cx, t)) || !single_use_param(cx, callee, index) {
                continue;
            }
            let arg = peel(arg);
            if arg.span.from_expansion() {
                continue;
            }
            let Some((konst, konst_ty)) = alignment_const(cx, arg) else {
                continue;
            };
            // `Token: Into<bound>` yields `bound::CONST` only when the
            // constant's alignment type is the bound's — the `From` impl
            // then produces the same value the constant names.
            if konst_ty != bound_ty || position_method(konst).is_none() {
                continue;
            }
            report_token(cx, arg, konst, bound_ty);
        }
    }
}
