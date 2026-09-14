//! `watch_ignores_value` fixture: a `watch`/`Dynamic::watch` whose closure
//! never reads the watched value warns; closures that read it — through a
//! destructure or a nested closure — and non-closure `f`s stay silent.

use waterui::prelude::*;

/// A named `f`: what it does with the value is not visible at the call site.
fn render(count: i32) -> impl View {
    text(count.to_string())
}

fn main() {
    let flag: Binding<bool> = binding(true);
    let count: Binding<i32> = binding(3);
    let pair: Binding<(i32, i32)> = binding((1, 2));

    // Fires — `_` discards the value.
    let _ = watch(flag.clone(), |_| text("hi"));
    // Fires — an underscore-named binding is still never read.
    let _ = watch(flag, |_unused| text("hi"));
    // Fires — `Dynamic::watch`, the associated-function spelling.
    let _ = Dynamic::watch(count.clone(), |_| vstack((text("a"),)));
    // Fires — a named binding the body never reads.
    let _ = watch(count.clone(), |_v| text("fixed"));
    // Fires — a destructure whose bindings are all unread.
    let _ = watch(pair.clone(), |(_a, _b)| text("fixed"));

    // Silent — `v` is read.
    let _ = watch(count.clone(), |v| text(v.to_string()));
    // Silent — destructured parameter, `a` is read.
    let _ = watch(pair, |(a, _)| text(a.to_string()));
    // Silent — `v` is read through a `let`.
    let _ = watch(count.clone(), |v| {
        let n = v;
        text(n.to_string())
    });
    // Silent — `v` is read only inside a nested closure.
    let _ = watch(count.clone(), |v| {
        button("x").action(move || {
            let _ = v;
        })
    });
    // Silent — `f` is a named fn, not a closure literal.
    let _ = watch(count.clone(), render);
    // Silent — `map` on a signal is not `watch`.
    let _ = count.map(|_| 0);
}
