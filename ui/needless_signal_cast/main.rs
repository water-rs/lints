//! `needless_signal_cast` fixture: a `SignalExt::map` whose argument is a
//! pure numeric conversion to `f32` warns when the produced signal only ever
//! reaches an `impl IntoSignalF32` parameter — as the direct argument,
//! through value-keeping adapters (`.with(..)`, `.cached()`, `.computed()`),
//! or through a `let` whose every use is such an argument. A signal also
//! consumed as `f32` elsewhere, a closure that does more than convert, a
//! cast to a non-`f32` type, and an `IntoComputed`/`Signal` parameter stay
//! silent. The fix deletes `.map(..)` — or rewrites it to `.clone()` when
//! the receiver is a place that must not move.

use num_traits::ToPrimitive;
use std::time::Duration;
use waterui::animation::Animation;
use waterui::prelude::*;
use waterui::signal::IntoComputed;
use waterui::text::Text;

struct Panel {
    dim: Computed<f32>,
}

struct Knob {
    level: Binding<u8>,
}

/// An `impl IntoComputed<f32>` parameter — it does not perform the `f32`
/// conversion the way `IntoSignalF32` does.
fn freeze(_: impl IntoComputed<f32>) {}

fn main() {
    // Fires — `Binding<f64>` through `.map(|v| v as f32)` and `.with(..)`
    // into `opacity`; the adapter borrows the receiver, so the fix deletes.
    let animated = Binding::f64(4.0)
        .map(|v| v as f32)
        .with(Animation::ease_in_out(Duration::from_millis(300)));
    let _ = text("a").opacity(animated);

    // Fires — the `map` is the direct `impl IntoSignalF32` argument and the
    // receiver is a temporary, so the fix deletes.
    let _ = text("a").opacity(Binding::f64(0.5).map(|v| v as f32));

    // Fires — path form `f32::from`; `level_from` is never used again.
    let level_from: Binding<u8> = binding(3u8);
    let _ = text("a").opacity(level_from.map(f32::from));

    // Fires — closure form `|v| f32::from(v)` (the explicit return type
    // keeps `clippy::redundant_closure` quiet).
    let level_call: Binding<u8> = binding(4u8);
    let _ = text("a").opacity(level_call.map(|v| -> f32 { f32::from(v) }));

    // Fires — `|v| v.into()` with the closure's return typed `f32`.
    let level_into: Binding<u8> = binding(5u8);
    let _ = text("a").opacity(level_into.map(|v| -> f32 { v.into() }));

    // Fires — `|v| v.to_f32().unwrap()`.
    let level_prim: Binding<u8> = binding(6u8);
    let _ = text("a").opacity(level_prim.map(|v| v.to_f32().unwrap()));

    // Fires — `.cached()` and `.computed()` keep the mapped value.
    let _ = text("a").opacity(Binding::f64(0.7).map(|v| v as f32).cached());
    let _ = text("a").opacity(Binding::f64(0.8).map(|v| v as f32).computed());

    // Fires — the `let`'s two uses are both `IntoSignalF32` arguments; the
    // suggestion is `MaybeIncorrect`.
    let shared = Binding::f64(0.9).map(|v| v as f32);
    let _ = text("a").opacity(shared.clone());
    let _ = text("b").aspect_ratio(shared);

    // Fires — `b` is read again after the `map`, so the fix rewrites the
    // segment to `.clone()` rather than deleting it.
    let b: Binding<f64> = Binding::f64(1.0);
    let _ = text("a").opacity(b.map(|v| v as f32));
    let _ = b.get();

    // Fires — `b2` is read again after the `let`; the fix is
    // `let y = b2.clone()`, still `MachineApplicable` for one consumer.
    let b2: Binding<f64> = Binding::f64(2.0);
    let y = b2.map(|v| v as f32);
    let _ = b2.get();
    let _ = text("a").opacity(y);

    // Fires — `b3` is bound outside the loop, so the next iteration reads it
    // again; the fix is `.clone()` even though no later use follows.
    let b3: Binding<f64> = Binding::f64(3.0);
    for _ in 0..2 {
        let _ = text("a").opacity(b3.map(|v| v as f32));
    }

    // Fires — `knob.level` is a field place; the fix is `.clone()`.
    let knob = Knob {
        level: binding(1u8),
    };
    let _ = text("a").opacity(knob.level.map(f32::from));

    // Fires — the comment inside `.map(..)` would be deleted by the fix;
    // the suggestion is `MaybeIncorrect`.
    let _ = text("a").opacity(Binding::f64(1.1).map(/* keep me */ |v| v as f32));

    // Silent — `t` is also read as `f32` through `.get()`.
    let t = Binding::f64(0.1).map(|v| v as f32);
    let _ = t.get();
    let _ = text("a").opacity(t);

    // Silent — `d` also feeds `Text::display`'s `impl Signal` parameter.
    let d = Binding::f64(0.2).map(|v| v as f32);
    let _ = Text::display(d.clone());
    let _ = text("a").opacity(d);

    // Silent — `s` is also captured by `text!`, a use inside a macro
    // expansion that is not an `IntoSignalF32` argument.
    let s = Binding::f64(1.2).map(|v| v as f32);
    let _ = text("a").opacity(s.clone());
    let _ = text!("{s}");

    // Silent — the closures do more than the conversion.
    let _ = text("a").opacity(Binding::f64(0.3).map(|v| v as f32 / 100.0));
    let _ = text("a").opacity(Binding::f64(0.4).map(|v| (v as f32).clamp(0.0, 1.0)));

    // Silent — `f64` output; there is no `IntoSignalF64`.
    let wide_out: Binding<u8> = binding(9u8);
    let _ = text("a").opacity(wide_out.map(|v| v as f64));

    // Silent — `u128` has no `IntoF32`; the parameter could not convert the
    // unmapped signal.
    let wide: Binding<u128> = binding(7u128);
    let _ = text("a").opacity(wide.map(|v| v as f32));

    // Silent — the mapped signal is stored into a `Computed<f32>` field.
    let panel = Panel {
        dim: Binding::f64(0.6).map(|v| v as f32).computed(),
    };
    let _ = panel.dim.get();

    // Silent — an `impl IntoComputed<f32>` parameter does not convert.
    freeze(Binding::f64(0.7).map(|v| v as f32));

    // Silent — the `map` result is dropped, never reaching a parameter.
    let _ = Binding::f64(0.8).map(|v| v as f32);
}
