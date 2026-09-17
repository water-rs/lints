// compile-flags: --test
//! `unwrap_in_signal_map` fixture: an `unwrap`/`expect`/`unwrap_unchecked`
//! on the parameter inside a `SignalExt::map` closure — directly, through a
//! `zip` destructure, through field/`as_ref` projections, spliced into a
//! macro call, or in a nested closure — warns; an unwrap on a captured
//! value, inside a handler closure, in a closure over a captured iterable,
//! or in a `#[cfg(test)]`/`#[waterui::test]` item stays silent, as do
//! `Option::map`/`Iterator::map` and non-panicking combinators.
//!
//! The `--test` flag is required: rustc strips `#[test]` items — including
//! the `#[test] fn` the harness macro emits — and `#[cfg(test)]` items in
//! non-test builds.

use waterui::prelude::*;

// Imported under `cfg(test)` for the same reason the harness compiles this
// file with `--test`: the only use is the `#[waterui::test]` signature,
// which rustc strips in non-test builds.
#[cfg(test)]
use waterui_testing::SemanticApp;

#[derive(Clone)]
struct User {
    name: String,
}

#[derive(Clone)]
struct State {
    user: Option<User>,
}

// A non-literal `Option` — a `Some(..)` receiver would be clippy's
// `unnecessary_literal_unwrap`, not this lint's case.
fn cached() -> Option<i32> {
    Some(7)
}

fn main() {
    // `#[test]` items are stripped in non-test builds; name `home` here so it
    // is live either way.
    let _ = home;

    let maybe: Binding<Option<i32>> = binding(Some(1));
    let res: Binding<Result<i32, String>> = binding(Ok(1));
    let flag = Binding::bool(false);
    let num = Binding::i32(2);
    let state: Binding<State> = binding(State { user: None });
    let nested: Binding<Option<Option<i32>>> = binding(Some(Some(1)));

    // Fires — `unwrap` on the `map` parameter.
    let _ = maybe.map(|v| v.unwrap());
    // Fires — `expect` on a `Result` output.
    let _ = res.map(|r| r.expect("loaded"));
    // Fires — `unwrap_unchecked` is the same hazard without the check.
    let _ = maybe.map(|v| unsafe { v.unwrap_unchecked() });
    // Fires — a `zip(..).map(..)` parameter through a tuple destructure.
    let _ = maybe.zip(&flag).map(|(a, _)| a.unwrap());
    // Fires — field access and `as_ref` peel back to the parameter.
    let _ = state.map(|s| s.user.as_ref().unwrap().name.clone());
    // Fires — a nested closure's parameter is bound from `v`'s value.
    let _ = nested.map(|v| v.map(|x| x.unwrap()));
    // Fires — `v.unwrap()` keeps its callsite span inside `format!`.
    let _ = maybe.map(|v| format!("{}", v.unwrap()));

    let inner_sig: Binding<Option<i32>> = binding(Some(3));
    // Fires once, on the inner `map` — `w` is the inner closure's own
    // parameter, so the inner call is checked on its own span rather than
    // through the outer closure's walk.
    let _ = num.map(move |n| inner_sig.map(move |w| w.unwrap() + n));

    let captured: Option<i32> = cached();
    // Silent — the receiver is a captured value, not the parameter.
    let _ = num.map(move |n| n + captured.unwrap());
    // Silent — `unwrap` inside a handler closure is not a `map` transform.
    let _ = button("b").action(move || {
        let _ = captured.unwrap();
    });
    // Silent — a handler closure nested in the body still is not the
    // transform, and `captured` is not the parameter.
    let _ = num.map(move |n| {
        let _ = button("b").action(move || {
            let _ = captured.unwrap();
        });
        n * 2
    });
    let captured_opts: [Option<i32>; 2] = [Some(1), None];
    // Silent — `i` is bound by an `Iterator::map` over a captured array;
    // `captured_opts.iter()` does not derive from the parameter.
    let _ = num.map(move |n| captured_opts.iter().map(|i| i.unwrap()).sum::<i32>() + n);
    // Silent — `unwrap_or_default` is the remedy, not the panic.
    let _ = maybe.map(|v| v.unwrap_or_default());
    // Silent — `Option::map`, not `SignalExt::map`.
    let _ = Some(Some(1)).map(|v| v.unwrap());
    // Silent — `Iterator::map`, not `SignalExt::map`.
    let _ = [Some(1)].into_iter().map(|v| v.unwrap());
}

fn home() -> impl View {
    text("a")
}

// Silent — a `map` in a `#[cfg(test)]` item: a test asserting the value is
// the point.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps() {
        let maybe: Binding<Option<i32>> = binding(Some(1));
        let _ = maybe.map(|v| v.unwrap());
    }
}

// Silent — a `#[waterui::test]` body asserts the value.
#[waterui::test(home)]
fn unwraps_in_test(app: &mut SemanticApp) {
    let maybe: Binding<Option<i32>> = binding(Some(1));
    let _ = maybe.map(|v| v.unwrap());
    let _ = app;
}
