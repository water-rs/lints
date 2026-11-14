//! `watch_for_reactive_value` fixture: a `watch`/`Dynamic::watch` whose
//! single-expression closure reads the value only through carriers into
//! signal-taking parameters warns; scrutinee reads, non-carrier calls,
//! statement bodies, and named `f`s stay silent.

use waterui::prelude::*;

/// A named `f`: what it does with the value is not visible at the call site.
fn render(count: i32) -> impl View {
    text(count.to_string())
}

fn main() {
    let count: Binding<i32> = binding(3);
    let flag: Binding<bool> = binding(true);
    let name: Binding<String> = binding("n".to_string());

    // Fires — `v` reaches `text`'s `IntoText` parameter through `.to_string()`.
    let _ = watch(count.clone(), |v| text(v.to_string()));
    // Fires — `v` reaches `text` as a `format!` argument.
    let _ = watch(count.clone(), |v| text(format!("{v} left")));
    // Fires — `f` reaches `visible`'s `IntoComputed<bool>` parameter.
    let _ = watch(flag.clone(), |f| text("x").visible(f));
    // Fires — `n` reaches `text`'s `IntoText` parameter through `.clone()`.
    let _ = watch(name.clone(), |n| text(n.clone()));
    // Fires — `Dynamic::watch`, the associated-function spelling.
    let _ = Dynamic::watch(count.clone(), |v| vstack((text(v.to_string()), text("b"))));
    // Fires — a block with no statements and a tail expression.
    let _ = watch(count.clone(), |v| {
        vstack((text(v.to_string()), text("b"), text("c"), text("d")))
    });

    // Silent — `v` is the `match` scrutinee: the structural switch is `watch`'s job.
    let _ = watch(count.clone(), |v| match v {
        0 => text("none"),
        _ => text("some"),
    });
    // Silent — `.len()` inspects the value; it is not a carrier.
    let _ = watch(name.clone(), |n| text(n.len().to_string()));
    // Silent — the body computes `n` in a statement first.
    let _ = watch(count.clone(), |v| {
        let n = v + 1;
        text(n.to_string())
    });
    // Silent — `f` is a named fn, not a closure literal.
    let _ = watch(count.clone(), render);
}
