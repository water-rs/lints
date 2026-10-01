//! `manual_string_signal` facade fixture: an app that depends only on
//! `waterui` — `nami` is in the graph transitively but is not a name it
//! can write, and `nami_derive::s` expands to `::nami` paths. A real
//! facade-only crate therefore gets the "add `nami` to `[dependencies]`"
//! help (the lint keys the wording on whether `nami` is in `--extern`).
//! Under this test harness `nami` is a dev-dependency of the lint crate
//! itself, so it is passed as `--extern` to every fixture and the
//! blessed stderr shows the `use nami::s;` wording a `nami`-dependent
//! crate gets — the two situations cannot coexist in one compilation.

use waterui::prelude::*;

fn main() {
    let count: Binding<i32> = Binding::i32(3);
    // Fires — the map formats the signal's value by hand. A real
    // facade-only crate would be told to add `nami` to `[dependencies]`;
    // under this harness `nami` is already `--extern`, so the help says
    // `use nami::s;` instead (see the header comment).
    let _label = count.map(|c| format!("{c} items"));
}
