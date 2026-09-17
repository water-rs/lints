use std::cell::OnceCell;

use clippy_utils::diagnostics::span_lint_hir_and_then;
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::source::{snippet_indent, snippet_opt};
use clippy_utils::ty::{implements_trait, ty_from_hir_ty};
use rustc_errors::Applicability;
use rustc_hir::attrs::AttributeKind;
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{self, FnKind, Visitor};
use rustc_hir::{
    AmbigArg, BindingMode, Body, BodyId, ByRef, Expr, ExprKind, FnDecl, GenericArg, HirId,
    Mutability, Node, Param, Pat, PatKind, QPath, Ty, TyKind,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter::OnlyBodies;
use rustc_middle::ty::{self, GenericParamDefKind, TyCtxt, TypeVisitableExt};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::Symbol;
use rustc_span::{Ident, Span};

use crate::applicability::comment_guard;
use crate::def_path::def_path_eq;
use crate::imports::{Bare, bare_status, extern_nameable, target_did, use_insertion};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a closure parameter — the `f` of `button(..).action(..)` and
    /// other `Handler`/`HandlerOnce` positions, or of `use_env(..)` — whose
    /// declared type is `State<T>` where `T` extracts the same environment
    /// value `State<T>` does: a type whose `Extractor` impl reads the
    /// `.state(&value)` channel (`SnackbarManager`, `Navigator<Route>`,
    /// `DynamicHandler`, any local `#[state]` type), or a non-generic
    /// `Clone` struct/enum defined in the same crate, which `#[state]`
    /// makes extractable. `State<Binding<T>>`, `State<Vec<..>>`,
    /// `State<Option<..>>`, `State<Environment>`, and `State<T>` over a type
    /// parameter of the enclosing function stay silent.
    ///
    /// ### Why is this bad?
    ///
    /// `State<T>` exists for values the crate cannot implement traits on —
    /// `Binding<String>` and other foreign types — and for `Extractor`
    /// impls that read a different environment channel. Wrapping a type
    /// whose `extract` delegates to `State<T>` only adds a pattern to
    /// destructure: the bare `T` extracts the same environment value.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// button("Save").action(|State(m): State<SnackbarManager>| m.show(..));
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// button("Save").action(|m: SnackbarManager| m.show(..));
    /// ```
    pub NEEDLESS_STATE_WRAPPER,
    style,
    "a `State<T>` closure parameter whose `T` can be extracted directly"
}

const MSG: &str =
    "`State<T>` is for types that cannot implement `Extractor`; `T` can be extracted directly";
const LOCAL_NOTE: &str = "add `#[state]` to `T`";

/// `waterui_core`'s `State<T>` — the extractor wrapper for foreign values.
const STATE: &[&str] = &["waterui_core", "foundation", "extract", "State"];

/// The `Extractor` trait, spelled through `waterui_core`'s public
/// re-export (`foundation` is private).
const EXTRACTOR: &str = "waterui_core::extract::Extractor";

/// The `#[state]` attribute macro at its defining path.
const STATE_MACRO: &str = "waterui_macros::state";

/// `Extractor` impls that read the `.state(&value)` channel — the types
/// whose `extract` delegates to `<State<Self> as Extractor>::extract`, so
/// `State<T>` and bare `T` are the same extraction. `Navigator`'s impl is
/// hand-written (a `#[state]` impl would be unconditional on `T: Clone`,
/// which `Navigator<T>`'s `Clone` is not); the rest are `#[state]` impls in
/// the framework, foreign to the linted crate, so their macro origin is not
/// inspectable — they are named here instead. Every other `Extractor` impl
/// (`Environment`, `Option<E>`, `Use<T>`, `State<T>` itself, the tuple impls,
/// `impl_extractor!` types, foreign hand impls like `ChromiumProxy`) reads a
/// different channel and stays silent.
const STATE_CHANNEL: &[&[&str]] = &[
    &["waterui_internal", "runtime", "snackbar", "SnackbarManager"],
    &[
        "waterui_internal",
        "runtime",
        "fullscreen",
        "FullScreenOverlayManager",
    ],
    &["waterui_core", "components", "dynamic", "DynamicHandler"],
    &["waterui_navigation", "Navigator"],
    &["waterui_chromium", "controller", "ChromiumController"],
    &["waterui_chromium", "page", "ChromiumPage"],
];

