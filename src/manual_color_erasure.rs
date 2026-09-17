use std::cell::OnceCell;
use std::mem;

use clippy_utils::diagnostics::{span_lint_and_sugg, span_lint_and_then};
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::MaybeQPath;
use clippy_utils::sugg::Sugg;
use clippy_utils::ty::{
    deref_chain, get_adt_inherent_method, implements_trait, make_normalized_projection,
};
use rustc_ast::{LitFloatType, LitIntType, LitKind};
use rustc_data_structures::fx::FxHashSet;
use rustc_errors::Applicability;
use rustc_hir::def::{Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{BodyId, Expr, ExprKind, HirId, Node, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter::OnlyBodies;
use rustc_middle::ty::{
    AssocContainer, FnSig, Ty, TyCtxt, TyKind, TypeVisitableExt, TypeckResults,
};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::Symbol;
use rustc_span::{Span, Spanned};

use crate::anyview::peel;
use crate::def_path::def_path_eq;
use crate::imports::{Bare, bare_status, use_insertion};
use crate::param_bounds::{
    BoundTarget, call_arg_all_bounds, call_arg_all_bounds_in, call_arg_bounds, call_args,
    implemented_trait_item,
};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags arguments passed to an `impl Into<Color>`/`impl IntoBackground`
    /// parameter (`.foreground(..)`, `.border(..)`, `Circle::fill`,
    /// `Border::new`, `Avatar::ring`, …) that erase the colorspace by hand:
    /// `Color::srgb_f32(..)`/`Color::srgb(..)`/`Color::srgb_hex(..)`/
    /// `Color::srgb_u32(..)`/`Color::p3(..)`, and `e.into()`/`Into::into(e)`/
    /// `Color::from(e)`/`Color::new(e)` where `e` is already a
    /// `Resolvable<Resolved = ResolvedColor>` value. A `let`-bound constructor
    /// later passed to such a parameter is flagged on the `let`.
    ///
    /// ### Why is this bad?
    ///
    /// The parameter performs the conversion itself — `Color` implements
    /// `From<T: Resolvable<Resolved = ResolvedColor>>`, so `Srgb`, `P3`,
    /// `Oklch`, `WithOpacity<_>`, and the theme tokens all pass as they are.
    /// Boxing the value first names a conversion the caller never had to
    /// write and hides which colorspace the literal is in.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("a").foreground(Color::srgb_f32(0.9, 0.2, 0.35))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("a").foreground(Srgb::new(0.9, 0.2, 0.35))
    /// ```
    pub MANUAL_COLOR_ERASURE,
    style,
    "a `Color` conversion written by hand where the parameter already takes the colorspace value"
}

/// `waterui_graphics::color::Color`.
const COLOR: &[&str] = &["waterui_graphics", "color", "Color"];

/// `waterui_graphics::color::ResolvedColor` — the `Resolvable::Resolved`
/// output a wrapped color value must have.
const RESOLVED_COLOR: &[&str] = &["waterui_graphics", "color", "ResolvedColor"];

/// `core::convert::Into` — the `Into<Color>` parameter bound.
const INTO: &[&str] = &["core", "convert", "Into"];

/// `waterui_internal::appearance::background::IntoBackground` — the
/// `.background(..)` bound; `Srgb`/`P3`/`Oklch`/`WithOpacity` are `View`s, so
/// the colorspace value passes as a background too.
const INTO_BACKGROUND: &[&str] = &[
    "waterui_internal",
    "appearance",
    "background",
    "IntoBackground",
];

/// `core::convert::From::from` — for `Color::from(e)` the trait-impl method
/// normalizes here.
const FROM: &[&str] = &["core", "convert", "From", "from"];

/// `core::convert::Into::into` — `e.into()` and `Into::into(e)` both
/// normalize here.
const INTO_INTO: &[&str] = &["core", "convert", "Into", "into"];

/// `waterui_graphics::color::Color::new`.
const COLOR_NEW: &[&str] = &["waterui_graphics", "color", "Color", "new"];

/// `core::marker::Sized` — implicit on every parameter, so it is not an
/// extra bound the rewrite goes unverified against.
const SIZED: &[&str] = &["core", "marker", "Sized"];

/// The parameter bounds a colorspace value may be passed to directly.
const COLOR_PARAM_BOUNDS: &[&[&str]] = &[INTO, INTO_BACKGROUND];

/// A `Color` associated constructor that pre-erases a colorspace value,
/// paired with the colorspace constructor the fix names.
struct Ctor {
    /// The `Color::<ctor>` def-path tail.
    ctor: &'static str,
    /// The colorspace type the fix constructs (`Srgb`/`P3`).
    ty: &'static str,
    /// The associated function on `ty` (`new`/`new_u8`/`from_hex`/`from_u32`).
    fn_name: &'static str,
}

const CTORS: &[Ctor] = &[
    Ctor {
        ctor: "srgb_f32",
        ty: "Srgb",
        fn_name: "new",
    },
    Ctor {
        ctor: "srgb",
        ty: "Srgb",
        fn_name: "new_u8",
    },
    Ctor {
        ctor: "srgb_hex",
        ty: "Srgb",
        fn_name: "from_hex",
    },
    Ctor {
        ctor: "srgb_u32",
        ty: "Srgb",
        fn_name: "from_u32",
    },
    Ctor {
        ctor: "p3",
        ty: "P3",
        fn_name: "new",
    },
];

/// What a flagged expression rewrites to.
enum Replacement<'tcx> {
    /// `Color::<ctor>(..)` — the callee path is replaced with `<Srgb|P3>::<fn>`;
    /// the colorspace type may need importing.
    Ctor {
        /// The callee path's span — the suggestion's replacement site.
        replace: Span,
        /// `Srgb` or `P3`.
        ty_name: &'static str,
        /// The constructor on the colorspace type.
        fn_name: &'static str,
        /// The colorspace type's `DefId` — `bare_status`'s target.
        ty_did: DefId,
        /// The rewritten expression's type.
        ty: Ty<'tcx>,
    },
    /// `e.into()`/`Into::into(e)`/`Color::from(e)`/`Color::new(e)` — the whole
    /// expression is replaced by `e`'s source.
    Inner {
        /// `e`'s snippet, parenthesized where the rewrite needs it.
        text: String,
        /// `e`'s type — what the rewritten expression evaluates to.
        ty: Ty<'tcx>,
    },
}

