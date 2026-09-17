//! `manual_computed` fixture: `Computed::new(signal)`, `Computed::from(e)`,
//! `e.into()`/`Into::into(e)` typed `Computed<T>`, and `e.into_computed()`
//! all warn — each is `SignalExt::computed` written long; `Computed::constant`,
//! `constant(..)`, `.computed()`, and the `Output`-converting `into_computed`
//! stay silent.

use waterui::prelude::*;
use waterui::reactive::constant;
use waterui::signal::IntoComputed;

fn make_count() -> Binding<i32> {
    Binding::i32(0)
}

/// `value` is `impl IntoComputed`, not a `Signal` — `.into_computed()` is the
/// only spelling it has.
fn takes(value: impl IntoComputed<i32>) -> Computed<i32> {
    // Silent — `value` is not a `Signal`; `value.computed()` would not compile.
    value.into_computed()
}

fn main() {
    let count: Binding<i32> = Binding::i32(0);
    let name: Binding<String> = binding("hello".to_string());

    // Fires — `Computed::new` of a cloned place is `s.computed()`.
    let _ = Computed::new(count.clone());
    // Fires — the signal need not be a clone; `e.computed()` reads the same.
    let _ = Computed::new(count.map(|v| v * 2));
    // Fires — `Computed::from` of a cloned binding is `s.computed()`.
    let _ = Computed::from(count.clone());
    // Fires — `.into()` whose target is `Computed<T>` over a signal.
    let _: Computed<i32> = count.clone().into();
    // Fires — `IntoComputed::into_computed` is the blanket trait form.
    let _: Computed<i32> = count.clone().into_computed();
    // Fires — UFCS `Into::into` is the same conversion.
    let _: Computed<i32> = Into::into(count.clone());
    // Fires — a `Constant` is still a signal; `e.computed()` is the form.
    let _ = Computed::new(constant(1));
    // Fires — the clone's receiver is a temporary; the suggestion keeps it.
    let _ = Computed::new(make_count().clone());

    // Silent — `Computed::constant` has no signal to call `computed()` on.
    let _: Computed<i32> = Computed::constant(1);
    // Silent — `constant(..)` produces a `Constant`, not a `Computed`.
    let _ = constant(1);
    // Silent — already the method form.
    let _ = count.computed();
    // Silent — `into_computed` may convert the output (`i64: From<i32>`), and
    // `count.computed()` would be `Computed<i32>`, not `Computed<i64>`.
    let _: Computed<i64> = count.clone().into_computed();
    // Silent — `.into()` whose target is not `Computed`.
    let _: Vec<u8> = name.get().into();
    // Silent — the `impl IntoComputed` helper stays as written.
    let _ = takes(count.clone());
}
