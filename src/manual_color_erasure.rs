use std::cell::OnceCell;

use clippy_utils::diagnostics::{span_lint_and_sugg, span_lint_and_then};
use rustc_data_structures::fx::FxHashSet;
use rustc_errors::Applicability;
use rustc_hir::def::{Namespace, Res};
use rustc_hir::{Expr, ExprKind, HirId, Node, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::Symbol;

use crate::anyview::peel;
use crate::color::{self, COLOR_PARAM_BOUNDS, Defs, Flag, Replacement};
use crate::imports::{Bare, bare_status, use_insertion};
use crate::param_bounds::{BoundTarget, call_arg_all_bounds, call_arg_bounds, call_args};

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

/// The lint pass. `flagged` deduplicates `let`-initializers reached through
/// more than one argument position.
#[derive(Default)]
pub struct ManualColorErasure {
    defs: OnceCell<Option<Defs>>,
    flagged: FxHashSet<HirId>,
}

impl_lint_pass!(ManualColorErasure => [MANUAL_COLOR_ERASURE]);

impl ManualColorErasure {
    /// The crate-local `DefId`s, resolved once.
    fn defs(&self, cx: &LateContext<'_>) -> Option<Defs> {
        *self.defs.get_or_init(|| color::defs(cx))
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
            self.check_local(cx, pat, color::bound_name(cx, targets));
            return;
        }
        let Some(defs) = self.defs(cx) else {
            return;
        };
        let Some((flag, ty)) = color::flagged(cx, defs, arg) else {
            return;
        };
        if !color::satisfies(cx, ty, targets) {
            return;
        }
        self.report(cx, &flag, arg.hir_id, color::bound_name(cx, targets));
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
        let Some(defs) = self.defs(cx) else {
            return;
        };
        let Some((flag, ty)) = color::flagged(cx, defs, init) else {
            return;
        };
        if !color::only_color_uses(cx, pat, ty) {
            return;
        }
        self.flagged.insert(init.hir_id);
        self.report(cx, &flag, init.hir_id, bound);
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
        if color::is_color_wrapper(cx, expr) || color::is_into(cx, expr) {
            return;
        }
        let Some((_, per_arg)) = call_arg_bounds(cx, expr, &[COLOR_PARAM_BOUNDS]) else {
            return;
        };
        let Some((_, per_arg_all)) = call_arg_all_bounds(cx, expr) else {
            return;
        };
        for ((arg, targets), all) in call_args(expr).into_iter().zip(per_arg).zip(per_arg_all) {
            let color_targets: Vec<BoundTarget<'tcx>> = targets
                .into_iter()
                .filter(|target| color::color_bound(cx, target))
                .collect();
            // Every trait bound on the parameter must be one the color table
            // knows — the rewrite is verified against the color bounds only,
            // so an extra bound (`impl Into<Color> + Debug`) would go
            // unchecked and the fixed code could fail to compile.
            if !color_targets.is_empty() && all.iter().all(|target| color::known_bound(cx, target))
            {
                self.check_arg(cx, arg, &color_targets);
            }
        }
    }
}
