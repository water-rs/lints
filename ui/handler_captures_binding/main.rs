//! `handler_captures_binding` fixture: a `Binding`/reactive `List` captured
//! by a closure in a `Handler`/`HandlerOnce` position warns; `Copy` captures,
//! `State` extractor parameters, `on_change`, and captures outside handler
//! positions stay silent.

use waterui::prelude::*;
use waterui::reactive::collection::List as ReactiveList;

fn takes(_: u32) {}

fn main() {
    // Fires — `count` is a `Binding` captured by `action`'s `Handler` parameter.
    let count: Binding<i32> = binding(3);
    let _ = button("+").action(move || count.set(1));

    // Fires — `flag` is captured by `on_tap`'s `Handler` parameter.
    let flag: Binding<bool> = binding(false);
    let _ = text("a").on_tap(move || flag.set(true));

    // Fires — `items` is a reactive `List`.
    let items: ReactiveList<u32> = ReactiveList::new();
    let _ = button("clear").action(move || items.clear());

    // Fires — one diagnostic with one label per capture.
    let count: Binding<i32> = binding(0);
    let flag: Binding<bool> = binding(false);
    let _ = button("reset").action(move || {
        count.set(0);
        flag.set(false);
    });

    // Fires — `action_async`'s `Handler` parameter; `count` is captured by the
    // outer closure and cloned into the async block so the handler stays `FnMut`.
    let count: Binding<i32> = binding(0);
    let _ = button("async").action_async(move || {
        let count = count.clone();
        async move { count.set(2) }
    });

    let count: Binding<i32> = binding(0);
    let flag: Binding<bool> = binding(false);

    // Silent — `n` is a `Copy` value, not a reactive handle.
    let n = 3_u32;
    let _ = button("n").action(move || takes(n));
    // Silent — `State` is the extractor remedy; the handler captures nothing.
    let _ = button("+")
        .action(|State(count): State<Binding<i32>>| count.set(1))
        .state(&count);
    // Silent — `on_change`'s handler parameter is a plain `Fn(T)`, not a `Handler`.
    let _ = text("x").on_change(&count, move |_| flag.set(true));
    // Silent — `SignalExt::map`'s `f` is a `Fn`, not a `Handler`.
    let _ = count.map(|n| n * 2);
    // Silent — `Box::new` is no handler position, though `count` is captured.
    let _setters: Vec<Box<dyn Fn()>> = vec![Box::new(move || count.set(9))];
}
