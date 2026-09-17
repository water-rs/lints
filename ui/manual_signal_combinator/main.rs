//! `manual_signal_combinator` fixture: a `signal.map(|v| ..)` whose
//! one-parameter closure restates a named `SignalExt` combinator fires;
//! anything else stays silent.
#![allow(unknown_lints)]

use std::rc::Rc;
use waterui::prelude::*;

fn helper() -> i32 {
    7
}

fn zero() -> i32 {
    0
}

fn mk_zero_fn() -> fn() -> i32 {
    zero
}

static ZERO_FN: fn() -> i32 = zero;

fn takes_i64(_: impl Signal<Output = i64>) {}

#[derive(Clone)]
struct Score(i32);

impl PartialEq<i32> for Score {
    fn eq(&self, other: &i32) -> bool {
        self.0 == *other
    }
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

    let float: Binding<f64> = Binding::f64(0.0);
    let unsigned: Binding<u32> = Binding::u32(0);
    let nested: Binding<Option<Option<i32>>> = binding(Some(Some(1)));
    let arr: Binding<[i32; 3]> = binding([1, 2, 3]);
    let vec: Binding<Vec<i32>> = binding(vec![1, 2]);
    let score: Binding<Score> = binding(Score(0));

    // Fires — `v != e` is `.equal_to(e).not()`.
    let _ = count.map(|v| v != 3);
    // Fires — `v == 0` on a numeric output is `.is_zero()`.
    let _ = count.map(|v| v == 0);
    // Fires — `v == 0.0` on a float output is `.is_zero()`.
    let _ = float.map(|v| v == 0.0);
    // Fires — `v > 0` on a signed output is `.is_positive()`.
    let _ = count.map(|v| v > 0);
    // Fires — `v < 0` on a signed output is `.is_negative()`.
    let _ = count.map(|v| v < 0);
    // Fires — `v > 0` on an unsigned output stays `.gt(0)`.
    let _ = unsigned.map(|v| v > 0);
    // Fires — `v.abs()` on a signed output is `.abs()`.
    let _ = count.map(|v| v.abs());
    // Fires — `v.unwrap_or(d)` on an `Option` output is `.unwrap_or(d)`.
    let _ = opt.map(|v| v.unwrap_or(0));
    // Fires — `v.unwrap_or_default()` on an `Option` output.
    let _ = opt.map(|v| v.unwrap_or_default());
    // Fires — `v.unwrap_or_else(f)` on an `Option` output. (A
    // non-constant body keeps stock `unnecessary_lazy_evaluations` quiet.)
    let _ = opt.map(|v| v.unwrap_or_else(|| helper() + 1));
    // Fires — `f` may be a `fn` path.
    let _ = opt.map(|v| v.unwrap_or_else(zero));
    // Fires — `v.unwrap_or(d)` on a `Result` output is
    // `.unwrap_or_result(d)`.
    let _ = res.map(|v| v.unwrap_or(0));
    // Fires — `v.unwrap_or_else(f)` on a `Result` output is
    // `.unwrap_or_else_result(f)`; `f` takes the error.
    let _ = res.map(|v| v.unwrap_or_else(|e| e.len() as i32));
    // Fires — `v == Some(e)` on an `Option` output is `.some_equal_to(e)`.
    let _ = opt.map(|v| v == Some(3));
    // Fires — `v == None` on an `Option` output is `.is_none()`. (Stock
    // `partialeq_to_none` wants `v.is_none()` inside the closure — the
    // lint under test rewrites the whole `map`.)
    #[allow(clippy::partialeq_to_none)]
    let _ = opt.map(|v| v == None);
    // Fires — `v != None` on an `Option` output is `.is_some()`.
    #[allow(clippy::partialeq_to_none)]
    let _ = opt.map(|v| v != None);
    // Fires — `v.flatten()` on an `Option<Option<T>>` output.
    let _ = nested.map(|v| v.flatten());
    // Fires — `v.ok()`/`v.err()` on a `Result` output.
    let _ = res.map(|v| v.ok());
    let _ = res.map(|v| v.err());
    // Fires — `v.map(f)` on an `Option` output is `.map_some(f)`.
    let _ = opt.map(|v| v.map(|x| x + 1));
    // Fires — `v.and_then(f)` on an `Option` output is `.and_then_some(f)`.
    // (A conditional body keeps stock `bind_instead_of_map` quiet.)
    let _ = opt.map(|v| v.and_then(|x| if x > 0 { Some(x + 1) } else { None }));
    // Fires — `v.map(f)` on a `Result` output is `.map_ok(f)`.
    let _ = res.map(|v| v.map(|x| x + 1));
    // Fires — `v.map_err(f)` on a `Result` output is `.map_err(f)`.
    let _ = res.map(|v| v.map_err(|e| e.len()));
    // Fires — `v.then_some(a)` on a `bool` output is `.then_some(a)`.
    let _ = flag.map(|v| v.then_some(3));
    // Fires — `if v { Some(a) } else { None }` is `.then_some(a)`.
    let _ = flag.map(|v| if v { Some(3) } else { None });
    // Fires — `v.into()` spells the inferred target: `count.map_into::<i64>()`.
    takes_i64(count.map(|v| v.into()));
    // Fires — `U::from(v)` names the conversion target. (Annotated params
    // keep stock `redundant_closure` quiet.)
    let _ = count.map(|v: i32| i64::from(v));
    // Fires — `Into::into(v)`/`From::from(v)` are `.map_into()` too.
    takes_i64(count.map(|v: i32| Into::into(v)));
    takes_i64(count.map(|v: i32| From::from(v)));
    // Fires — `w: i64` pins `U` through the outer closure; the fix spells
    // it: `count.map_into::<i64>()`.
    let _ = count.map(|v| v.into()).map(|w: i64| w + 1);
    // Fires — a generic `impl IntoSignalF32` argument does not pin `U`
    // itself, but the inferred `f32` is still spelled:
    // `level.map_into::<f32>()`.
    let level: Binding<u8> = binding(7u8);
    let _ = text("a").opacity(level.map(|v: u8| f32::from(v)));
    // Fires — `|v| v` is a needless map: a place receiver clones.
    let _ = count.map(|v| v);
    // Fires — `|v| v.clone()` is a needless map too.
    let _ = name.map(|v| v.clone());
    // Fires — a temporary receiver is kept verbatim.
    let _ = count.zip(&flag).map(|v| v);
    // Fires — an `Rc<Binding>` receiver derefs before cloning:
    // `(*shared).clone()`.
    let shared = Rc::new(Binding::i32(4));
    let _ = shared.map(|v| v);

