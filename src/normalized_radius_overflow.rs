use clippy_utils::consts::{ConstEvalCtxt, Constant};
use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::res::MaybeQPath;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `RoundedRectangle::new(radius)` and `UnevenRoundedRectangle::new(..)`
    /// calls whose constant-evaluated radius is greater than `0.5`.
    ///
    /// ### Why is this bad?
    ///
    /// These constructors take a normalized radius: a fraction of the shape's
    /// shorter side, where `0.5` is fully rounded and anything above it
    /// saturates. A value like `12.0` reads as a point size but silently
    /// clamps, so the shape is only correct when it happens to be exactly that
    /// tall.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// RoundedRectangle::new(12.0)
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// FixedRoundedRectangle::new(12.0)
    /// ```
    pub NORMALIZED_RADIUS_OVERFLOW,
    correctness,
    "a normalized corner radius above 0.5 was meant in points"
}

declare_lint_pass!(NormalizedRadiusOverflow => [NORMALIZED_RADIUS_OVERFLOW]);

/// A `waterui_shape` constructor taking normalized corner radii, paired with
/// the absolute-radius counterpart a point value was probably meant for.
struct RadiusCtor {
    /// `LateContext::get_def_path` segments of the `<Type>::new` constructor.
    def_path: [&'static str; 3],
    /// The `waterui_shape` type that takes the radius in points instead.
    fixed_ty: &'static str,
}

const NORMALIZED_RADIUS_CTORS: &[RadiusCtor] = &[
    RadiusCtor {
        def_path: ["waterui_shape", "RoundedRectangle", "new"],
        fixed_ty: "FixedRoundedRectangle",
    },
    RadiusCtor {
        def_path: ["waterui_shape", "UnevenRoundedRectangle", "new"],
        fixed_ty: "FixedUnevenRoundedRectangle",
    },
];

fn normalized_radius_ctor(cx: &LateContext<'_>, def_id: DefId) -> Option<&'static RadiusCtor> {
    let def_path = cx.get_def_path(def_id);
    NORMALIZED_RADIUS_CTORS.iter().find(|ctor| {
        def_path
            .iter()
            .map(|segment| segment.as_str())
            .eq(ctor.def_path.iter().copied())
    })
}

impl<'tcx> LateLintPass<'tcx> for NormalizedRadiusOverflow {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if let ExprKind::Call(func, args) = expr.kind
            && let Res::Def(DefKind::AssocFn, def_id) = func.res(cx)
            && let Some(ctor) = normalized_radius_ctor(cx, def_id)
        {
            let consts = ConstEvalCtxt::new(cx);
            for arg in args {
                if let Some(Constant::F32(radius)) = consts.eval(arg)
                    && radius > 0.5
                {
                    span_lint_and_help(
                        cx,
                        NORMALIZED_RADIUS_OVERFLOW,
                        arg.span,
                        "the radius is a fraction of the shorter side (0.0–0.5), not points",
                        None,
                        format!(
                            "use `{}::new(..)` for a corner radius in points",
                            ctor.fixed_ty
                        ),
                    );
                }
            }
        }
    }
}