impl<'tcx> Replacement<'tcx> {
    /// The type the rewritten expression evaluates to — the colorspace type
    /// for `Ctor`, the unwrapped value's type for `Inner`.
    fn ty(&self) -> Ty<'tcx> {
        match self {
            Self::Ctor { ty, .. } | Self::Inner { ty, .. } => *ty,
        }
    }
}

/// A flagged manual erasure: the span the diagnostic underlines plus the
/// rewrite it suggests.
struct Flag<'tcx> {
    /// The flagged expression — `Color::srgb_f32(..)` or `e.into()`; the
    /// whole chain when it ends in a `.into()` the rewrite drops.
    span: Span,
    /// The rewrite.
    repl: Replacement<'tcx>,
    /// Spans the suggestion erases: a trailing `.into()` tail
    /// (`recv.hi()..call.hi()`) or an `Into::into(`…`)`/`Color::from(`…`)`
    /// wrapper around the flagged expression. The colorspace value satisfies
    /// the parameter bound directly, and `Srgb::new(..).into()` could not
    /// infer its target.
    erased: Vec<Span>,
}

/// The `DefId`s the lint resolves once per pass — `None` fields' lookups stay
/// `None` when `waterui` is not in the dependency graph.
#[derive(Clone, Copy)]
struct Defs {
    srgb: DefId,
    p3: DefId,
    resolvable: DefId,
}

/// The lint pass. `flagged` deduplicates `let`-initializers reached through
/// more than one argument position.
#[derive(Default)]
pub struct ManualColorErasure {
    defs: OnceCell<Option<Defs>>,
    flagged: FxHashSet<HirId>,
}

