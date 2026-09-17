use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::res::{MaybeDef, MaybeQPath};
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind, GenericArg, GenericArgs, LangItem, QPath, Ty};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::GenericParamDefKind;
use rustc_session::declare_lint_pass;
use rustc_span::symbol::Symbol;
use rustc_span::{Span, sym};

use crate::binding::{binding_annotation, binding_name, dedicated_ctor, generic_ctor_call};
use crate::imports::{Bare, bare_status};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `binding(..)` and `Binding::container(..)` calls seeded with the
    /// binding type's default: `T::default()`/`<T as Default>::default()`/
    /// `Default::default()`, the std `new` that is the type's `Default`
    /// (`String::new()`, `Vec::new()`/`Vec::<T>::new()`, `vec![]`,
    /// `HashMap::new()`, `HashSet::new()`, `BTreeMap::new()`,
    /// `BTreeSet::new()`, `VecDeque::new()`), or `None`.
    ///
    /// ### Why is this bad?
    ///
    /// `Binding<T>` implements `Default` whenever `T: Default + Clone`, so the
    /// hand-seeded form says the type and the default twice.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let name: Binding<String> = binding(String::new());
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let name = Binding::<String>::default();
    /// ```
    pub DEFAULT_BINDING_CONSTRUCTOR,
    style,
    "`binding(..)`/`Binding::container(..)` seeded with the type's default is `Binding::<T>::default()`"
}

declare_lint_pass!(DefaultBindingConstructor => [DEFAULT_BINDING_CONSTRUCTOR]);

/// The std collections whose inherent `new` is their `Default`, as diagnostic
/// names matched against the `new` callee's self type (`String` is a lang
/// item and is checked separately).
const DEFAULT_NEWS: &[Symbol] = &[
    sym::Vec,
    clippy_utils::sym::VecDeque,
    sym::HashMap,
    sym::HashSet,
    sym::BTreeMap,
    clippy_utils::sym::BTreeSet,
];

/// `span`'s source text for spelling `T` — `None` when the span is in an
/// expansion: the text would come from a `macro_rules!` body and name types
/// the call site cannot resolve.
fn text_at(cx: &LateContext<'_>, span: Span) -> Option<String> {
    if span.from_expansion() {
        return None;
    }
    snippet_opt(cx.sess(), span)
}

/// Whether `arg` is one of the default-value shapes the lint rewrites, and if
/// so how `T` is spelled in its source — `Some(None)` for a shape that does
/// not name `T` (`Default::default()`, a bare `None`, an under-specified
/// `HashMap::new()`, or a spelling that lives in a macro definition), `None`
/// for an argument that is not a default value.
fn default_seed(cx: &LateContext<'_>, arg: &Expr<'_>) -> Option<Option<String>> {
    match arg.kind {
        ExprKind::Call(func, []) => {
            let Res::Def(DefKind::AssocFn, did) = func.res(cx) else {
                return None;
            };
            (did.assoc_fn_parent(cx).is_diag_item(cx, sym::Default)
                || is_default_new(cx, func, did))
            .then(|| self_ty_text(cx, func))
        }
        ExprKind::Path(..)
            if arg
                .res(cx)
                .ctor_parent(cx)
                .is_lang_item(cx, LangItem::OptionNone) =>
        {
            Some(none_ty_text(cx, arg))
        }
        _ => None,
    }
}

/// Whether `did` — the resolved callee of the zero-argument call `func` — is
/// the inherent `new` of a std type whose `new` produces its `Default`
/// (`String::new()`, `Vec::new()`, `HashMap::new()`, …; `vec![]` lowers to
/// `Vec::new()`).
fn is_default_new(cx: &LateContext<'_>, func: &Expr<'_>, did: DefId) -> bool {
    if !matches!(func.kind, ExprKind::Path(QPath::TypeRelative(_, seg)) if seg.ident.name == sym::new)
    {
        return false;
    }
    let Some(impl_did) = cx.tcx.inherent_impl_of_assoc(did) else {
        return false;
    };
    let Some(adt) = cx
        .tcx
        .type_of(impl_did)
        .instantiate_identity()
        .skip_norm_wip()
        .ty_adt_def()
    else {
        return false;
    };
    cx.tcx.lang_items().string() == Some(adt.did())
        || cx
            .tcx
            .get_diagnostic_name(adt.did())
            .is_some_and(|name| DEFAULT_NEWS.contains(&name))
}

/// `T` as the call's self type spells it — `WindowState` in
/// `WindowState::default()`, `Vec::<Item>` in `Vec::<Item>::new()`, `T` in
/// `<T as Default>::default()`. `None` when the self type is not a path
/// (`Default::default()` spells the trait, not `T`), or when it names a type
/// whose required generics the path does not give (`HashMap::new()` cannot
/// spell `HashMap<K, V>`).
fn self_ty_text(cx: &LateContext<'_>, func: &Expr<'_>) -> Option<String> {
    let ExprKind::Path(QPath::TypeRelative(ty, _)) = func.kind else {
        return None;
    };
    ty_text(cx, ty)
}

