//! `handler_captures_binding` fixture: a `Binding`/`Computed`/reactive
//! `List` captured by a closure in a `Handler`/`HandlerOnce` position warns,
//! even through the clone-then-move block idiom; `Copy` captures, `State`
//! extractors, `on_change`, and non-handler captures stay silent.

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

    // Fires — the clone-then-move block idiom: `count` is cloned only to be
    // captured.
    let count: Binding<i32> = binding(0);
    let _ = button("+").action({
        let count = count.clone();
        move || count.set(1)
    });

    // Fires — two handles cloned into the block.
    let flag: Binding<bool> = binding(false);
    let _ = button("reset").action({
        let count = count.clone();
        let flag = flag.clone();
        move || {
            count.set(0);
            flag.set(false);
        }
    });

    let total: Computed<i32> = count.map(|n| n * 2).computed();
    // Fires — clone-then-move over a `Computed`.
    let _ = button("total").action({
        let total = total.clone();
        move || takes_i32(total.get())
    });
    // Silent — `State(total): State<Computed<i32>>` is the extractor remedy.
    let _ = button("total")
        .action(|State(total): State<Computed<i32>>| takes_i32(total.get()))
        .state(&total);
    // Fires — `total` is a `Computed`, a captured reactive handle too.
    let _ = button("total").action(move || takes_i32(total.get()));

    // Fires — `self.items` is cloned only to be captured.
    let _ = Row {
        items: ReactiveList::new(),
    }
    .view();

    // Fires — `extra` is captured, but the clone is also read before the
    // closure, so the `let` is not "only to be captured": no clone label,
    // generic help.
    let extra: Binding<i32> = binding(0);
    let _ = button("x").action({
        let extra = extra.clone();
        takes_i32(extra.get());
        move || extra.set(1)
    });

    // Fires — the clone-let sits in an outer wrapper block; the label and
    // the block-rewrite help still apply.
    let outer: Binding<i32> = binding(0);
    let _ = button("o").action({
        let outer = outer.clone();
        {
            let n = 1_i32;
            move || outer.set(n)
        }
    });
}

fn takes_i32(_: i32) {}

struct Row {
    items: ReactiveList<u32>,
}

impl Row {
    fn view(&self) -> impl View {
        button("clear").action({
            let items = self.items.clone();
            move || items.clear()
        })
    }
}