impl_lint_pass!(ManualColorErasure => [MANUAL_COLOR_ERASURE]);

fn lookup(tcx: TyCtxt<'_>, path: &str) -> Option<DefId> {
    lookup_path_str(tcx, PathNS::Type, path).first().copied()
}

/// Whether `ty` is `waterui_graphics::color::Color`.
fn is_color(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    matches!(ty.kind(), TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), COLOR))
}

/// Whether the literal can be typed `param` — an unsuffixed literal's
/// inferred type at the *original* call (e.g. `0.5` as `f64` under
/// `impl IntoSignalF32`) may differ from the rewritten method's declared
/// parameter (`Srgb::with_opacity` takes `f32`); retyping it is exactly what
/// inference does. A suffix pins the type instead: `0.5f64` stays `f64` and
/// cannot be passed to an `f32` parameter.
fn literal_fits(lit: &Spanned<LitKind>, param: Ty<'_>) -> bool {
    match lit.node {
        LitKind::Float(_, LitFloatType::Unsuffixed) => {
            matches!(param.kind(), TyKind::Float(_))
        }
        LitKind::Float(_, LitFloatType::Suffixed(suffix)) => {
            matches!(param.kind(), TyKind::Float(fty) if *fty == suffix)
        }
        LitKind::Int(_, LitIntType::Unsuffixed) => {
            matches!(param.kind(), TyKind::Int(_) | TyKind::Uint(_))
        }
        LitKind::Int(_, LitIntType::Signed(suffix)) => {
            matches!(param.kind(), TyKind::Int(ity) if *ity == suffix)
        }
        LitKind::Int(_, LitIntType::Unsigned(suffix)) => {
            matches!(param.kind(), TyKind::Uint(uty) if *uty == suffix)
        }
        LitKind::Bool(..) => param.is_bool(),
        LitKind::Char(..) => matches!(param.kind(), TyKind::Char),
        LitKind::Str(..) => matches!(param.peel_refs().kind(), TyKind::Str),
        _ => false,
    }
}

impl ManualColorErasure {
    /// The crate-local `DefId`s, resolved once.
    fn defs(&self, cx: &LateContext<'_>) -> Option<Defs> {
        *self.defs.get_or_init(|| {
            Some(Defs {
                srgb: lookup(cx.tcx, "waterui_graphics::color::Srgb")?,
                p3: lookup(cx.tcx, "waterui_graphics::color::P3")?,
                resolvable: lookup(cx.tcx, "waterui_core::resolve::Resolvable")?,
            })
        })
    }

    /// Whether `target` is the `Into<Color>` or `IntoBackground` bound — a
    /// parameter position that takes a colorspace value directly.
    fn color_bound(&self, cx: &LateContext<'_>, target: &BoundTarget<'_>) -> bool {
        if def_path_eq(cx, target.trait_did, INTO_BACKGROUND) {
            return true;
        }
        def_path_eq(cx, target.trait_did, INTO)
            && target
                .args
                .first()
                .and_then(|arg| arg.as_type())
                .is_some_and(|ty| is_color(cx, ty))
    }

