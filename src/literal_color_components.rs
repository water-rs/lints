use clippy_utils::consts::{ConstEvalCtxt, Constant};
use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::res::MaybeQPath;
use clippy_utils::source::snippet_opt;
use rustc_data_structures::fx::FxHashMap;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind, QPath, UnOp};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::{FloatTy, Ty, TyCtxt, TyKind};
use rustc_session::impl_lint_pass;
use rustc_span::Span;

use crate::color;
use crate::def_path::def_path_eq;
use crate::param_bounds::implemented_trait_item;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `Srgb::new`/`Srgb::new_u8`/`Srgb::from_u32` and
    /// `Color::srgb`/`Color::srgb_f32`/`Color::srgb_u32` calls whose
    /// components are all numeric literals.
    ///
    /// ### Why is this bad?
    ///
    /// `Srgb::new_u8(244, 67, 54)` names a color nobody can read back;
    /// `Srgb::from_hex("#F44336")` spells the same color the way designers
    /// and tools do, and it is a `const fn`, so the same literal can
    /// initialize a constant. Components that are variables or expressions
    /// (`Srgb::new(r, g, b)`, `Srgb::new(t, 0.0, 1.0 - t)`) are the only way
    /// to build a computed color and are never reported.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let accent = Srgb::new(0.96, 0.26, 0.21);
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let accent = Srgb::from_hex("#F54236");
    /// ```
    pub LITERAL_COLOR_COMPONENTS,
    style,
    "a color built from numeric-literal components; write it as a hex literal"
}

/// How a flagged constructor's components are written.
enum Kind {
    /// `(f32, f32, f32)` — `Srgb::new`, `Color::srgb_f32`.
    F32,
    /// `(u8, u8, u8)` — `Srgb::new_u8`, `Color::srgb`.
    U8,
    /// A packed `0xRRGGBB` `u32` — `Srgb::from_u32`, `Color::srgb_u32`.
    U32,
}

/// A color constructor the lint rewrites to the hex spelling.
struct Ctor {
    /// The constructor's `LateContext::get_def_path` segments — the defining
    /// crate's path (`waterui_graphics::color::srgb::Srgb::new`), never a
    /// facade re-export.
    def_path: &'static [&'static str],
    /// The component encoding.
    kind: Kind,
    /// Whether the constructor produces a `Color` — those calls defer to
    /// `manual_color_erasure` wherever it would rewrite them.
    color: bool,
}

const CTORS: &[Ctor] = &[
    Ctor {
        def_path: &["waterui_graphics", "color", "srgb", "Srgb", "new"],
        kind: Kind::F32,
        color: false,
    },
    Ctor {
        def_path: &["waterui_graphics", "color", "srgb", "Srgb", "new_u8"],
        kind: Kind::U8,
        color: false,
    },
    Ctor {
        def_path: &["waterui_graphics", "color", "srgb", "Srgb", "from_u32"],
        kind: Kind::U32,
        color: false,
    },
    Ctor {
        def_path: &["waterui_graphics", "color", "Color", "srgb"],
        kind: Kind::U8,
        color: true,
    },
    Ctor {
        def_path: &["waterui_graphics", "color", "Color", "srgb_f32"],
        kind: Kind::F32,
        color: true,
    },
    Ctor {
        def_path: &["waterui_graphics", "color", "Color", "srgb_u32"],
        kind: Kind::U32,
        color: true,
    },
];

/// A component expression's evaluated value.
enum Num<'tcx> {
    /// An integer's (or char's/bool's) bit pattern at its type — the type
    /// carries the width and signedness that interpret it.
    Int(u128, Ty<'tcx>),
    /// A float, widened to `f64` — `f32` values keep their `f32` rounding.
    Float(f64),
}

/// The bit width of an integer type — the pointer types' width comes from
/// the data layout.
fn int_bits(tcx: TyCtxt<'_>, ty: Ty<'_>) -> Option<u64> {
    match ty.kind() {
        TyKind::Int(ity) => Some(
            ity.bit_width()
                .unwrap_or_else(|| tcx.data_layout.pointer_size().bits()),
        ),
        TyKind::Uint(uty) => Some(
            uty.bit_width()
                .unwrap_or_else(|| tcx.data_layout.pointer_size().bits()),
        ),
        _ => None,
    }
}

/// The mask of a `w`-bit value.
fn mask(w: u64) -> u128 {
    u128::MAX >> (128 - w)
}

