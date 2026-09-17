use waterui::prelude::*;
use waterui::shape::{Circle, ShapeExt};

fn main() {
    // Fires: `Color::srgb_f32` — the parameter already takes the `Srgb`.
    let _ = text("a").foreground(Color::srgb_f32(0.9, 0.2, 0.35));
    // Fires: `Color::srgb` — the `Srgb::new_u8` triple.
    let _ = text("b").background(Color::srgb(230, 51, 89));
    // Fires: `Color::srgb_hex` — `Color::srgb_hex` is `Color::new(Srgb::from_hex(h))`.
    let _ = Circle.fill(Color::srgb_hex("#3B82F6"));
    // Fires: `Color::srgb_u32` — `Srgb::from_u32`.
    let _ = text("c").background(Color::srgb_u32(0x10B981));
    // Fires: `Color::p3` — `P3::new`.
    let _ = text("d").border(Color::p3(0.1, 0.5, 0.9), 1.0);
    // Fires: `Into::into` — a bare `.into()` cannot infer its target at an
    // `impl Into<Color>` parameter (every `Srgb` also has `Into<Srgb>` and
    // `Into<ResolvedColor>`), so the qualified form is the only one that
    // type-checks; the `Srgb` already satisfies `Into<Color>`.
    let _ = Circle.fill(<Srgb as Into<Color>>::into(Srgb::from_hex("#4CAF50")));
    // Fires: `Into::into` on a `P3` — same erasure, other colorspace.
    let _ = text("e").foreground(<P3 as Into<Color>>::into(P3::new(0.2, 0.3, 0.4)));
    // Fires: `Color::from` — the argument is already a colorspace value.
    let _ = text("f").foreground(Color::from(Srgb::new(0.1, 0.5, 0.9)));
    // Fires: `Color::new` — the boxing constructor the `From` impl calls.
    let _ = Border::new(Color::new(Srgb::new_u8(255, 0, 0)), 2.0);
    // Fires: `Color::from` on a `WithOpacity` — still a colorspace value.
    let _ = text("g").foreground(Color::from(Srgb::new(0.1, 0.2, 0.3).with_opacity(0.4)));
    // Fires: `Color::new` on a theme token — `Resolvable<Resolved = ResolvedColor>` too.
    let _ = text("h").foreground(Color::new(theme_color::Accent));
    // Fires: `with_opacity` exists on `Srgb` — the chain survives the rewrite.
    let _ = text("i").foreground(Color::srgb_f32(0.1, 0.2, 0.3).with_opacity(0.5));
    // Fires: `Into::into` on the `Color` receiver — a bare `.into()` cannot
    // infer its target (`Color` also has `Into<Background>` and
    // `Into<WindowBackgroundInput>`), so only the qualified form compiles;
    // the rewrite drops the whole conversion.
    let _ = text("i").foreground(<Color as Into<Color>>::into(Color::srgb(9, 20, 35)));
    // Fires: `Avatar::ring` is an `Into<Color>` position too.
    let _ = avatar("Radia Perlman").ring(Color::srgb(10, 20, 30), 2.0);
    // Fires: the `let` form — the diagnostic lands on the constructor.
    let inline = Color::srgb_f32(0.7, 0.1, 0.2);
    let _ = text("j").foreground(inline);

    // Silent: the `: Color` annotation makes `.into()` load-bearing —
    // `let pinned: Color = Srgb::from_hex(..)` would not type-check.
    let pinned: Color = Srgb::from_hex("#224466").into();
    let _ = text("k").foreground(pinned);
    // Silent: `with_headroom` exists on `Color` but not on `Srgb` — the chain
    // would not compile on the rewritten value.
    let _ = text("k").foreground(Color::srgb_f32(0.1, 0.2, 0.3).with_headroom(1.5));
    // Silent: `0.5f64` is suffixed — it cannot re-infer to
    // `Srgb::with_opacity`'s `f32`, so the chain needs the `Color`
    // (`IntoSignalF32` accepts `f64`).
    let _ = text("k").foreground(Color::srgb_f32(0.1, 0.2, 0.3).with_opacity(0.5f64));
    // Silent: `paint`'s parameter carries a `Debug` bound outside the color
    // table — the rewrite is never verified against it.
    paint(Color::srgb_f32(0.1, 0.2, 0.3));
    // Silent: a `Color`-typed binding is used as a `Color`.
    let c: Color = Color::srgb_f32(0.2, 0.3, 0.4);
    let _ = c.resolve(&Environment::new());
    // Silent: `mixed` is also used where a `Color` is required — rewriting the
    // initializer would change that use's type.
    let mixed = Color::srgb_f32(0.2, 0.3, 0.4);
    let _ = wants_color(mixed.clone());
    let _ = text("l").foreground(mixed);
    // Silent: the theme token passes as it is — nothing was erased.
    let _ = text("m").foreground(theme_color::Accent);
    // Silent: `Srgb::from_hex` passed directly is the remedy, not the bug.
    let _ = text("n").foreground(Srgb::from_hex("#ffffff"));
    // Silent: `signal_color` already returns a `Color` — a reactive color is
    // not a colorspace literal the parameter can take.
    let _ = text("o").foreground(signal_color(Color::red()));
    // Silent: `Color::red()` is a named color constructor, not a table row.
    let _ = text("p").foreground(Color::red());
    // Silent: a `Material` background — not a color at all.
    let _ = text("q").background(Material::Regular);

    no_srgb_import::f();
}

fn wants_color(color: Color) -> Color {
    color
}

/// A silent-case callee: the parameter's `Debug` bound is outside the color
/// table, so the lint cannot prove the rewritten type satisfies it.
fn paint(color: impl Into<Color> + std::fmt::Debug) -> Color {
    color.into()
}

mod no_srgb_import {
    use waterui::prelude::{Color, ViewExt, text};

    pub fn f() {
        // Fires: `Srgb` is not in scope — the suggestion imports it.
        let _ = text("r").foreground(Color::srgb_f32(0.5, 0.5, 0.5));
    }
}