/// A flagged `State<T>` closure parameter — its HIR nodes plus how `T`
/// qualifies for direct extraction.
struct Flag<'tcx> {
    /// The parameter (its `pat` and whole `pat: ty` span).
    param: &'tcx Param<'tcx>,
    /// The declared `State<T>` annotation.
    ty: &'tcx Ty<'tcx>,
    /// `State`'s ADT `DefId`.
    state: DefId,
    /// The semantic `T`.
    inner: ty::Ty<'tcx>,
    /// `T`'s item when `T` is a crate-local `Clone` struct/enum the fix marks
    /// `#[state]` — `None` when `T` already implements `Extractor`.
    local: Option<LocalDefId>,
}

/// Resolved def ids, cached on first use.
#[derive(Clone, Copy)]
struct Defs {
    /// `waterui_core::extract::Extractor`.
    extractor: DefId,
    /// `waterui_macros::state` — the `#[state]` attribute, when the crate's
    /// dependency graph can name it.
    state: Option<DefId>,
}

/// The lint pass.
#[derive(Default)]
pub struct NeedlessStateWrapper {
    defs: OnceCell<Option<Defs>>,
}

impl_lint_pass!(NeedlessStateWrapper => [NEEDLESS_STATE_WRAPPER]);

impl NeedlessStateWrapper {
    fn defs(&self, cx: &LateContext<'_>) -> Option<Defs> {
        *self.defs.get_or_init(|| {
            let extractor = lookup_path_str(cx.tcx, PathNS::Type, EXTRACTOR)
                .first()
                .copied()?;
            let state = lookup_path_str(cx.tcx, PathNS::Macro, STATE_MACRO)
                .first()
                .copied();
            Some(Defs { extractor, state })
        })
    }

