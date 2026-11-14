use waterui::prelude::*;
use waterui::widget::condition::when;

fn side_a() {}

fn side_b() {}

// Silent: the `if` is a `-> impl View` body's tail — a view position —
// but the condition is a plain `bool`, which `when` cannot take and no
// `.get()` sits in it to unwrap, so the lint stays silent.
fn plain_bool(c: bool) -> impl View {
    if c { text("a") } else { text("b") }
}

// Fires (snapshot): `x.get()` — the fix passes `flag.clone()` to `when`.
fn snapshot(flag: Binding<bool>) -> impl View {
    if flag.get() { text("on") } else { text("off") }
}

// Fires (snapshot): `!x.get()` — the fix passes `!flag.clone()`.
fn negated(flag: Binding<bool>) -> impl View {
    if !flag.get() { text("off") } else { text("on") }
}

// Fires (snapshot): `.get()` deeper than the outer call — help only.
fn deep_get(items: Binding<Vec<i32>>) -> impl View {
    if items.get().is_empty() {
        text("empty")
    } else {
        text("full")
    }
}

// Fires: an `else if` chain is one diagnostic on the outer `if`.
fn chained(flag: Binding<bool>, d: Binding<bool>) -> impl View {
    if flag.get() {
        text("a")
    } else if d.get() {
        text("b")
    } else {
        text("e")
    }
}

// Fires: block arms with statements stay verbatim in `|| { .. }`.
fn blocks(flag: Binding<bool>) -> impl View {
    if flag.get() {
        side_a();
        text("a")
    } else {
        side_b();
        text("b")
    }
}

// Silent: the `let` initializer is data position even inside a
// `-> impl View` body — only the body's own tail is a view position.
fn inner_let(flag: Binding<bool>) -> impl View {
    let _v = if flag.get() { text("a") } else { text("b") };
    text("body")
}

fn main() {
    let flag: Binding<bool> = Binding::bool(true);
    let items: Binding<Vec<i32>> = binding(vec![1, 2, 3]);
    let c = true;

    // Fires (snapshot): an `if` passed where an `impl View` parameter sits.
    let _ = scroll(if flag.get() { text("on") } else { text("off") });
    // Fires (snapshot): an `if` inside a `vstack` tuple element — each
    // element of a `TupleViews` argument is a view position.
    let _ = vstack((if flag.get() { text("a") } else { text("b") }, text("tail")));
    // Silent: a `when` builder closure's tail — the closure is passed to a
    // `ViewBuilder` parameter — but `c` is a plain `bool`.
    let _ = when(flag.clone(), move || if c { text("a") } else { text("b") });
    // Fires (snapshot): the receiver of a `ViewExt` method call.
    let _ = (if flag.get() { text("a") } else { text("b") }).anyview();

    let _ = plain_bool(c);
    let _ = snapshot(Binding::bool(true));
    let _ = negated(Binding::bool(true));
    let _ = deep_get(binding(vec![4, 5, 6]));
    let _ = chained(Binding::bool(true), Binding::bool(false));
    let _ = blocks(Binding::bool(true));
    let _ = inner_let(Binding::bool(false));

    // Silent: an `if` picking between colors — data position, and `Color`
    // is not a view (the issue's repro).
    let compact = true;
    let _accent = if compact {
        Color::srgb(255, 0, 0)
    } else {
        Color::srgb(0, 0, 255)
    };
    // Silent: a view-typed `if` in a `let` initializer — data position.
    let _v = if c { text("a") } else { text("b") };
    // Silent: same with a signal-reading condition — still data position.
    let _v2 = if flag.get() { text("a") } else { text("b") };
    // Silent: the `if` is consumed by a parameter with no view bound — a
    // data position. (`drop` stands in for a discarded-statement `if`,
    // which rustc's own `unused_must_use` rejects.)
    drop(if c { text("a") } else { text("b") });
    // Silent: `if let` is not a `when` condition.
    let opt = Some(text("x"));
    let _ = if let Some(t) = opt { t } else { text("none") };
    // Silent: `bool::then` producing `Option<impl View>` is the documented idiom.
    let _ = c.then(|| text("a"));
    // Silent: `Option<V>` arms — `Option` is `core`, not a `waterui` ADT.
    let _ = if c { Some(text("a")) } else { None };
    // Silent: `()` arms — a side-effect `if`, not a view choice.
    if c {
        side_a()
    } else {
        side_b()
    };
    // Silent: `&'static str` arms are not a `waterui` ADT.
    let _ = if c { "a" } else { "b" };
    // Silent: `match` on a non-bool is untouched.
    let _ = match items.get().len() {
        0 => text("empty"),
        _ => text("full"),
    };
    // Silent: the arms are picked as a `button` label — `when` yields a
    // view, not an `IntoLabel`, so a label parameter is not a view position.
    let _ = button(if c { text("a") } else { text("b") });
    // Silent: an `if` without `else` has type `()`.
    if c {
        let _ = text("a");
    }
}
