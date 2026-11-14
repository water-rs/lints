//! `hardcoded_theme_value` fixture: all-literal `Color::srgb*`/`Srgb::*`
//! constructors and `Text`/`Font`/`StyledStr::size` calls warn inside
//! `-> impl View` functions, their nested closures, and `View::body`; theme
//! tokens, semantic fonts, named constants, non-literal arguments, and
//! literals outside view code stay silent.

#![allow(unknown_lints)]
#![warn(hardcoded_theme_value)]

use waterui::prelude::*;
use waterui::text::font::{Font, Title};
use waterui::widget::condition::when;

/// A brand color lifted to a named constant — the sanctioned spelling.
const BRAND: Srgb = Srgb::new(0.1, 0.3, 0.9);

fn card() -> impl View {
    let flag: Binding<bool> = binding(true);
    let (r, g, b) = (1, 2, 3);
    vstack((
        // Fires — literal `Color::srgb` in view code.
        text("a").foreground(Color::srgb(255, 0, 0)),
        // Fires — literal `Srgb::new` in view code.
        text("a").foreground(Srgb::new(0.2, 0.2, 0.2)),
        // Fires — literal `Color::srgb_hex` in view code.
        text("a").foreground(Color::srgb_hex("#ff8800")),
        // Fires — literal `Text::size` in view code.
        text("a").size(18.0),
        // Fires — literal `Font::size` in view code.
        text("a").font(Font::new(Title).size(20.0)),
        // Fires — literal `Text::size` inside a nested `when` closure.
        when(flag.clone(), || text("b").size(11.0)),
        // Silent — a theme token adapts to the color scheme.
        text("a").foreground(AccentColor),
        // Silent — a semantic font follows the typography scale.
        text("a").font(Title),
        // Silent — a named constant is the sanctioned brand-color spelling.
        text("a").foreground(BRAND),
        // Silent — non-literal arguments are computed, not hardcoded.
        text("a").foreground(Color::srgb(r, g, b)),
    ))
}

struct Card;

impl View for Card {
    fn body(self, _env: &Environment) -> impl View {
        // Fires — literal `Text::size` inside `View::body`.
        text("a").size(13.0)
    }
}

/// Silent — not a view function; literal colors outside view code are fine.
fn palette() -> Color {
    Color::srgb(1, 2, 3)
}

fn main() {
    let _ = card();
    let _ = Card.body(&Environment::new());
    let _ = palette();
}
