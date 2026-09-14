use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{
    Expr, ExprKind, FnDecl, FnRetTy, GenericBound, ImplItemKind, ItemKind, Node, TraitItemKind,
    TyKind, UnOp,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::declare_lint_pass;

use crate::def_path::def_path_eq;
use crate::param_bounds::{call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags literal-valued color constructors — `Color::srgb`/`srgb_f32`/
    /// `srgb_hex`/`srgb_u32`/`p3`/`oklch` and `Srgb::new`/`new_u8`/
    /// `from_hex`/`from_u32` — and literal font-size setters —
    /// `Text::size`, `Font::size`, `StyledStr::size` — inside a function
    /// returning `impl View` or a `View::body` method. Every argument of the
    /// call must be a literal for it to be flagged.
    ///
    /// ### Why is this bad?
    ///
    /// Theme tokens (`ForegroundColor`, `AccentColor`, …) and semantic fonts
    /// (`.font(Title)`, `.font(Body)`, …) adapt to the color scheme and the
    /// type scale; a literal freezes one appearance into the view and breaks
    /// dark mode and Dynamic Type. Allow-by-default because a literal brand
    /// color is legitimate — lift those to named constants so the lint stays
    /// quiet and the brand palette lives in one place.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// fn card() -> impl View {
    ///     text("Sale").foreground(Color::srgb_hex("#ff8800")).size(18.0)
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// fn card() -> impl View {
    ///     text("Sale").foreground(AccentColor).font(Title)
    /// }
    /// ```
    pub HARDCODED_THEME_VALUE,
    pedantic,
    "a literal color or font size in view code instead of a theme token"
}

declare_lint_pass!(HardcodedThemeValue => [HARDCODED_THEME_VALUE]);

/// Literal color constructors — `waterui-graphics`' `impl Color`
/// (`color/mod.rs`) and `impl Srgb` (`color/srgb.rs`).
const COLOR_CTORS: &[&[&str]] = &[
    &["waterui_graphics", "color", "Color", "srgb"],
    &["waterui_graphics", "color", "Color", "srgb_f32"],
    &["waterui_graphics", "color", "Color", "srgb_hex"],
    &["waterui_graphics", "color", "Color", "srgb_u32"],
    &["waterui_graphics", "color", "Color", "p3"],
    &["waterui_graphics", "color", "Color", "oklch"],
    &["waterui_graphics", "color", "srgb", "Srgb", "new"],
    &["waterui_graphics", "color", "srgb", "Srgb", "new_u8"],
    &["waterui_graphics", "color", "srgb", "Srgb", "from_hex"],
    &["waterui_graphics", "color", "srgb", "Srgb", "from_u32"],
];

/// Literal font-size setters — `waterui-text`'s `impl Text` (`text.rs`),
/// `impl Font` (`font.rs`), and `impl StyledStr` (`styled.rs`).
const SIZE_SETTERS: &[&[&str]] = &[
    &["waterui_text", "text", "Text", "size"],
    &["waterui_text", "font", "Font", "size"],
    &["waterui_text", "styled", "StyledStr", "size"],
];

/// `View::body` — an impl item whose trait item is this method is a view
/// scope even when the impl spells a concrete return type.
const VIEW_BODY: &[&str] = &["waterui_core", "ui", "view", "View", "body"];

const COLOR_MSG: &str = "a literal color in view code does not adapt to the color scheme";
const COLOR_HELP: &str = "use a theme token — `ForegroundColor`, `BackgroundColor`, \
    `SurfaceColor`, `AccentColor`, `MutedForegroundColor` from `waterui::graphics::color` — \
    or lift a brand color to a named constant";
const SIZE_MSG: &str = "a literal font size in view code does not follow the typography scale";
const SIZE_HELP: &str = "use a semantic font — `.font(Title)`, `.font(Body)`, `.font(Caption)`, \
    `.font(Headline)`, `.font(Footnote)` from `waterui::text::font`";

/// Which of the two diagnostics a matched callee reports.
enum Kind {
    Color,
    Size,
}

/// Whether `expr` is a literal: `ExprKind::Lit` under parens/`DropTemps`
/// wrappers, with unary minus allowed.
fn is_literal(expr: &Expr<'_>) -> bool {
    match expr.kind {
        ExprKind::Lit(_) => true,
        ExprKind::Unary(UnOp::Neg, inner) | ExprKind::DropTemps(inner) => is_literal(inner),
        _ => false,
    }
}

/// Whether `decl` declares `-> impl` with a bound on
/// `waterui_core::ui::view::View`.
fn returns_impl_view(cx: &LateContext<'_>, decl: &FnDecl<'_>) -> bool {
    let FnRetTy::Return(ty) = decl.output else {
        return false;
    };
    match ty.kind {
        TyKind::OpaqueDef(opaque) => opaque.bounds.iter().any(|bound| match bound {
            GenericBound::Trait(poly) => match poly.trait_ref.path.res {
                Res::Def(DefKind::Trait, did) => {
                    def_path_eq(cx, did, &["waterui_core", "ui", "view", "View"])
                }
                _ => false,
            },
            _ => false,
        }),
        _ => false,
    }
}

/// Whether `expr` sits inside view code: the nearest enclosing item is a
/// function or method declaring `-> impl View`, or the `body` method of an
/// `impl View for _`. Nested closures count as their enclosing item; a
/// non-function item (a `const` initializer, a nested `fn` of its own)
/// closes the scope.
fn in_view_code(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        match node {
            Node::Item(item) => {
                return match item.kind {
                    ItemKind::Fn { sig, .. } => returns_impl_view(cx, sig.decl),
                    _ => false,
                };
            }
            Node::ImplItem(item) => {
                return match item.kind {
                    ImplItemKind::Fn(sig, _) => {
                        returns_impl_view(cx, sig.decl)
                            || def_path_eq(
                                cx,
                                implemented_trait_item(cx.tcx, item.owner_id.to_def_id()),
                                VIEW_BODY,
                            )
                    }
                    _ => false,
                };
            }
            Node::TraitItem(item) => {
                return match item.kind {
                    TraitItemKind::Fn(sig, _) => returns_impl_view(cx, sig.decl),
                    _ => false,
                };
            }
            Node::ForeignItem(_) => return false,
            _ => {}
        }
    }
    false
}

impl<'tcx> LateLintPass<'tcx> for HardcodedThemeValue {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        // The receiver of a method call is the object being styled, not an
        // argument — only the call arguments must be literals.
        let args: &[Expr<'tcx>] = match expr.kind {
            ExprKind::Call(_, args) => args,
            ExprKind::MethodCall(_, _, args, _) => args,
            _ => return,
        };
        let Some(did) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        let kind = if COLOR_CTORS.iter().any(|path| def_path_eq(cx, did, path)) {
            Kind::Color
        } else if SIZE_SETTERS.iter().any(|path| def_path_eq(cx, did, path)) {
            Kind::Size
        } else {
            return;
        };
        if !args.iter().all(|arg| is_literal(arg)) || !in_view_code(cx, expr) {
            return;
        }
        let (msg, help) = match kind {
            Kind::Color => (COLOR_MSG, COLOR_HELP),
            Kind::Size => (SIZE_MSG, SIZE_HELP),
        };
        // A method call's span covers its receiver; the offense is the
        // `.size(literal)` tail.
        let span = match expr.kind {
            ExprKind::MethodCall(_, receiver, ..) => expr.span.with_lo(receiver.span.hi()),
            _ => expr.span,
        };
        span_lint_and_help(cx, HARDCODED_THEME_VALUE, span, msg, None, help);
    }
}