    fn report<'tcx>(&self, cx: &LateContext<'tcx>, body: &'tcx Body<'tcx>, flag: Flag<'tcx>) {
        let Flag {
            param,
            ty,
            state,
            inner,
            local,
        } = flag;
        let inner_text = written_inner(ty)
            .and_then(|inner| snippet_opt(cx, inner.span))
            .unwrap_or_else(|| inner.to_string());
        let mut app = Applicability::MachineApplicable;
        let mut parts: Vec<(Span, String)> = Vec::new();
        match param.pat.kind {
            // `s: State<T>` — the type becomes `T` and every `s.0` use in the
            // body becomes `s`; any other use shape (`*s`, `s.clone()` on the
            // wrapper, passing `s`) demotes to `MaybeIncorrect`.
            PatKind::Binding(mode, local, ident, None) if mode.0 == ByRef::No => {
                parts.push((ty.span, inner_text.clone()));
                let mut uses = DotFieldUses {
                    tcx: cx.tcx,
                    local,
                    fields: Vec::new(),
                    other: false,
                };
                uses.visit_body(body);
                for span in uses.fields {
                    parts.push((span, ident.name.to_string()));
                }
                if uses.other {
                    app = Applicability::MaybeIncorrect;
                }
            }
            PatKind::Wild => parts.push((ty.span, inner_text.clone())),
            _ => match state_pattern(cx, param.pat, state) {
                // `State(m)` → `m`; `State(..)` → `_`.
                Some(binding) => {
                    parts.push((param.pat.span, binding));
                    parts.push((ty.span, inner_text.clone()));
                }
                None => {
                    // A destructuring the rewrite cannot express (`State(ref
                    // m)`, `x @ State(..)`, nested patterns) — report without
                    // a suggestion.
                    span_lint_hir_and_then(
                        cx,
                        NEEDLESS_STATE_WRAPPER,
                        ty.hir_id,
                        ty.span,
                        MSG,
                        |diag| {
                            if local.is_some() {
                                diag.note(LOCAL_NOTE);
                            }
                        },
                    );
                    return;
                }
            },
        }
        let name = local.map(|did| cx.tcx.item_name(did).to_string());
        if let Some(did) = local {
            self.state_attr(cx, did, &mut parts, &mut app);
        }
        for (span, _) in &parts {
            comment_guard(cx, *span, &mut app);
        }
        let label = match name {
            Some(name) => format!("mark `{name}` `#[state]` and extract it directly"),
            None => format!("extract `{inner_text}` directly"),
        };
        span_lint_hir_and_then(
            cx,
            NEEDLESS_STATE_WRAPPER,
            ty.hir_id,
            ty.span,
            MSG,
            |diag| {
                diag.multipart_suggestion(label, parts, app);
                if local.is_some() {
                    diag.note(LOCAL_NOTE);
                }
            },
        );
    }

    /// The `#[state]` half of the local-type fix: an insertion of the
    /// attribute above `did`'s item — before its first attribute or doc
    /// comment, where the framework writes it — plus a `use` for the macro
    /// when `state` does not already resolve to it at that position.
    fn state_attr(
        &self,
        cx: &LateContext<'_>,
        did: LocalDefId,
        parts: &mut Vec<(Span, String)>,
        app: &mut Applicability,
    ) {
        let hir_id = cx.tcx.local_def_id_to_hir_id(did);
        let Node::Item(item) = cx.tcx.hir_node(hir_id) else {
            *app = Applicability::MaybeIncorrect;
            return;
        };
        if item.span.from_expansion() {
            *app = Applicability::MaybeIncorrect;
            return;
        }
        // `item.span` starts at the item keyword (`pub`, `struct`, `enum`) —
        // after its attributes. The attribute insertion point is the lowest
        // `lo` of `item.span` and the HIR attribute spans that expose one:
        // doc comments (`AttributeKind::DocComment`) and unparsed
        // attributes. `#[derive]` is consumed at expansion and most parsed
        // builtin attributes expose no span, so without a doc comment the
        // point falls back to the item keyword.
        let attrs_lo = cx
            .tcx
            .hir_attrs(hir_id)
            .iter()
            .filter_map(|attr| match attr {
                rustc_hir::Attribute::Unparsed(item) => Some(item.span.lo()),
                rustc_hir::Attribute::Parsed(AttributeKind::DocComment { span, .. }) => {
                    Some(span.lo())
                }
                _ => None,
            });
        let head = item.span.with_lo(
            attrs_lo
                .chain(Some(item.span.lo()))
                .min()
                .unwrap_or(item.span.lo()),
        );
        let indent = snippet_indent(cx, head).unwrap_or_default();
        parts.push((head.shrink_to_lo(), format!("#[state]\n{indent}")));
        let status = self.defs(cx).map_or(Bare::Unknown, |defs| {
            defs.state.map_or(Bare::Unknown, |state| {
                bare_status(
                    cx,
                    hir_id,
                    item.span.lo(),
                    Symbol::intern("state"),
                    Namespace::MacroNS,
                    Some(state),
                )
            })
        });
        match status {
            Bare::Same => {}
            Bare::Free | Bare::Unknown | Bare::Conflict => {
                match state_use_path(cx).and_then(|path| {
                    use_insertion(cx, hir_id, path).map(|(point, before, after)| {
                        (point, format!("{before}use {path};{after}"))
                    })
                }) {
                    Some(part) => parts.push(part),
                    // No nameable crate spells the macro, or the module's
                    // `use` position is in an expansion — the attribute
                    // alone would not resolve.
                    None => *app = Applicability::MaybeIncorrect,
                }
                if !matches!(status, Bare::Free) {
                    *app = Applicability::MaybeIncorrect;
                }
            }
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for NeedlessStateWrapper {
    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        kind: FnKind<'tcx>,
        decl: &'tcx FnDecl<'tcx>,
        body: &'tcx Body<'tcx>,
        _span: Span,
        _id: LocalDefId,
    ) {
        if !matches!(kind, FnKind::Closure) {
            return;
        }
        for (param, ty) in body.params.iter().zip(decl.inputs) {
            self.check_param(cx, body, param, ty);
        }
    }
}

impl NeedlessStateWrapper {
    fn check_param<'tcx>(
        &mut self,
        cx: &LateContext<'tcx>,
        body: &'tcx Body<'tcx>,
        param: &'tcx Param<'tcx>,
        ty: &'tcx Ty<'tcx>,
    ) {
        if ty.span.from_expansion()
            || param.span.from_expansion()
            || matches!(ty.kind, TyKind::Infer(()))
        {
            return;
        }
        let declared = normalize(cx, ty_from_hir_ty(cx, ty));
        let ty::TyKind::Adt(adt, args) = *declared.kind() else {
            return;
        };
        if !def_path_eq(cx, adt.did(), STATE) {
            return;
        }
        let inner = normalize(cx, args.type_at(0));
        if inner.has_infer() {
            return;
        }
        let Some(defs) = self.defs(cx) else {
            return;
        };
        let ty::TyKind::Adt(inner_adt, _) = *inner.kind() else {
            // A type parameter of the enclosing fn, a tuple, a reference —
            // never flaggable. `T: Extractor` on a `Param` does not prove
            // channel equivalence: the caller can instantiate `T` with a
            // non-delegating impl.
            return;
        };
        let inner_did = inner_adt.did();
        if implements_trait(cx, inner, defs.extractor, &[]) {
            // `Extractor` alone is not equivalence: `T::extract` must read
            // the `.state(&value)` channel `State<T>::extract` reads — a
            // `#[state]` impl on a local type (identified by its expansion),
            // or a framework type in `STATE_CHANNEL`.
            if STATE_CHANNEL
                .iter()
                .any(|path| def_path_eq(cx, inner_did, path))
                || (inner_did.is_local() && self.state_impl(cx, defs, inner_did))
            {
                self.report(
                    cx,
                    body,
                    Flag {
                        param,
                        ty,
                        state: adt.did(),
                        inner,
                        local: None,
                    },
                );
            }
            return;
        }
        // Otherwise `T` must be a local `Clone` struct/enum the suggestion
        // can mark `#[state]` — `Binding<_>`, `Vec<_>`, and foreign
        // non-`Extractor` types stay silent.
        let Some(local) = inner_did.as_local() else {
            return;
        };
        if !matches!(cx.tcx.def_kind(local), DefKind::Struct | DefKind::Enum) {
            return;
        }
        // `#[state]` copies the item's generics verbatim into an
        // unconditional `impl` — a type or lifetime parameter would leave
        // `Self: Clone + 'static` unprovable (`W<T>: Clone` needs `T:
        // Clone`, which is not declared). Const parameters are fine.
        if cx
            .tcx
            .generics_of(local)
            .own_params
            .iter()
            .any(|param| !matches!(param.kind, GenericParamDefKind::Const { .. }))
        {
            return;
        }
        let Some(clone) = cx.tcx.lang_items().clone_trait() else {
            return;
        };
        if !implements_trait(cx, inner, clone, &[]) {
            return;
        }
        self.report(
            cx,
            body,
            Flag {
                param,
                ty,
                state: adt.did(),
                inner,
                local: Some(local),
            },
        );
    }

    /// Whether `adt_did` — a local ADT that implements `Extractor` — got the
    /// impl from `#[state]` rather than a hand-written impl reading another
    /// channel.
    fn state_impl(&self, cx: &LateContext<'_>, defs: Defs, adt_did: DefId) -> bool {
        let Some(state) = defs.state else {
            return false;
        };
        cx.tcx.all_impls(defs.extractor).any(|impl_did| {
            impl_did.is_local()
                && matches!(
                    *cx
                        .tcx
                        .type_of(impl_did)
                        .instantiate_identity()
                        .skip_normalization()
                        .kind(),
                    ty::TyKind::Adt(adt, _) if adt.did() == adt_did
                )
                && cx
                    .tcx
                    .def_span(impl_did)
                    .ctxt()
                    .outer_expn_data()
                    .macro_def_id
                    == Some(state)
        })
    }
}