    // Fires — `f` may also be a `static`/`const` callable path.
    let _ = opt.map(|v| v.unwrap_or_else(ZERO_FN));
    // Fires — a `move` `f` may capture a `Copy` value: it can only be
    // copied out, so the stored callable stays `Fn`.
    let add = 5;
    let _ = opt.map(move |v| v.map(move |x| x + add));

    // Silent — `v != e` where `e` mentions `v`.
    let _ = count.map(|v| v != v * 2);
    // Silent — `equal_to` stores the operand and evaluates it once, so a
    // call operand stays.
    let _ = count.map(|v| v == helper());
    // Silent — `some_equal_to` stores `e` once; `equal_to(Some(..))` would
    // store the same call, so the comparison stays.
    let _ = opt.map(|v| v == Some(helper()));
    // Silent — `f` is a call result, not a callable literal or path. (A
    // closure literal capturing a non-`Clone` or borrowed value cannot even
    // sit inside `map` — `SignalExt::map` already requires its `F` to be
    // `Clone + 'static`, which the capture would poison transitively.)
    let _ = opt.map(|v| v.unwrap_or_else(mk_zero_fn()));
    // Silent — `f` is a `&` borrow, not a stored callable literal or path.
    let _ = opt.map(|v| v.unwrap_or_else(&zero));
    // Silent — `f` reads the parameter inside its own closure.
    let _ = opt.map(|v| v.map(|x| x + v.unwrap_or(0)));
    // Silent — `f` captures `suffix` by reference, borrowing the frame —
    // a stored callable must be `'static`. (A `move` capture of a
    // non-`Clone` value cannot even sit inside `map`: the outer `Fn`
    // closure would have to move its own capture out.)
    let suffix = String::new();
    let _ = opt.map(move |v| v.map(|x| x + suffix.len() as i32));
    // Silent — `[T; N]::map`/`Iterator::map` are not signal combinators.
    let _ = arr.map(|v| v.map(|x| x + 1));
    let _ = vec.map(|v| v.into_iter().map(|x| x + 1));
    // Silent — `v == 0` where the output is a user type with
    // `PartialEq<i32>` — `0` is not the output type.
    let _ = score.map(|v| v == 0);
    // Silent — `then_some`'s operand is evaluated eagerly, so a call stays.
    let _ = flag.map(|v| v.then_some(helper()));
}