/// `v` sign-extended from `w` bits to `i128`.
fn sext(v: u128, w: u64) -> i128 {
    ((v << (128 - w)) as i128) >> (128 - w)
}

/// `f` as a `w`-bit signed integer's bit pattern — `as` saturates
/// out-of-range values, truncates toward zero, and maps NaN to zero.
fn sat_signed(f: f64, w: u64) -> u128 {
    let min = i128::MIN >> (128 - w);
    let max = i128::MAX >> (128 - w);
    let f = if f.is_nan() {
        0.0
    } else {
        f.clamp(min as f64, max as f64)
    };
    (f as i128) as u128
}

/// `f` as a `w`-bit unsigned integer — the same `as` semantics.
fn sat_unsigned(f: f64, w: u64) -> u128 {
    let max = u128::MAX >> (128 - w);
    let f = if f.is_nan() {
        0.0
    } else {
        f.clamp(0.0, max as f64)
    };
    (f as u128) & max
}

/// The `u128` bit pattern of an integer `v` at `ty` — signed integers
/// sign-extend.
fn int_val(tcx: TyCtxt<'_>, v: u128, ty: Ty<'_>) -> Option<u128> {
    Some(match ty.kind() {
        TyKind::Int(_) => sext(v, int_bits(tcx, ty)?) as u128,
        TyKind::Uint(_) | TyKind::Bool | TyKind::Char => v,
        _ => return None,
    })
}

/// `v` widened to `f64` — a signed integer's bit pattern reads as its
/// signed value.
fn as_f64(tcx: TyCtxt<'_>, v: Num<'_>) -> Option<f64> {
    Some(match v {
        Num::Float(f) => f,
        Num::Int(v, ty) => match ty.kind() {
            TyKind::Int(_) => sext(v, int_bits(tcx, ty)?) as f64,
            _ => v as f64,
        },
    })
}

/// `-(v)` as const evaluation computes it — a float negates, a signed
/// integer two's-complements at its width.
fn negate<'tcx>(tcx: TyCtxt<'tcx>, v: Num<'tcx>) -> Option<Num<'tcx>> {
    Some(match v {
        Num::Float(f) => Num::Float(-f),
        Num::Int(v, ty) if matches!(ty.kind(), TyKind::Int(_)) => {
            let w = int_bits(tcx, ty)?;
            Num::Int(((-sext(v, w)) as u128) & mask(w), ty)
        }
        Num::Int(..) => return None,
    })
}

/// `v as to` — const evaluation's cast: int-to-int truncates the bit
/// pattern (sign-extending a signed source first), float-to-int saturates,
/// anything-to-float converts.
fn cast<'tcx>(tcx: TyCtxt<'tcx>, v: Num<'tcx>, to: Ty<'tcx>) -> Option<Num<'tcx>> {
    Some(match to.kind() {
        TyKind::Int(_) => {
            let w = int_bits(tcx, to)?;
            let v = match v {
                Num::Int(v, from) => int_val(tcx, v, from)?,
                Num::Float(f) => sat_signed(f, w),
            };
            Num::Int(v & mask(w), to)
        }
        TyKind::Uint(_) => {
            let w = int_bits(tcx, to)?;
            let v = match v {
                Num::Int(v, from) => int_val(tcx, v, from)?,
                Num::Float(f) => sat_unsigned(f, w),
            };
            Num::Int(v & mask(w), to)
        }
        TyKind::Float(fty) => Num::Float(match fty {
            FloatTy::F32 => f64::from(as_f64(tcx, v)? as f32),
            FloatTy::F64 => as_f64(tcx, v)?,
            _ => return None,
        }),
        _ => return None,
    })
}

/// The literal `expr` reduces to and the value const evaluation gives it —
/// `DropTemps`, `as` casts, and unary minus are applied to the inner
/// literal's value, so a `300u16 as u8` component is `44` and `-1i8 as u8`
/// is `255`. Anything else — a `const` name, a call, a field, a binary op
/// — is not a literal and stays silent. The returned expression is the
/// literal itself; its snippet carries the written base (`0x` vs decimal)
/// for the packed `u32` rows.
fn eval<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<(&'tcx Expr<'tcx>, Num<'tcx>)> {
    let typeck = cx.typeck_results();
    match expr.kind {
        ExprKind::DropTemps(inner) => eval(cx, inner),
        ExprKind::Cast(inner, _) => {
            let (lit, v) = eval(cx, inner)?;
            Some((lit, cast(cx.tcx, v, typeck.expr_ty(expr))?))
        }
        ExprKind::Unary(UnOp::Neg, inner) => {
            let (lit, v) = eval(cx, inner)?;
            Some((lit, negate(cx.tcx, v)?))
        }
        ExprKind::Lit(_) => Some((expr, lit_num(cx, expr)?)),
        _ => None,
    }
}

