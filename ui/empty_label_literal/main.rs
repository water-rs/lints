//! `empty_label_literal` fixture: an empty or whitespace-only string
//! literal in an `IntoLabel` parameter or `.a11y_label(..)` errors; a
//! non-empty label, a non-literal label, and an `IntoText` position stay
//! silent.

use waterui::Str;
use waterui::prelude::*;

fn main() {
    let flag: Binding<bool> = binding(false);
    let name: Binding<Str> = binding(Str::from_static("lexo"));
    let value: Binding<f64> = binding(0.5);

    // Fires — the mandatory label is empty.
    let _ = button("");
    // Fires — whitespace-only still announces nothing.
    let _ = button("   ");
    // Fires — `toggle`'s label parameter is `impl IntoLabel`.
    let _ = toggle("", &flag);
    // Fires — a tab trims to empty.
    let _ = slider("\t", &value);
    // Fires — `field`'s label parameter is `impl IntoLabel`.
    let _ = field("", &name);
    // Fires — `a11y_label` is the accessibility label itself.
    let _ = text("a").a11y_label("");
    // Fires once — the constructor argument; `action`'s handler is no label.
    let _ = button("").action(|| {});

    // Silent — a real label.
    let _ = button("Save");
    // Silent — the argument is a variable, not a literal at the position.
    let title = "";
    let _ = button(title);
    // Silent — `text` takes `impl IntoText`, not a label position.
    let _ = text("");
    // Silent — `Label::new`'s first parameter is `impl IntoText`; the
    // documented icon-only label keeps its semantic text.
    let _ = Label::new("Save", || text("x")).icon_only();
    // Fires once — the constructor argument stays mandatory; the a11y label
    // fixes the tree, not the `IntoLabel` parameter.
    let _ = button("").a11y_label("Save");
}