/// The `use` path for `#[state]` in the linted crate — `waterui::state`
/// when the facade is a dependency, `waterui_macros::state` when only the
/// macro crate is nameable (in-tree component crates), `None` when neither
/// is in the extern prelude.
fn state_use_path(cx: &LateContext<'_>) -> Option<&'static str> {
    if extern_nameable(cx, "waterui") {
        Some("waterui::state")
    } else {
        extern_nameable(cx, "waterui_macros").then_some("waterui_macros::state")
    }
}

/// `ty` with type aliases normalized away — a parameter annotated through
/// `type My = State<X>` is still `State<X>`.
fn normalize<'tcx>(cx: &LateContext<'tcx>, ty: ty::Ty<'tcx>) -> ty::Ty<'tcx> {
    cx.tcx
        .try_normalize_erasing_regions(cx.typing_env(), ty::Unnormalized::new_wip(ty))
        .unwrap_or(ty)
}

/// The `T` as written inside `State<T>` — `None` when the annotation is not
/// a plain `State<..>` path (an alias, a macro-spelled type), in which case
/// the suggestion prints the resolved type.
fn written_inner<'hir>(ty: &'hir Ty<'hir>) -> Option<&'hir Ty<'hir, AmbigArg>> {
    let TyKind::Path(QPath::Resolved(None, path)) = ty.kind else {
        return None;
    };
    let args = path.segments.last()?.args?;
    let [GenericArg::Type(inner)] = args.args else {
        return None;
    };
    Some(inner)
}

