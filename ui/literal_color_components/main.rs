use waterui::prelude::*;

const R: u8 = 244;
const G: u8 = 67;
const B: u8 = 54;

// Fires: a `const` initializer — `Srgb::from_hex` is a `const fn`.
const RED: Srgb = Srgb::new_u8(244, 67, 54);

fn main() {
    let (r, g, b) = (0.96f32, 0.26f32, 0.21f32);
    let t = 0.5f32;
    let _ = RED;

    // Fires: on-grid floats map exactly — `#000000`.
    let _ = Srgb::new(0.0, 0.0, 0.0);
    // Fires: `u8` components map exactly — `#F44336`, first occurrence.
    let _ = Srgb::new_u8(244, 67, 54);
    // Fires: the same color packed decimal — second `#F44336`.
    let _ = Srgb::from_u32(16007990);
    // Fires: inexact — rounds to (245, 66, 54), so `MaybeIncorrect`.
    let _ = Srgb::new(0.96, 0.26, 0.21);
    // Fires: a literal under a cast still counts — third `#F44336`.
    let _ = Srgb::new_u8(244u16 as u8, 67, 54);
    // Fires: a `Color` is required — `Color::srgb_hex("#FFFFFF")`.
    let _: Color = Color::srgb(255, 255, 255);
    // Fires: `#FFFFFF` again through the `Srgb` spelling.
    let _ = Srgb::new_u8(255, 255, 255);
    // Fires: `Color::srgb_f32` inexact — `MaybeIncorrect`.
    let _: Color = Color::srgb_f32(0.96, 0.26, 0.21);
    // Fires: `Color::srgb_u32` decimal — fourth `#F44336`.
    let _: Color = Color::srgb_u32(16007990);
    // Fires: `as` truncates — `300u16 as u8` is `44`, so `#2C0000`.
    let _ = Srgb::new_u8(300u16 as u8, 0, 0);
    // Fires: negation then cast — `-1i8 as u8` is `255`, so `#FF0000`.
    let _ = Srgb::new_u8(-1i8 as u8, 0, 0);
    // Fires: `into`'s `self` is `Self` pinned by the call, not an
    // `impl Into<Color>` position — `manual_color_erasure` never rewrites
    // there, so the `Color::*` call inside still reports.
    let c: Color = <Color as Into<Color>>::into(Color::srgb(1, 2, 3));
    let _ = c;
    // Fires: an `Srgb` constructor at `impl Into<Color>` still reports —
    // only the `Color::*` spelling defers to `manual_color_erasure`.
    let _ = text("b").foreground(Srgb::new_u8(9, 20, 35));

    // Silent: variable components are the only spelling of a computed color.
    let _ = Srgb::new(r, g, b);
    // Silent: one expression among the literals is enough.
    let _ = Srgb::new(t, 0.0, 1.0 - t);
    // Silent: the hex spelling is the remedy, not the bug.
    let _ = Srgb::from_hex("#F44336");
    // Silent: a packed hex literal is already readable.
    let _ = Srgb::from_u32(0xF44336);
    // Silent: `P3` has no hex constructor.
    let _ = P3::new(1.0, 0.0, 0.0);
    // Silent: `const` names are not literals.
    let _ = Srgb::new_u8(R, G, B);
    // Silent for this lint: a `Color::*` call at an `impl Into<Color>`
    // parameter is `manual_color_erasure`'s diagnostic.
    let _ = text("a").foreground(Color::srgb(244, 67, 54));
}