/// The literal's own value, evaluated in place — `ConstEvalCtxt` reads the
/// literal's type, so suffixes and unsuffixed floats land on the right
/// `Constant` variant.
fn lit_num<'tcx>(cx: &LateContext<'tcx>, expr: &Expr<'tcx>) -> Option<Num<'tcx>> {
    let ty = cx.typeck_results().expr_ty(expr);
    match ConstEvalCtxt::new(cx).eval(expr)? {
        Constant::Int(v) => Some(Num::Int(v, ty)),
        Constant::Char(c) => Some(Num::Int(u128::from(c), ty)),
        Constant::Bool(b) => Some(Num::Int(u128::from(b), ty)),
        Constant::F32(f) => Some(Num::Float(f64::from(f))),
        Constant::F64(f) => Some(Num::Float(f)),
        _ => None,
    }
}

/// The `f32` a float-kind component literal holds.
fn float<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Option<f32> {
    Some(as_f64(cx.tcx, eval(cx, expr)?.1)? as f32)
}

/// The `u8` an int-kind component literal holds — const evaluation has
/// already applied the casts' truncation, so `300u16 as u8` arrives as `44`.
fn int_u8<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Option<u8> {
    match eval(cx, expr)?.1 {
        Num::Int(v, _) => u8::try_from(v).ok(),
        _ => None,
    }
}

/// Whether the literal was written decimal — a `0x`/`0o`/`0b` packed value
/// is already readable and stays silent.
fn decimal(cx: &LateContext<'_>, lit: &Expr<'_>) -> bool {
    snippet_opt(cx.sess(), lit.span).is_some_and(|text| {
        !(text.starts_with("0x") || text.starts_with("0o") || text.starts_with("0b"))
    })
}

/// The `u8` `v` rounds to and whether it already sits on the `n / 255`
/// grid — i.e. `Srgb::from_hex` stores bit-identical `f32`s (the
/// constructor computes `n as f32 / 255.0`), making the rewrite
/// `MachineApplicable`. An off-grid component like `0.96` keeps the
/// nearest hex but downgrades to `MaybeIncorrect`.
fn to_u8(v: f32) -> (u8, bool) {
    let n = (v * 255.0).round().clamp(0.0, 255.0) as u8;
    (n, v == n as f32 / 255.0)
}

/// The stored components' `f32` bit patterns of a `u8` triple —
/// `new_u8`/`from_u32` store `n as f32 / 255.0`, so this key equals the one
/// `Srgb::new` produces for the same color.
fn u8_key(rgb: [u8; 3]) -> [u32; 3] {
    rgb.map(|n| (f32::from(n) / 255.0).to_bits())
}

/// The call's literal components — the key is the stored components' `f32`
/// bit patterns, so two spellings of the same color count together while
/// distinct colors that merely round to the same hex do not. `None` as
/// soon as one component is a variable, field, call, or any other
/// non-literal expression.
fn components<'tcx>(
    cx: &LateContext<'tcx>,
    kind: &Kind,
    args: &'tcx [Expr<'tcx>],
) -> Option<([u32; 3], [u8; 3], bool)> {
    match kind {
        Kind::F32 => {
            let [r, g, b] = args else {
                return None;
            };
            let (fr, fg, fb) = (float(cx, r)?, float(cx, g)?, float(cx, b)?);
            let (r, re) = to_u8(fr);
            let (g, ge) = to_u8(fg);
            let (b, be) = to_u8(fb);
            Some((
                [fr.to_bits(), fg.to_bits(), fb.to_bits()],
                [r, g, b],
                re && ge && be,
            ))
        }
        Kind::U8 => {
            let [r, g, b] = args else {
                return None;
            };
            let rgb = [int_u8(cx, r)?, int_u8(cx, g)?, int_u8(cx, b)?];
            Some((u8_key(rgb), rgb, true))
        }
        Kind::U32 => {
            let [arg] = args else {
                return None;
            };
            let (lit, Num::Int(v, _)) = eval(cx, arg)? else {
                return None;
            };
            if !decimal(cx, lit) {
                return None;
            }
            let v = u32::try_from(v).ok()?;
            let rgb = [
                ((v >> 16) & 0xFF) as u8,
                ((v >> 8) & 0xFF) as u8,
                (v & 0xFF) as u8,
            ];
            Some((u8_key(rgb), rgb, true))
        }
    }
}