/// The binding text for a `mode`/`ident` pair — `m`, `mut m`.
fn binding_text(mode: BindingMode, ident: Ident) -> String {
    if mode.1 == Mutability::Mut {
        format!("mut {}", ident.name)
    } else {
        ident.name.to_string()
    }
}

/// The binding a `State(..)` pattern extracts — `State(m)` → `m`,
/// `State(mut m)` → `mut m`, `State(..)`/`State(_)`/`State { .. }` → `_` —
/// or `None` for a pattern the rewrite cannot express (a nested
/// destructuring, a `ref` binding, a binding with a subpattern). The
/// pattern's resolution must land on `State`'s constructor.
fn state_pattern(cx: &LateContext<'_>, pat: &Pat<'_>, state: DefId) -> Option<String> {
    let (res, subpat): (Res, Option<&Pat<'_>>) = match pat.kind {
        PatKind::TupleStruct(ref qpath, pats, _) => {
            let subpat = match pats {
                [] => None,
                [pat] => Some(pat),
                _ => return None,
            };
            (cx.typeck_results().qpath_res(qpath, pat.hir_id), subpat)
        }
        PatKind::Struct(ref qpath, fields, _) => {
            let subpat = match fields {
                [] => None,
                [field] if field.ident.name.as_str() == "0" => Some(field.pat),
                _ => return None,
            };
            (cx.typeck_results().qpath_res(qpath, pat.hir_id), subpat)
        }
        _ => return None,
    };
    if target_did(cx.tcx, res) != Some(state) {
        return None;
    }
    match subpat {
        None => Some("_".to_owned()),
        Some(Pat {
            kind: PatKind::Wild,
            ..
        }) => Some("_".to_owned()),
        Some(Pat {
            kind: PatKind::Binding(mode, _, ident, None),
            ..
        }) if mode.0 == ByRef::No => Some(binding_text(*mode, *ident)),
        _ => None,
    }
}

/// The uses of a `s: State<T>` parameter inside the closure body: every use
/// must be an `s.0` field access for the fix to stay `MachineApplicable`.
struct DotFieldUses<'tcx> {
    tcx: TyCtxt<'tcx>,
    /// The `HirId` of the parameter's `s` binding.
    local: HirId,
    /// Spans of the `s.0` field expressions, in visit order.
    fields: Vec<Span>,
    /// A use that is not an `s.0` field access exists — `*s`, `s.clone()`
    /// on the wrapper, passing `s` itself, a use inside an expansion.
    other: bool,
}

impl<'tcx> Visitor<'tcx> for DotFieldUses<'tcx> {
    type NestedFilter = OnlyBodies;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_nested_body(&mut self, id: BodyId) {
        // Nested closures may capture `s`; their `s.0` uses rewrite the same.
        self.visit_body(self.tcx.hir_body(id));
    }

    fn visit_expr(&mut self, e: &'tcx Expr<'tcx>) {
        if let ExprKind::Path(QPath::Resolved(None, path)) = e.kind
            && path.res == Res::Local(self.local)
        {
            match self.tcx.hir_node(self.tcx.parent_hir_id(e.hir_id)) {
                Node::Expr(
                    field @ Expr {
                        kind: ExprKind::Field(base, ident),
                        ..
                    },
                ) if base.hir_id == e.hir_id
                    && ident.name.as_str() == "0"
                    && !field.span.from_expansion() =>
                {
                    self.fields.push(field.span);
                }
                _ => self.other = true,
            }
        }
        intravisit::walk_expr(self, e);
    }
}
