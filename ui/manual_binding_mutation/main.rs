//! `manual_binding_mutation` fixture: a `Binding` mutation or `set`
//! conversion written by hand where `Binding` has the named method warns;
//! multi-statement closures, `ref`/`ref mut` parameters, self reads inside
//! the operand, a bound `with_mut` result, a live `get_mut` guard, an owned
//! `to_owned` receiver, a plain `set`, and a `CustomBinding` receiver stay
//! silent.

use std::ops::{Add, AddAssign};

use waterui::Str;
use waterui::prelude::*;
use waterui::reactive::{Container, CustomBinding};

/// `Meters: Add<Meters> + Extend<Centimeters>` but no `AddAssign` — the
/// `*v = *v + x` shape stays clippy-clean while `Binding::add_assign`
/// applies, and `+= Centimeters` exercises the `append` fallback.
#[derive(Clone, Copy)]
struct Meters(f64);

#[derive(Clone, Copy)]
struct Centimeters(f64);

impl Add<Meters> for Meters {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign<Centimeters> for Meters {
    fn add_assign(&mut self, rhs: Centimeters) {
        self.0 += rhs.0;
    }
}

impl Extend<Centimeters> for Meters {
    fn extend<I: IntoIterator<Item = Centimeters>>(&mut self, iter: I) {
        for c in iter {
            self.0 += c.0;
        }
    }
}

fn main() {
    let title: Binding<Str> = binding(Str::from_static("init"));
    let name: Binding<String> = binding(String::new());
    let items: Binding<Vec<u32>> = binding(Vec::new());
    let flag: Binding<bool> = binding(false);
    let count: Binding<i32> = binding(0_i32);
    let meters: Binding<Meters> = binding(Meters(0.0));
    let s = String::from("x");
    let sm: &'static mut str = Box::leak(String::from("x").into_boxed_str());

    // Fires: `Str::from_static` is a `set` conversion — `set_from`.
    title.set(Str::from_static("untitled"));
    // Fires: `to_string` on `&str` — `set_from`.
    name.set("Ada".to_string());
    // Fires: `String::from` — `set_from`.
    name.set(String::from("Ada"));
    // Fires: `x.into()` — `set_from`.
    name.set("Ada".into());
    // Fires: `Into::into(x)` — `set_from`.
    name.set(Into::into("Ada"));
    // Fires: `From::from(x)` — `set_from`.
    name.set(From::from("Ada"));
    // Fires: `to_owned` on `&str` — `set_from`.
    name.set("Ada".to_owned());
    // Fires: `sm` reborrows `&mut str` to `&'static str` for
    // `from_static` — `set_from(&*sm)` renders that coercion for the
    // generic `Into`.
    title.set(Str::from_static(sm));
    // Fires: `v.push(x)` — `append(x)`.
    items.with_mut(|v| v.push(7));
    // Fires: `v.extend([x])` — `append(x)`.
    items.with_mut(|v| v.extend([7]));
    // Fires: `v.extend(core::iter::once(x))` — `append(x)`.
    items.with_mut(|v| v.extend(core::iter::once(7)));
    // Fires: `v.extend(Some(x))` — `append(x)`.
    items.with_mut(|v| v.extend(Some(7)));
    // Fires: `v.push_str(x)` — `append(x)`.
    name.with_mut(|v| v.push_str("?!"));
    // Fires: `&s` derefs to `&str` for `push_str` — `append(&*s)`.
    name.with_mut(|v| v.push_str(&s));
    // Fires: `String::push(c)` — `append(c)`.
    name.with_mut(|v| v.push('!'));
    // Fires: `*v = !*v` on `Binding<bool>` — `toggle()`.
    flag.with_mut(|v| *v = !*v);
    // Fires: `*v += 1` — `add_assign(1)`.
    count.with_mut(|v| *v += 1);
    // Fires: `*b.get_mut() += 1` — `add_assign(1)`.
    *count.get_mut() += 1;
    // Fires: `*v = *v + x` — `add_assign(x)`.
    meters.with_mut(|v| *v = *v + Meters(1.0));
    // Fires: `*v = v.clone() + x` with `x: &str` — `append(x)`.
    name.with_mut(|v| *v = v.clone() + "!");
    // Fires: `*v += x` where `x` is the `Extend` element — `append(x)`.
    meters.with_mut(|v| *v += Centimeters(1.0));
    // Fires: `*v = x` — `set(x)`.
    count.with_mut(|v| *v = 9);
    // Fires: `*b.get_mut() = x` — `set(x)`.
    *count.get_mut() = 9;
    // Fires: a one-statement block body — `add_assign(1)`.
    count.with_mut(|v| {
        *v += 1;
    });
    // Fires: a `while` body tail drops the `()` like a `loop` tail —
    // `set(0)`.
    let mut done = false;
    while !done {
        done = true;
        count.with_mut(|v| *v = 0)
    }

    // Silent: `b.set(x)` is already the named method.
    count.set(5);
    // Silent: `x.clone()` is not a `set_from` conversion.
    name.set(s.clone());
    // Silent: two statements in the `with_mut` closure.
    count.with_mut(|v| {
        *v += 1;
        *v += 2;
    });
    // Silent: the operand mentions the closure parameter.
    count.with_mut(|v| *v += *v);
    // Silent: the `with_mut` result is bound.
    let _pair = (count.with_mut(|v| *v += 1), 0);
    // Silent: the closure returns a value the caller binds.
    let _v = count.with_mut(|v| *v);
    // Silent: `*b.get_mut() = b.get() + 1` reads the binding back —
    // `set_with_own_get`'s case.
    *count.get_mut() = count.get() + 1;
    // Silent: the guard lives to the end of the enclosing expression —
    // `set(9)` would publish before `count.get()` observes the old value.
    let _ = (*count.get_mut() = 9, count.get());
    // Silent: `s.to_owned()` borrows an owned `String`; `set_from(s)`
    // would move it.
    name.set(s.to_owned());
    // Silent: `|ref v|` binds `v: &&mut String` — no mutation shape can
    // reach the value through it.
    name.with_mut(|ref v| v.is_empty());
    // Silent: `|ref mut v|` binds `v: &mut &mut String` — `*v` is the
    // `&mut String` slot, not the `String`; `set(slot)` would not
    // typecheck.
    let slot: &'static mut String = Box::leak(Box::new(String::new()));
    name.with_mut(|ref mut v| *v = slot);
    // Silent: `Container` is a `CustomBinding` impl — its `set` is the
    // trait method, not `Binding::set`.
    let custom = Container::new(0_i32);
    custom.set(1);
}