/// The callee path's final segment — `new` in `Srgb::new(..)` — whatever
/// qualifier the source wrote.
fn last_segment(func: &Expr<'_>) -> Option<Span> {
    let ExprKind::Path(qpath) = &func.kind else {
        return None;
    };
    match qpath {
        QPath::Resolved(_, path) => path.segments.last().map(|seg| seg.ident.span),
        QPath::TypeRelative(_, seg) => Some(seg.ident.span),
    }
}

/// A flagged call queued for `check_crate_post` — the "appears more than
/// once" help needs the crate-wide count before any diagnostic goes out.
struct Flag {
    /// The whole call — the diagnostic's span.
    span: Span,
    /// The callee's final segment — replaced with `from_hex`/`srgb_hex`.
    seg: Span,
    /// The argument list — replaced with `("#RRGGBB")`.
    args: Span,
    /// The hex constructor's name.
    fix: &'static str,
    /// The stored components' `f32` bit patterns — the occurrence-count key.
    key: [u32; 3],
    /// `"#RRGGBB"` — the suggested literal.
    hex: String,
    /// The message — `… write the color as `Srgb::from_hex("#RRGGBB")``.
    msg: String,
    /// The rounded components for the inexact note.
    rgb: [u8; 3],
    /// Whether the rewrite stores identical components.
    exact: bool,
}

/// The lint pass; `flags` collects every call so `check_crate_post` can
/// count how often each literal color appears.
#[derive(Default)]
pub struct LiteralColorComponents {
    flags: Vec<Flag>,
}

impl_lint_pass!(LiteralColorComponents => [LITERAL_COLOR_COMPONENTS]);

impl<'tcx> LateLintPass<'tcx> for LiteralColorComponents {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let ExprKind::Call(func, args) = expr.kind else {
            return;
        };
        let Some(did) = func.res(cx).opt_def_id() else {
            return;
        };
        let Some(ctor) = CTORS
            .iter()
            .find(|ctor| def_path_eq(cx, implemented_trait_item(cx.tcx, did), ctor.def_path))
        else {
            return;
        };
        let Some((key, rgb, exact)) = components(cx, &ctor.kind, args) else {
            return;
        };
        // A `Color::*` constructor `manual_color_erasure` would rewrite here
        // is that lint's diagnostic, not this one's.
        if ctor.color && color::would_rewrite(cx, cx.typeck_results(), expr) {
            return;
        }
        let Some(seg) = last_segment(func) else {
            return;
        };
        let (ty, fix) = if ctor.color {
            ("Color", "srgb_hex")
        } else {
            ("Srgb", "from_hex")
        };
        let hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
        self.flags.push(Flag {
            span: expr.span,
            seg,
            args: expr.span.with_lo(func.span.hi()),
            fix,
            key,
            msg: format!(
                "color components written as literals; write the color as `{ty}::{fix}(\"{hex}\")`"
            ),
            hex,
            rgb,
            exact,
        });
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        let flags = std::mem::take(&mut self.flags);
        let mut counts: FxHashMap<[u32; 3], usize> = FxHashMap::default();
        for flag in &flags {
            *counts.entry(flag.key).or_default() += 1;
        }
        for flag in &flags {
            span_lint_and_then(
                cx,
                LITERAL_COLOR_COMPONENTS,
                flag.span,
                flag.msg.clone(),
                |diag| {
                    diag.multipart_suggestion(
                        "write it as a hex literal",
                        vec![
                            (flag.seg, flag.fix.to_owned()),
                            (flag.args, format!("(\"{}\")", flag.hex)),
                        ],
                        if flag.exact {
                            Applicability::MachineApplicable
                        } else {
                            Applicability::MaybeIncorrect
                        },
                    );
                    if !flag.exact {
                        let [r, g, b] = flag.rgb;
                        diag.note(format!("the components round to ({r}, {g}, {b})"));
                    }
                    let n = counts[&flag.key];
                    if n > 1 {
                        diag.help(format!(
                            "this color is built from literals {n} times in this crate; \
                             name it once — `const NAME: Srgb = Srgb::from_hex(\"{}\")`",
                            flag.hex
                        ));
                    }
                },
            );
        }
    }
}