/// `ty` as source text for `T` — following a qualified self (`<T as Trait>`'s
/// `T`) and requiring the path to spell every generic the type needs.
fn ty_text(cx: &LateContext<'_>, ty: &Ty<'_>) -> Option<String> {
    let rustc_hir::TyKind::Path(QPath::Resolved(qself, path)) = ty.kind else {
        return None;
    };
    // `<T as Trait>` — the self type is `T`, the path is the trait.
    if let Some(qself) = qself {
        return ty_text(cx, qself);
    }
    let did = match path.res {
        Res::Def(DefKind::TyParam, _) => return text_at(cx, ty.span),
        Res::Def(
            DefKind::Struct
            | DefKind::Enum
            | DefKind::Union
            | DefKind::TyAlias
            | DefKind::AssocTy
            | DefKind::ForeignTy,
            did,
        ) => did,
        _ => return None,
    };
    let required = cx
        .tcx
        .generics_of(did)
        .own_params
        .iter()
        .filter(|param| match param.kind {
            GenericParamDefKind::Type { has_default, .. }
            | GenericParamDefKind::Const { has_default } => !has_default,
            GenericParamDefKind::Lifetime => false,
        })
        .count();
    let provided = path.segments.last().map_or(0, |seg| {
        seg.args.map_or(0, |args| {
            args.args
                .iter()
                .filter(|arg| !matches!(arg, GenericArg::Lifetime(..)))
                .count()
        })
    });
    if provided < required {
        return None;
    }
    text_at(cx, ty.span)
}

/// `None::<u8>` spells its `T` as `Option<u8>` — the variant's generic
/// arguments moved onto the enum name. A bare `None`, or a call site where a
/// bare `Option` would not resolve to the option enum, spells nothing.
fn none_ty_text(cx: &LateContext<'_>, arg: &Expr<'_>) -> Option<String> {
    let ExprKind::Path(QPath::Resolved(_, path)) = arg.kind else {
        return None;
    };
    let args = path.segments.last()?.args?;
    let option_did = cx.tcx.lang_items().option_type()?;
    if !matches!(
        bare_status(
            cx,
            arg.hir_id,
            arg.span.lo(),
            sym::Option,
            Namespace::TypeNS,
            Some(option_did),
        ),
        Bare::Same
    ) {
        return None;
    }
    Some(format!("Option{}", text_at(cx, args.span_ext)?))
}

/// `T` from the call's own generics — `binding::<T>`'s turbofish or the
/// `Binding::<T>` in `Binding::<T>::container`.
fn call_ty_text(cx: &LateContext<'_>, func: &Expr<'_>) -> Option<String> {
    let ExprKind::Path(qpath) = func.kind else {
        return None;
    };
    match qpath {
        QPath::Resolved(_, path) => path
            .segments
            .last()?
            .args
            .and_then(|args| first_ty_arg(cx, args)),
        QPath::TypeRelative(ty, _) => {
            let rustc_hir::TyKind::Path(QPath::Resolved(_, path)) = ty.kind else {
                return None;
            };
            path.segments
                .last()?
                .args
                .and_then(|args| first_ty_arg(cx, args))
        }
    }
}

/// The source text of the first type generic argument — `T` in `::<T>`. A
/// `GenericArg::Infer` (`_`) spells nothing.
fn first_ty_arg(cx: &LateContext<'_>, args: &GenericArgs<'_>) -> Option<String> {
    args.args.iter().find_map(|arg| match arg {
        GenericArg::Type(ty) => text_at(cx, ty.span),
        _ => None,
    })
}

impl<'tcx> LateLintPass<'tcx> for DefaultBindingConstructor {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let Some(call) = generic_ctor_call(cx, expr) else {
            return;
        };
        // `binding(0)`/`binding(false)` are `typed_binding_constructor`'s; the
        // seed must be a default of `T` itself, not another `Into<T>` type's.
        if dedicated_ctor(call.value_ty).is_some()
            || cx.typeck_results().expr_ty(call.arg) != call.value_ty
        {
            return;
        }
        let Some(arg_text) = default_seed(cx, call.arg) else {
            return;
        };
        // `binding`/`container` already require `T: Clone + 'static`; `Default`
        // is the bound `Binding::<T>::default()` adds.
        let Some(default_trait) = cx.tcx.get_diagnostic_item(sym::Default) else {
            return;
        };
        if !implements_trait(cx, call.value_ty, default_trait, &[]) {
            return;
        }

        let annotation = binding_annotation(cx, expr);
        let ty_text = arg_text
            .or_else(|| call_ty_text(cx, call.func))
            .or_else(|| {
                annotation.as_ref().and_then(|ann| {
                    ann.path
                        .segments
                        .last()
                        .and_then(|seg| seg.args)
                        .and_then(|args| first_ty_arg(cx, args))
                })
            });

        let mut parts: Vec<(Span, String)> = Vec::new();
        let mut applicable = Applicability::MachineApplicable;
        let name = binding_name(
            cx,
            expr.hir_id,
            expr.span.lo(),
            call.adt_did,
            &mut parts,
            &mut applicable,
        );
        let generics = match ty_text {
            Some(text) => format!("::<{text}>"),
            // Nothing spells `T` — `Binding::default()` compiles only when a
            // later use pins `T`, so it is not machine-applicable.
            None => {
                applicable = Applicability::MaybeIncorrect;
                String::new()
            }
        };
        span_lint_and_then(
            cx,
            DEFAULT_BINDING_CONSTRUCTOR,
            expr.span,
            "this binding is seeded with its type's default",
            |diag| match name {
                Some(name) => {
                    let replacement = format!("{name}{generics}::default()");
                    parts.push((expr.span, replacement.clone()));
                    if let Some(ann) = annotation {
                        parts.push((ann.erase, String::new()));
                    }
                    diag.multipart_suggestion(format!("use `{replacement}`"), parts, applicable);
                }
                // Neither `waterui` nor `nami` is nameable here — the rewrite
                // cannot be spelled, so no suggestion is attached.
                None => {
                    diag.help(format!("use `Binding{generics}::default()`"));
                }
            },
        );
    }
}
