//! `needless_computed` fixture: `.computed()`/`.into_computed()`/
//! `Computed::new(..)` whose result only reaches parameters that accept any
//! signal (`IntoComputed`/`IntoSignal`/`Signal`/`IntoSignalF32`) warn —
//! directly (`MachineApplicable`) or through a `let`/`Option` whose every
//! use is one (`MaybeIncorrect`). Erasures stored in fields, returned from
//! `-> Computed<T>` functions, unified across branches, or consumed by
//! non-signal positions stay silent.

use waterui::prelude::*;
use waterui::signal::IntoComputed;

/// A `Computed` field — the erasure is needed to name the concrete type.
struct Panel {
    dim: Computed<f32>,
}

/// A `Binding` field — a place that cannot move out of its owner.
struct Toggle {
    flag: Binding<bool>,
}

/// `-> Computed<bool>` — the erasure names the shared return type.
fn produce(flag: &Binding<bool>) -> Computed<bool> {
    flag.not().computed()
}

/// A parameter that takes `Computed<T>` concretely — the erasure is the
/// only way to produce it.
fn takes_computed(_: Computed<bool>) {}

fn main() {
    let flag = Binding::bool(false);
    let count = Binding::i32(3);

    // Fires — `.disabled` takes `impl IntoComputed<bool>`.
    let _ = text("a").disabled(flag.not().computed());

    // Fires — `.opacity` takes `impl IntoSignalF32`, which any numeric
    // signal satisfies.
    let _ = text("a").opacity(flag.select(0.45, 1.0).computed());

    // Silent — `text!` reads its slot through `SignalExt::map`, whose
    // `Map<Self, _>` return embeds `Self`: the erasure's receiver type is
    // load-bearing for that use.
    let _ = text!("{n}", n = count.map(|v| v * 2).computed());

    // Fires — `Option::map` wraps the erasure; the `if let` binding's only
    // use is an `IntoComputed` argument.
    let channel: Option<u32> = Some(3);
    let unavailable = channel.map(|_| flag.not().computed());
    if let Some(d) = unavailable {
        let _ = text("a").disabled(d);
    }

    // Fires — `Some(..)` wraps the erasure; a `match` arm destructure.
    let boxed = Some(flag.not().computed());
    match boxed {
        Some(d) if flag.get() => {
            let _ = text("a").disabled(d);
        }
        _ => {}
    }

    // Fires — `let`-bound, used once at an `IntoComputed` parameter.
    let d = flag.not().computed();
    let _ = text("a").disabled(d);

    // Fires — `Computed::new(e)` is the same erasure.
    let _ = text("a").disabled(Computed::new(flag.not()));

    // Fires — `into_computed` is the trait spelling of it (UFCS pins the
    // output type).
    let _ = text("a").disabled(IntoComputed::<bool>::into_computed(flag.not()));

    // Fires — every use of `shared` reaches a signal-accepting parameter,
    // one through `.clone()`.
    let shared = flag.not().computed();
    let _ = text("a").disabled(shared.clone());
    let _ = text("b").disabled(shared);

    // Fires — `read`'s only use is a `&self` `Signal` method.
    let read = flag.not().computed();
    let _ = read.get();

    // Silent — the erasure is stored in a `Computed` field.
    let panel = Panel {
        dim: count.map(|v| v as f32).computed(),
    };
    let _ = panel.dim.get();

    // Silent — `produce` declares `-> Computed<bool>`.
    let _ = produce(&flag);

    // Silent — the `if`/`else` branches unify on `Computed<bool>`.
    let sel = if flag.get() {
        flag.computed()
    } else {
        count.map(|v| v.is_positive()).computed()
    };
    let _ = sel.get();

    // Silent — `Computed::constant` has no source signal.
    let _ = Computed::constant(true);

    // Fires — `get` is a `&self` `Signal` method returning `Self::Output`,
    // which the erasure pins to `f32`; `level.get()` reads the same value.
    let level = Binding::f32(0.5);
    let _ = level.computed().get();

    // Fires — `reused` is read again after the erasure, so the fix clones
    // rather than moves it.
    let reused = Binding::bool(false);
    let _ = text("a").disabled(reused.computed());
    let _ = reused.get();

    // Fires — a field place cannot move out of `toggle`; the fix clones it.
    let toggle = Toggle {
        flag: Binding::bool(true),
    };
    let _ = text("a").disabled(toggle.flag.computed());

    // Fires — `sole`'s only read is the erasure; the deletion moves it on
    // its last use, which is safe.
    let sole = Binding::bool(true);
    let _ = text("a").disabled(sole.computed());

    // Fires — the UFCS spelling borrows `x`; the rewrite moves it into
    // `disabled`, and `x` is read again, so the fix clones.
    let x = Binding::bool(true);
    let _ = text("a").disabled(SignalExt::computed(&x));
    let _ = x.get();

    // Fires, demoted — a comment inside the deleted span would be lost.
    let _ = text("a").disabled(
        flag.not() /* why */
            .computed(),
    );

    // Silent — `map` returns `Map<Self, _>`; dropping the erasure would
    // change the adapter's `Self` and its return type.
    let mapped = count.map(|v| v + 1).computed();
    let _ = mapped.map(|v| v * 2);

    // Silent — `kept` also reaches a concrete `Computed` parameter.
    let kept = flag.not().computed();
    takes_computed(kept.clone());
    let _ = text("a").disabled(kept);

    // Silent — the `let` annotation pins `Computed<bool>`.
    let pinned: Computed<bool> = flag.not().computed();
    let _ = text("a").disabled(pinned);

    // Silent — `unwrapped` is consumed through `Option::unwrap`, not a
    // pattern; what `unwrap` yields cannot be traced.
    let wrapped = channel.map(|_| flag.not().computed());
    let _ = text("a").disabled(wrapped.unwrap_or_else(|| flag.computed()));
}
