use waterui::prelude::*;

struct State {
    count: Binding<i32>,
}

fn main() {
    let count: Binding<i32> = binding(0_i32);
    let flag: Binding<bool> = binding(false);
    let name: Binding<String> = binding(String::new());
    let other: Binding<i32> = binding(0_i32);
    let state = State {
        count: binding(0_i32),
    };

    // Fires: `b.set(b.get() + x)` mutates through `get_mut` instead.
    count.set(count.get() + 1);
    // Fires: a different compound-assignable operator.
    count.set(count.get() * 2);
    // Fires: `b.set(!b.get())` on `Binding<bool>` toggles in place.
    flag.set(!flag.get());
    // Fires: `String: AddAssign<&str>`, so the suggestion stays applicable.
    name.set(name.get() + "!");
    // Fires: not a bare `get() op x`, so the general `with_mut` rewrite.
    count.set(count.get().max(0) + 1);
    // Fires: two reads of the same binding inside one argument.
    count.set(if count.get() > 3 { 0 } else { count.get() });
    // Fires: the receiver is a struct field.
    state.count.set(state.count.get() + 1);

    // Silent: the `.get()` is on a different binding.
    count.set(other.get() + 1);
    // Silent: no `.get()` in the argument.
    count.set(5);
    // Silent: already an in-place `get_mut` mutation.
    *count.get_mut() += 1;
    // Silent: already `with_mut`.
    count.with_mut(|v| *v += 1);
    // Silent: the read happens outside the `set` argument.
    let n = count.get();
    count.set(n + 1);
}
