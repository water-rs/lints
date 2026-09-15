//! `localized_concat` fixture: `+` on `Text` whose left operand is localized
//! — a `text!` expansion, a `Text::localized` call, or `text(..)` on a
//! `&'static str` — warns once per chain in any position; verbatim
//! composition, a lone `text!`, and `String` addition stay silent.

#![allow(unknown_lints)]
#![warn(localized_concat)]

use waterui::Str;
use waterui::prelude::*;

fn main() {
    let name: Binding<Str> = binding(Str::from_static("Ada"));

    // Fires — `text!` on the left of a `Text` concat.
    let _ = text!("Dear ") + text!("{name}");
    // Fires — the rhs needs no localized form; the left operand decides.
    let _ = text!("Dear ") + name.clone();
    // Fires — `Text::localized` on the left.
    let _ = Text::localized("greeting") + text!("{name}");
    // Fires — `text("…")` resolves `&'static str` through the catalog too.
    let _ = text("Dear ") + text!("{name}");
    // Fires once — a chain reports at its outermost `+`.
    let _ = text!("a") + text!("b") + text!("c");

    // Silent — verbatim on the left is styled-run composition.
    let raw = String::from("a");
    let _ = Text::verbatim(raw.clone()) + Text::verbatim(raw);
    // Silent — a lone `text!` concatenates nothing.
    let _ = text!("Dear {name}");
    // Silent — `String` addition is not `Text` concatenation.
    let _ = String::from("a") + "b";
}
