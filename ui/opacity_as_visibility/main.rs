use waterui::prelude::*;

fn main() {
    // Fires: literal `0.0` — the view is invisible but still announced and tappable.
    let _ = text("a").opacity(0.0);
    // Fires: integer literal `0` — same zero opacity.
    let _ = text("a").opacity(0);

    // Fires: `select(1.0, 0.0)` — the signal is already "is visible".
    let shown: Binding<bool> = binding(true);
    let _ = text("a").opacity(shown.select(1.0, 0.0));
    // Fires: `select(0.0, 1.0)` — inverted; the fix negates the signal.
    let hidden: Binding<bool> = binding(true);
    let _ = text("a").opacity(hidden.select(0.0, 1.0));
    // Fires: `select(1, 0)` — integer literals toggle the same way.
    let int_select: Binding<bool> = binding(true);
    let _ = text("a").opacity(int_select.select(1, 0));
    // Fires: `map` to `1.0`/`0.0` — the signal is already "is visible".
    let mapped: Binding<bool> = binding(true);
    let _ = text("a").opacity(mapped.map(|b| if b { 1.0 } else { 0.0 }));
    // Fires: `map` with swapped branches — the fix negates the signal.
    let swapped: Binding<bool> = binding(true);
    let _ = text("a").opacity(swapped.map(|b| if b { 0.0 } else { 1.0 }));
    // Fires: a field receiver cannot be moved out — the fix clones it.
    let holder = Holder {
        flag: binding(true),
    };
    let _ = text("a").opacity(holder.flag.select(1.0, 0.0));
    // Fires: a local still used afterwards cannot be moved — the fix clones it.
    let reused: Binding<bool> = binding(true);
    let _ = text("a").opacity(reused.select(1.0, 0.0));
    let _ = text("b").visible(reused);

    let flag: Binding<bool> = binding(true);
    // Silent: a half-transparent view is styling, not hiding.
    let _ = text("a").opacity(0.5);
    // Silent: `select` between non-binary opacities is genuine modulation.
    let _ = text("a").opacity(flag.select(1.0, 0.3));
    // Silent: `map` to opacities other than `0.0`/`1.0`.
    let _ = text("a").opacity(flag.map(|b| if b { 0.8 } else { 0.2 }));
    // Silent: `.visible` is the remedy, not the bug.
    let _ = text("a").visible(flag.clone());
}

struct Holder {
    flag: Binding<bool>,
}
