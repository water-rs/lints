//! `s_macro_in_text` fixture: an `s!(..)` reaching an `IntoText`/`IntoLabel`
//! parameter warns and rewrites to `text!(..)` — the whole call for
//! `text(..)`/`Text::new(..)`, the argument elsewhere, help only for a
//! `let`-bound value. `s!` yields `Map`, so every text-position use below
//! goes through `.computed()` — the lint looks through that carrier.
//! Signal-typed parameters and `text!` stay silent.

use waterui::prelude::*;
use waterui::reactive::signal::IntoComputed;
use waterui::reactive::{constant, s};

fn keep(_: impl IntoComputed<String>) {}

fn main() {
    let name = constant("Alice");
    let count = constant(3);
    let price = constant(9.99);

    // Fires: `text(s!(..).computed())` — the `text!` rewrite replaces the
    // whole call.
    let _ = text(s!("Hello {name}").computed());
    // Fires: `Text::new(s!(..).computed())` — the same whole-call rewrite.
    let _ = Text::new(s!("{count} items").computed());
    // Fires: `button(s!(..).computed())` — an `IntoLabel` position; `text!`
    // replaces only the argument.
    let _ = button(s!("Buy {price}").computed());
    // Fires: a positional `{}` becomes a named `arg0 = name` binding.
    let _ = text(s!("Hi {}", name).computed());
    // Fires: the `let` form — the diagnostic lands on the `s!`, help only.
    let msg = s!("Total: {price}").computed();
    let _ = text(msg);

    // Silent: `s!` flows into an `IntoComputed<String>` parameter — the
    // signal stays a signal, no text position is involved.
    keep(s!("Hi {name}"));
    // Silent: `.a11y_label` takes `IntoComputed<Str>`, not a
    // `TEXT_PARAM_BOUNDS` bound.
    let _ = text("x").a11y_label(s!("Hi {name}"));
    // Silent: `text!` is the remedy.
    let _ = text!("Hello {name}");
}
