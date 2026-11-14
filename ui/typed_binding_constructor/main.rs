//! `typed_binding_constructor` fixture: `binding(..)`/`Binding::container(..)`
//! calls that produce a `Binding` of `u32`, `u64`, `usize`, `i32`, `i64`,
//! `isize`, `f32`, `f64`, or `bool` warn with the `Binding::<t>(..)` rewrite;
//! a `Binding` of any other `T`, the dedicated constructors,
//! `Binding::default()`, and `constant(..)` stay silent.

use waterui::prelude::*;
use waterui::reactive::constant;

struct Counters {
    total: Binding<u64>,
    index: Binding<usize>,
}

fn consume(_: Binding<i64>) {}

fn main() {
    // Fires: a `let`-annotated `binding(..)` per primitive — the fix drops
    // the annotation too (`let b = Binding::u32(0)`); a literal suffix naming
    // `T` goes with it (`0_u32` -> `0`).
    let _u: Binding<u32> = binding(0_u32);
    let _i: Binding<i32> = binding(0);
    let _l: Binding<i64> = binding(0);
    let _z: Binding<isize> = binding(0_isize);
    let _f: Binding<f32> = binding(0.0_f32);
    let _d: Binding<f64> = binding(0.0);
    let _b: Binding<bool> = binding(false);

    // Fires: `Binding::container(..)` in struct fields (`u64`, `usize`).
    let counters = Counters {
        total: Binding::container(0),
        index: Binding::container(0),
    };
    let _ = counters.total.get() + counters.index.get() as u64;

    // Fires: `T` is fixed by the function parameter.
    consume(binding(0));

    // Fires: the suffix naming `T` is dropped (`0_i32` -> `0`); `T` is fixed
    // by the later `set`.
    let suffix = binding(0_i32);
    suffix.set(1_i32);

    // Fires: `T` is fixed by the later `count.set(3_i64)`.
    let count = binding(0);
    count.set(3_i64);

    // Silent: `T` is `String`.
    let _s: Binding<String> = binding("x".to_string());
    // Silent: `T` is `Option<u8>`.
    let _o: Binding<Option<u8>> = binding(Some(3_u8));
    // Silent: already the dedicated constructor.
    let _t = Binding::bool(true);
    // Silent: `Binding::default()` is not a generic-constructor call.
    let _x: Binding<i32> = Binding::default();
    // Silent: `constant(..)` is a different function.
    let _k = constant(1);
    // Silent: `0_u8` is `u8`, not `u64` — `Binding::u64(0_u8)` does not
    // type-check (`binding` took `impl Into<T>`, the dedicated ctor takes `T`).
    let _w: Binding<u64> = binding(0_u8);
    // Silent: `0.1_f32` is `f32`, not `f64` — and dropping the suffix would
    // change the value (widening `0.1_f32` is not `0.1_f64`).
    let _p: Binding<f64> = binding(0.1_f32);

    no_binding_import::make();
}

/// `Binding` is not imported inside this module, so the fix also writes the
/// `use` line.
mod no_binding_import {
    use waterui::reactive::binding;

    pub fn make() {
        // Fires: suggests `Binding::i64(0)` plus `use waterui::Binding;`.
        let count = binding(0);
        count.set(3_i64);
    }
}
