//! `manual_signal_combinator` fixture: a `signal.map(|v| ..)` whose
//! one-parameter closure restates a named `SignalExt` combinator fires;
//! anything else stays silent.

use waterui::prelude::*;

fn helper() -> i32 {
    7
}

fn main() {
    let count: Binding<i32> = binding(0);
    let flag: Binding<bool> = binding(false);
    let name: Binding<String> = binding("hello".to_string());
    let opt: Binding<Option<i32>> = binding(Some(1));
    let res: Binding<Result<i32, String>> = binding(Ok(1));

    // Fires — `!v` on a `bool` output is `.not()`.
    let _ = flag.map(|v| !v);
    // Fires — `v == e` is `.equal_to(e)`.
    let _ = count.map(|v| v == 3);
    // Fires — `v > e` is `.gt(e)`.
    let _ = count.map(|v| v > 3);
    // Fires — `v <= e` is `.le(e)`.
    let _ = count.map(|v| v <= 3);
    // Fires — `-v` on a signed numeric output is `.negate()`.
    let _ = count.map(|v| -v);
    // Fires — `s.is_empty()` on a string output is `.str_is_empty()`.
    let _ = name.map(|s| s.is_empty());
    // Fires — `s.len()` on a string output is `.str_len()`.
    let _ = name.map(|s| s.len());
    // Fires — `s.contains(e)` is `.str_contains(e)`.
    let _ = name.map(|s| s.contains("x"));
    // Fires — `o.is_some()` on an `Option` output is `.is_some()`.
    let _ = opt.map(|o| o.is_some());
    // Fires — `o.is_none()` on an `Option` output is `.is_none()`.
    let _ = opt.map(|o| o.is_none());
    // Fires — `r.is_ok()` on a `Result` output is `.is_ok()`.
    let _ = res.map(|r| r.is_ok());
    // Fires — `if v { a } else { b }` on a `bool` output is `.select(a, b)`.
    let _ = flag.map(|b| if b { 10 } else { 20 });
    // Fires — a block body `{ !v }` matches the bare-expression shape.
    // (The comment keeps rustfmt from collapsing the block to `|v| !v`.)
    let _ = flag.map(|v| {
        // block with no statements and a tail
        !v
    });

    // Silent — `v * 2` spells no combinator.
    let _ = count.map(|v| v * 2);
    // Silent — the `v == e` operand mentions the parameter. (`v == v`
    // verbatim trips stock `eq_op`; `v * 2` keeps the same coverage.)
    let _ = count.map(|v| v == v * 2);
    // Silent — `s.trim().is_empty()` is not a bare `v` method call.
    let _ = name.map(|s| s.trim().is_empty());
    // Silent — `select` evaluates both arms eagerly, so a call stays.
    let _ = flag.map(|b| if b { helper() } else { 0 });
    // Silent — `Iterator::map`, not `SignalExt::map`.
    let _ = [true].into_iter().map(|v| !v);
    // Silent — `Option::map`, not `SignalExt::map`.
    let _ = Some(true).map(|v| !v);
    // Silent — `v + 1` spells no combinator.
    let _ = count.map(|v| v + 1);
}