    /// `impl Into<Color>` or `impl IntoBackground` — the bound the diagnostic
    /// names.
    fn bound_name(&self, cx: &LateContext<'_>, targets: &[BoundTarget<'_>]) -> &'static str {
        if targets
            .iter()
            .any(|target| def_path_eq(cx, target.trait_did, INTO_BACKGROUND))
        {
            "IntoBackground"
        } else {
            "Into<Color>"
        }
    }

    /// Whether `ty` is `Resolvable<Resolved = ResolvedColor>` — the wrapped
    /// value `e` must already be a colorspace value for the `into`/`from`/`new`
    /// rows.
    fn resolvable_color<'tcx>(&self, cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
        if ty.has_infer() {
            return false;
        }
        let Some(defs) = self.defs(cx) else {
            return false;
        };
        implements_trait(cx, ty, defs.resolvable, &[])
            && make_normalized_projection(
                cx.tcx,
                cx.typing_env(),
                defs.resolvable,
                Symbol::intern("Resolved"),
                [ty],
            )
            .is_some_and(|resolved| {
                matches!(resolved.kind(), TyKind::Adt(adt, _) if def_path_eq(cx, adt.did(), RESOLVED_COLOR))
            })
    }

    /// Whether `ty` satisfies every color bound the parameter carries — the
    /// rewritten argument must type-check where the `Color` did. The caller
    /// has already confirmed the parameter carries no bound outside the
    /// color table, so these are all the bounds there are.
    fn satisfies<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        ty: Ty<'tcx>,
        targets: &[BoundTarget<'tcx>],
    ) -> bool {
        !ty.has_infer()
            && targets
                .iter()
                .all(|target| implements_trait(cx, ty, target.trait_did, target.args))
    }

    /// Whether `target` is a bound the color table knows — `Into<Color>` or
    /// `IntoBackground` — or `Sized`, which every parameter carries.
    fn known_bound(&self, cx: &LateContext<'_>, target: &BoundTarget<'_>) -> bool {
        self.color_bound(cx, target) || def_path_eq(cx, target.trait_did, SIZED)
    }

    /// The flag `expr` produces — `expr` is the bottom of a method-call chain
    /// (the constructor itself when there is no chain).
    fn analyze<'tcx>(&self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Option<Flag<'tcx>> {
        let defs = self.defs(cx)?;
        let typeck = cx.typeck_results();
        match expr.kind {
            ExprKind::Call(func, args) => {
                let did = func.res(cx).opt_def_id()?;
                let norm = implemented_trait_item(cx.tcx, did);
                if let Some(row) = CTORS
                    .iter()
                    .find(|row| def_path_eq(cx, norm, &[COLOR[0], COLOR[1], COLOR[2], row.ctor]))
                {
                    let ty_did = match row.ty {
                        "Srgb" => defs.srgb,
                        _ => defs.p3,
                    };
                    let ty = cx
                        .tcx
                        .type_of(ty_did)
                        .instantiate_identity()
                        .skip_norm_wip();
                    return Some(Flag {
                        span: expr.span,
                        repl: Replacement::Ctor {
                            replace: func.span,
                            ty_name: row.ty,
                            fn_name: row.fn_name,
                            ty_did,
                            ty,
                        },
                        erased: Vec::new(),
                    });
                }
                // `Into::into(e)`, `Color::from(e)`, `Color::new(e)` — `e`
                // already is the colorspace value.
                let [inner] = args else {
                    return None;
                };
                if !(def_path_eq(cx, norm, COLOR_NEW)
                    || def_path_eq(cx, norm, INTO_INTO)
                    || (def_path_eq(cx, norm, FROM) && is_color(cx, typeck.expr_ty(expr))))
                {
                    return None;
                }
                let ty = typeck.expr_ty(inner);
                if !self.resolvable_color(cx, ty) {
                    return None;
                }
                Some(Flag {
                    span: expr.span,
                    repl: Replacement::Inner {
                        text: Sugg::hir_opt(cx, inner)?.maybe_paren().to_string(),
                        ty,
                    },
                    erased: Vec::new(),
                })
            }
            ExprKind::MethodCall(_, receiver, [], _) => {
                // `e.into()`.
                let did = typeck.type_dependent_def_id(expr.hir_id)?;
                if !def_path_eq(cx, implemented_trait_item(cx.tcx, did), INTO_INTO) {
                    return None;
                }
                let ty = typeck.expr_ty(receiver);
                if !self.resolvable_color(cx, ty) {
                    return None;
                }
                Some(Flag {
                    span: expr.span,
                    repl: Replacement::Inner {
                        text: Sugg::hir_opt(cx, receiver)?.maybe_paren().to_string(),
                        ty,
                    },
                    erased: Vec::new(),
                })
            }
            _ => None,
        }
    }

    /// Whether `call` is `Into::into(e)`, `Color::from(e)`, or `Color::new(e)`
    /// — a conversion wrapper `analyze` treats as manual erasure when `e` is
    /// a colorspace value.
    fn is_color_wrapper<'tcx>(&self, cx: &LateContext<'tcx>, call: &Expr<'tcx>) -> bool {
        let ExprKind::Call(func, [_]) = call.kind else {
            return false;
        };
        let Some(did) = func.res(cx).opt_def_id() else {
            return false;
        };
        let norm = implemented_trait_item(cx.tcx, did);
        def_path_eq(cx, norm, COLOR_NEW)
            || def_path_eq(cx, norm, INTO_INTO)
            || (def_path_eq(cx, norm, FROM) && is_color(cx, cx.typeck_results().expr_ty(call)))
    }

    /// The output type `recv_ty.<method>(args)` has — `Some` when the
    /// colorspace type offers the method (inherently, through `Deref`, or as
    /// the trait method the original call resolved) and its declared
    /// parameters match the written arguments. `None` keeps the lint silent:
    /// `Srgb` has no `with_headroom`, so `Color::srgb_f32(..).with_headroom(..)`
    /// genuinely needs the `Color`.
    fn chain_step<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        call: &'tcx Expr<'tcx>,
        recv_ty: Ty<'tcx>,
    ) -> Option<Ty<'tcx>> {
        let ExprKind::MethodCall(seg, _, args, _) = call.kind else {
            return None;
        };
        let name = seg.ident.name;
        if let Some(item) = deref_chain(cx, recv_ty)
            .find_map(|ty| get_adt_inherent_method(cx, ty, name))
            .filter(|item| item.is_method())
        {
            let sig = cx
                .tcx
                .fn_sig(item.def_id)
                .instantiate_identity()
                .skip_norm_wip()
                .skip_binder();
            return self.check_method_sig(cx, sig, args);
        }
        let did = cx.typeck_results().type_dependent_def_id(call.hir_id)?;
        let norm = implemented_trait_item(cx.tcx, did);
        let assoc = cx.tcx.opt_associated_item(norm)?;
        if !matches!(assoc.container, AssocContainer::Trait) {
            return None;
        }
        // The rewritten receiver must implement the trait with the call's own
        // arguments (`args[0]` is `Self`), and the trait item's signature —
        // instantiated with the same arguments — must match the written args.
        let trait_did = assoc.container_id(cx.tcx);
        let node_args = cx.typeck_results().node_args(call.hir_id);
        let arity = cx.tcx.generics_of(trait_did).own_params.len() - 1;
        if node_args.len() <= arity {
            return None;
        }
        let trait_args = &node_args[1..=arity];
        if trait_args.iter().any(|arg| arg.has_infer())
            || !implements_trait(cx, recv_ty, trait_did, trait_args)
        {
            return None;
        }
        let sig = cx
            .tcx
            .fn_sig(norm)
            .instantiate(cx.tcx, node_args)
            .skip_norm_wip()
            .skip_binder();
        self.check_method_sig(cx, sig, args)
    }

    /// `sig`'s `inputs[0]` is the receiver; each remaining parameter must
    /// equal the written argument's type, and the output is the chain's next
    /// type.
    fn check_method_sig<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        sig: FnSig<'tcx>,
        args: &'tcx [Expr<'tcx>],
    ) -> Option<Ty<'tcx>> {
        let typeck = cx.typeck_results();
        if sig.inputs().len() != args.len() + 1 {
            return None;
        }
        for (param, arg) in sig.inputs()[1..].iter().zip(args) {
            let arg_ty = typeck.expr_ty_adjusted(arg);
            if arg_ty.has_infer()
                || (*param != arg_ty
                    && !matches!(arg.kind, ExprKind::Lit(lit) if literal_fits(&lit, *param)))
            {
                return None;
            }
        }
        Some(sig.output())
    }

    /// The flag `expr` produces plus the rewritten expression's type —
    /// `None` when `expr` performs no manual erasure or its method chain
    /// would not survive the rewrite. The erasure is either the outermost
    /// call (`e.into()`, `Into::into(e)`, `Color::from(e)`, `Color::new(e)`)
    /// or the innermost one (`Color::srgb_*(..)`/`Color::p3(..)`), with any
    /// method chain on the constructor preserved.
    fn flagged<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        expr: &'tcx Expr<'tcx>,
    ) -> Option<(Flag<'tcx>, Ty<'tcx>)> {
        if let Some(flag) = self.analyze(cx, expr) {
            let ty = flag.repl.ty();
            return Some((flag, ty));
        }
        // `Into::into(<ctor>)`/`Color::from(<ctor>)`/`Color::new(<ctor>)` —
        // the inner conversion already erased to `Color`, so `analyze` on
        // the whole call fails, but the wrapper is part of the erasure: the
        // fix drops `Into::into(` and `)` around the inner rewrite.
        if let ExprKind::Call(_, [inner]) = expr.kind
            && self.is_color_wrapper(cx, expr)
            && let Some((mut flag, ty)) = self.flagged(cx, inner)
        {
            flag.span = expr.span;
            flag.erased.push(expr.span.with_hi(inner.span.lo()));
            flag.erased.push(expr.span.with_lo(inner.span.hi()));
            return Some((flag, ty));
        }
        let mut chain = Vec::new();
        let mut bottom = expr;
        while let ExprKind::MethodCall(_, receiver, _, _) = bottom.kind {
            chain.push(bottom);
            bottom = receiver;
        }
        if bottom.hir_id == expr.hir_id {
            return None;
        }
        let mut flag = self.analyze(cx, bottom)?;
        let mut ty = flag.repl.ty();
        let last = chain.len() - 1;
        for (i, &link) in chain.iter().rev().enumerate() {
            // A `.into()` ending the chain is part of the erasure too — the
            // rewrite drops it, since the colorspace value satisfies the
            // parameter bound directly and `Srgb::new(..).into()` could not
            // infer its target. A `.into()` in the middle of the chain
            // cannot be dropped without re-resolving the methods above it,
            // so the chain stays silent.
            if self.is_into(cx, link) {
                if i != last {
                    return None;
                }
                let ExprKind::MethodCall(_, receiver, _, _) = link.kind else {
                    return None;
                };
                flag.span = expr.span;
                flag.erased.push(link.span.with_lo(receiver.span.hi()));
                break;
            }
            ty = self.chain_step(cx, link, ty)?;
        }
        Some((flag, ty))
    }

    /// Whether `link` is a `.into()` call — `core::convert::Into::into`.
    fn is_into<'tcx>(&self, cx: &LateContext<'tcx>, link: &Expr<'tcx>) -> bool {
        matches!(link.kind, ExprKind::MethodCall(..))
            && cx
                .typeck_results()
                .type_dependent_def_id(link.hir_id)
                .is_some_and(|did| def_path_eq(cx, implemented_trait_item(cx.tcx, did), INTO_INTO))
    }

    /// `arg` sits at a color-bound parameter — flag the erasure it performs,
    /// or the `let` initializer it names.
    fn check_arg<'tcx>(
        &mut self,
        cx: &LateContext<'tcx>,
        arg: &'tcx Expr<'tcx>,
        targets: &[BoundTarget<'tcx>],
    ) {
        let arg = peel(arg);
        if arg.span.from_expansion() {
            return;
        }
        if let ExprKind::Path(QPath::Resolved(None, path)) = arg.kind
            && let Res::Local(pat) = path.res
        {
            self.check_local(cx, pat, self.bound_name(cx, targets));
            return;
        }
        let Some((flag, ty)) = self.flagged(cx, arg) else {
            return;
        };
        if !self.satisfies(cx, ty, targets) {
            return;
        }
        self.report(cx, &flag, arg.hir_id, self.bound_name(cx, targets));
    }

    /// `let c = <ctor>` passed by name — flag the initializer when every use
    /// of `c` is a color-bound argument the rewritten type satisfies.
    /// Annotated (`let c: Color`) and destructured bindings keep their `Color`.
    fn check_local<'tcx>(&mut self, cx: &LateContext<'tcx>, pat: HirId, bound: &'static str) {
        let Some(local) = cx
            .tcx
            .hir_parent_id_iter(pat)
            .find_map(|id| match cx.tcx.hir_node(id) {
                Node::LetStmt(local) => Some(local),
                _ => None,
            })
        else {
            return;
        };
        if local.pat.hir_id != pat || local.ty.is_some() {
            return;
        }
        let Some(init) = local.init else {
            return;
        };
        if init.span.from_expansion() || self.flagged.contains(&init.hir_id) {
            return;
        }
        let Some((flag, ty)) = self.flagged(cx, init) else {
            return;
        };
        if !self.only_color_uses(cx, pat, ty) {
            return;
        }
        self.flagged.insert(init.hir_id);
        self.report(cx, &flag, init.hir_id, bound);
    }

    /// Whether every use of the `pat` local in its body is a whole argument
    /// at a color-bound parameter the rewritten `ty` satisfies — the only
    /// shape under which rewriting the initializer cannot break another use.
    fn only_color_uses<'tcx>(&self, cx: &LateContext<'tcx>, pat: HirId, ty: Ty<'tcx>) -> bool {
        let owner = cx.tcx.hir_enclosing_body_owner(pat);
        let mut scan = LocalUses {
            cx,
            pat,
            typeck: cx.tcx.typeck(owner),
            uses: Vec::new(),
        };
        scan.visit_body(cx.tcx.hir_body_owned_by(owner));
        scan.uses
            .iter()
            .all(|&(use_expr, typeck)| self.use_is_color_arg(cx, typeck, use_expr, ty))
    }

    /// Whether `use_expr` is a whole argument of a call whose matching
    /// parameter carries a color bound `ty` satisfies.
    fn use_is_color_arg<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        typeck: &TypeckResults<'tcx>,
        use_expr: &'tcx Expr<'tcx>,
        ty: Ty<'tcx>,
    ) -> bool {
        if ty.has_infer() {
            return false;
        }
        let mut u = use_expr;
        let parent = loop {
            match cx.tcx.parent_hir_node(u.hir_id) {
                Node::Expr(parent) if matches!(parent.kind, ExprKind::DropTemps(_)) => u = parent,
                Node::Expr(parent) => break parent,
                _ => return false,
            }
        };
        if !matches!(parent.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
            return false;
        }
        let Some(index) = call_args(parent)
            .iter()
            .position(|arg| arg.hir_id == u.hir_id)
        else {
            return false;
        };
        call_arg_all_bounds_in(cx, typeck, parent).is_some_and(|(_, per_arg)| {
            per_arg.get(index).is_some_and(|targets| {
                targets.iter().any(|target| self.color_bound(cx, target))
                    && targets.iter().all(|target| {
                        self.known_bound(cx, target)
                            && (def_path_eq(cx, target.trait_did, SIZED)
                                || implements_trait(cx, ty, target.trait_did, target.args))
                    })
            })
        })
    }

    fn report(&self, cx: &LateContext<'_>, flag: &Flag<'_>, hir_id: HirId, bound: &'static str) {
        let msg = format!("this parameter takes `impl {bound}`; pass the colorspace value");
        match &flag.repl {
            Replacement::Inner { text, .. } => span_lint_and_sugg(
                cx,
                MANUAL_COLOR_ERASURE,
                flag.span,
                msg,
                "pass the colorspace value directly",
                text.clone(),
                Applicability::MachineApplicable,
            ),
            Replacement::Ctor {
                replace,
                ty_name,
                fn_name,
                ty_did,
                ..
            } => {
                let name = Symbol::intern(ty_name);
                let status = bare_status(
                    cx,
                    hir_id,
                    flag.span.lo(),
                    name,
                    Namespace::TypeNS,
                    Some(*ty_did),
                );
                match status {
                    Bare::Same => {
                        span_lint_and_then(cx, MANUAL_COLOR_ERASURE, flag.span, msg, |diag| {
                            let new_fn = format!("{ty_name}::{fn_name}");
                            let label = format!("use `{new_fn}`");
                            if flag.erased.is_empty() {
                                diag.span_suggestion(
                                    *replace,
                                    label,
                                    new_fn,
                                    Applicability::MachineApplicable,
                                );
                            } else {
                                let mut parts = vec![(*replace, new_fn)];
                                parts.extend(flag.erased.iter().map(|&span| (span, String::new())));
                                diag.multipart_suggestion(
                                    label,
                                    parts,
                                    Applicability::MachineApplicable,
                                );
                            }
                        });
                    }
                    Bare::Free | Bare::Conflict | Bare::Unknown => {
                        let use_path = format!("waterui::color::{ty_name}");
                        let Some((point, before, after)) = use_insertion(cx, hir_id, &use_path)
                        else {
                            return;
                        };
                        let (label, use_text, new_fn) = if matches!(status, Bare::Free) {
                            (
                                format!("import `{use_path}` and pass the colorspace value"),
                                format!("{before}use {use_path};{after}"),
                                format!("{ty_name}::{fn_name}"),
                            )
                        } else {
                            let alias = format!("color_{ty_name}");
                            (
                                format!(
                                    "`{ty_name}` resolves to a different item here — import under an alias"
                                ),
                                format!("{before}use {use_path} as {alias};{after}"),
                                format!("{alias}::{fn_name}"),
                            )
                        };
                        span_lint_and_then(cx, MANUAL_COLOR_ERASURE, flag.span, msg, |diag| {
                            let mut parts = vec![(point, use_text), (*replace, new_fn)];
                            parts.extend(flag.erased.iter().map(|&span| (span, String::new())));
                            diag.multipart_suggestion(
                                label,
                                parts,
                                match status {
                                    Bare::Free => Applicability::MachineApplicable,
                                    _ => Applicability::MaybeIncorrect,
                                },
                            );
                        });
                    }
                }
            }
        }
    }
}

