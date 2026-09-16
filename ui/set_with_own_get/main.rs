use std::ops::{Add, AddAssign};

use waterui::prelude::*;

struct State {
    count: Binding<i32>,
}

/// The operand is an `f64`, not a `Meters`, so `Binding::add_assign(other: T)`
/// does not apply and `Meters` extends nothing; `AddAssign<f64>` does apply,
/// so the `get_mut` deref-assign remains.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Meters(f64);

impl Add<f64> for Meters {
    type Output = Self;
    fn add(self, rhs: f64) -> Self {
        Self(self.0 + rhs)
    }
}

impl AddAssign<f64> for Meters {
    fn add_assign(&mut self, rhs: f64) {
        self.0 += rhs;
    }
}

fn main() {
    let count: Binding<i32> = binding(0_i32);
    let flag: Binding<bool> = binding(false);
    let name: Binding<String> = binding(String::new());
    let suffix = String::from("?");
    let ratio: Binding<f64> = binding(1.0_f64);
    let other: Binding<i32> = binding(0_i32);
    let meters: Binding<Meters> = binding(Meters(0.0));
    let state = State {
        count: binding(0_i32),
    };

    // Fires: `b.set(b.get() + x)` is the named `add_assign`.
    count.set(count.get() + 1);
    // Fires: a different operator, `mul_assign`.
    count.set(count.get() * 2);
    // Fires: `f64: Mul<Output = f64> + Clone`, `mul_assign`.
    ratio.set(ratio.get() * 0.5);
    // Fires: `b.set(!b.get())` on `Binding<bool>` toggles in place.
    flag.set(!flag.get());
    // Fires: `&str` is not a `String`, but `String: Extend<&str>` — `append`.
    name.set(name.get() + "!");
    // Fires: `&String` reaches `Add` as `&str`; the rewrite spells that
    // coercion out as `append(&*suffix)`.
    name.set(name.get() + &suffix);
    // Fires: the operand is an `f64`, so the named method is out and the
    // `AddAssign<f64>` deref-assign through `get_mut` remains.
    meters.set(meters.get() + 1.0);
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
