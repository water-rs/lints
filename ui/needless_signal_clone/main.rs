//! `needless_signal_clone` fixture: `x.clone().<method>(..)` warns when
//! `<method>` is a `SignalExt` combinator or an inherent
//! `Binding`/`Computed`/`List` method borrowing `&self`; a clone feeding a
//! `self`-consuming call, a by-value parameter, or a local binding stays.

use std::ops::Not;
use waterui::prelude::*;
use waterui::reactive::collection::List;

struct Model {
    available: Binding<bool>,
}

fn make_flag() -> Binding<bool> {
    Binding::bool(true)
}

fn shared<T>(r: &mut T) -> &T {
    r
}

fn main() {
    let flag: Binding<bool> = Binding::bool(false);
    let other: Binding<bool> = Binding::bool(true);
    let count: Binding<i32> = Binding::i32(0);
    let mut a: Binding<i32> = Binding::i32(1);
    let b: Binding<i32> = Binding::i32(2);
    let x: Binding<i32> = Binding::i32(5);
    let maybe: Binding<Option<i32>> = Binding::container(Some(1));
    let model = Model {
        available: Binding::bool(true),
    };
    let list: List<i32> = List::new();

    // Fires — `SignalExt::select` borrows `&self`.
    let _ = flag.clone().select(0.45, 1.0);
    // Fires — the clone's receiver may be a field, not just a local.
    let _ = model.available.clone().map(|v| v as u8);
    // Fires — `SignalExt::computed` borrows `&self`.
    let _ = count.clone().computed();
    // Fires — inherent `Binding::unwrap_or` borrows `&self`.
    let _ = maybe.clone().unwrap_or(0);
    // Fires — `SignalExt::zip` borrows `&self`.
    let _ = a.clone().zip(&b);
    // Fires — the receiver may be a temporary; it can be used directly.
    let _ = make_flag().clone().map(|v| v as u8);
    // Fires — inherent `List::push` borrows `&self`.
    list.clone().push(3);
    // Fires — the fix deletes `.clone()` and the parens survive.
    let _ = (flag.clone()).select(0.45, 1.0);

    // Silent — `Add::add` consumes `self`; both clones are needed.
    let _ = a.clone() + b.clone();
    // Silent — `Computed::new` takes its argument by value.
    let _ = Computed::new(count.clone());
    // Silent — the clone is bound to a local before the combinator call.
    let s = count.clone();
    let _ = s.map(|v| v + 1);
    // Silent — `String::len` is not a signal method.
    let _ = String::new().clone().len();
    // Silent — with `Not` in scope, `not` on a `Binding<bool>` resolves to
    // `Not::not`, which consumes `self`. (Without the import it resolves to
    // `SignalExt::not` and this would fire.)
    let _ = flag.clone().not();

    // Silent — `flag.clone()` comes from the macro; a fix would edit the
    // macro's body, not this callsite.
    macro_rules! cloned_flag {
        () => {
            flag.clone()
        };
    }
    let _ = cloned_flag!().map(|v| v as u8);

    // Silent — `flag` moves into an argument while `&flag` is still
    // borrowed by the receiver; the clone ends that borrow early.
    let _ = flag.clone().select(flag, other);
    // Silent — the argument mutably borrows `a`, which would alias the
    // receiver's `&a` while `zip` runs; the clone ends that borrow first.
    let _ = a.clone().zip(shared(&mut a));
    // Silent — the `move` closure captures `x` by value.
    let _ = x.clone().map(move |v| x.set(v));
}