/// Collects the uses of a `let` local inside its body — nested closure bodies
/// are uses too, each paired with the `TypeckResults` of the body that
/// contains it.
struct LocalUses<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    pat: HirId,
    typeck: &'tcx TypeckResults<'tcx>,
    uses: Vec<(&'tcx Expr<'tcx>, &'tcx TypeckResults<'tcx>)>,
}

impl<'tcx> Visitor<'tcx> for LocalUses<'_, 'tcx> {
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
        if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
            && path.res == Res::Local(self.pat)
        {
            self.uses.push((expr, self.typeck));
        }
        intravisit::walk_expr(self, expr);
    }
}

impl<'tcx> LateLintPass<'tcx> for ManualColorErasure {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        // `Into::into(..)`/`Color::from(..)`/`Color::new(..)`/`e.into()` — a
        // conversion's `self` parameter carries `Self: Into<Color>`-shaped
        // bounds, but its type is `Self` pinned by the call, not an
        // `impl Into<Color>` position an argument can be rewritten at; the
        // wrapper itself is flagged where it appears as an argument.
        if self.is_color_wrapper(cx, expr) || self.is_into(cx, expr) {
            return;
        }
        let Some((_, per_arg)) = call_arg_bounds(cx, expr, &[COLOR_PARAM_BOUNDS]) else {
            return;
        };
        let Some((_, per_arg_all)) = call_arg_all_bounds(cx, expr) else {
            return;
        };
        for ((arg, targets), all) in call_args(expr).into_iter().zip(per_arg).zip(per_arg_all) {
            let color: Vec<BoundTarget<'tcx>> = targets
                .into_iter()
                .filter(|target| self.color_bound(cx, target))
                .collect();
            // Every trait bound on the parameter must be one the color table
            // knows — the rewrite is verified against the color bounds only,
            // so an extra bound (`impl Into<Color> + Debug`) would go
            // unchecked and the fixed code could fail to compile.
            if !color.is_empty() && all.iter().all(|target| self.known_bound(cx, target)) {
                self.check_arg(cx, arg, &color);
            }
        }
    }
}
