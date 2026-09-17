//! `on_change_derives_binding` fixture: an `on_change` handler whose body is
//! a single `b.set(f(v))` warns; handlers that ignore the watched value, do
//! more than one set, or derive the value with `.map` stay silent.

#![allow(unknown_lints)]
#![warn(on_change_derives_binding)]

use waterui::Str;
use waterui::prelude::*;

fn main() {
    let count: Binding<i32> = binding(0);
    let flag: Binding<bool> = binding(false);

    // Fires — the handler body is a single `b.set(f(v))`.
    let doubled: Binding<i32> = binding(0);
    let _ = text("a").on_change(&count, move |v: i32| doubled.set(v * 2));

    // Fires — a block body with a single tail expression is still one set.
    let label: Binding<Str> = binding(Str::from(""));
    let _ = text("a").on_change(&count, move |v: i32| {
        label.set(Str::from(v.to_string().to_uppercase()))
    });

    // Fires — `!v` still derives the stored value from the watched one.
    let other: Binding<bool> = binding(false);
    let _ = text("a").on_change(&flag, move |v: bool| other.set(!v));

    // Silent — the body is more than one statement, not a single set.
    let copied: Binding<i32> = binding(0);
    let _ = text("a").on_change(&count, move |v: i32| {
        copied.set(v);
        flag.set(true)
    });

    // Silent — the set does not read the watched value.
    let flag: Binding<bool> = binding(false);
    let _ = text("a").on_change(&count, move |_v: i32| flag.set(true));

    // Silent — the derived value is a `Computed`, not a copied `Binding`.
    let _doubled = count.map(|v| v * 2);
}
