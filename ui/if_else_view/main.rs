use waterui::prelude::*;

fn side_a() {}

fn side_b() {}

fn main() {
    let flag: Binding<bool> = binding(true);
    let items: Binding<Vec<i32>> = binding(vec![1, 2, 3]);
    let c = true;
    let d = false;

    // Fires (plain): both arms erase to `AnyView` — `when` needs no erasure.
    let _ = if c {
        text("a").anyview()
    } else {
        text("b").anyview()
    };
    // Fires (plain): same-typed arms unify to `Text`.
    let _ = if c { text("a") } else { text("b") };
    // Fires (snapshot): `x.get()` — the fix passes `flag.clone()` to `when`.
    let _ = if flag.get() { text("on") } else { text("off") };
    // Fires (snapshot): `!x.get()` — the fix passes `!flag.clone()`.
    let _ = if !flag.get() { text("off") } else { text("on") };
    // Fires (snapshot): `.get()` deeper than the outer call — help only.
    let _ = if items.get().is_empty() {
        text("empty")
    } else {
        text("full")
    };
    // Fires (plain): an `else if` chain is one diagnostic on the outer `if`.
    let _ = if c {
        text("a")
    } else if d {
        text("b")
    } else {
        text("e")
    };
    // Fires (plain): block arms with statements stay verbatim in `|| { .. }`.
    let _ = if c {
        side_a();
        text("a")
    } else {
        text("b")
    };
    // Fires (plain): an `if` inside a `vstack` tuple element.
    let _ = vstack((if c { text("a") } else { text("b") }, text("tail")));

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
    // Silent: the arms are picked as a `button` label — `when` yields a view,
    // not an `IntoLabel`.
    let _ = button(if c { text("a") } else { text("b") });
    // Silent: an `if` without `else` has type `()`.
    if c {
        let _ = text("a");
    }
}
