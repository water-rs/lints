//! `state_created_in_rebuilt_scope` / `state_created_in_row_builder` fixture:
//! reactive-state constructors inside `watch`, `when`, `.otherwise`, and
//! `Lazy::for_each` closures warn; state owned outside and moved in, a
//! constructor inside a nested `action` handler, a `.clone()` of an existing
//! binding, and top-level constructors stay silent.

#![allow(unknown_lints)]
#![warn(state_created_in_row_builder)]

use waterui::Identifiable;
use waterui::component::lazy::Lazy;
use waterui::prelude::*;
use waterui::reactive::collection::{Collection, List as ReactiveList};
use waterui::widget::condition::when;

#[derive(Identifiable, Clone)]
struct Item {
    #[id]
    id: u32,
}

fn main() {
    // Silent — `binding(..)`/`ReactiveList::from(..)` at the top level of
    // `main` are owned by `main`, not by a rebuilt scope.
    let count: Binding<i32> = binding(3);
    let flag: Binding<bool> = binding(false);
    let list = ReactiveList::from(vec![Item { id: 2 }]);

    // Fires — `binding` inside the `watch` closure.
    let _ = watch(count.clone(), |v| {
        let draft: Binding<String> = binding(String::new());
        vstack((
            text(draft),
            match v {
                0 => text("none"),
                _ => text("some"),
            },
        ))
    });

    // Fires — `Binding::i32` inside `when`'s `ViewBuilder` closure.
    let _ = when(flag.clone(), || {
        let n = Binding::i32(0);
        Text::display(n)
    });

    // Fires — `Binding::default` inside the `.otherwise` closure; the `when`
    // arm creates no state and stays silent.
    let _ = when(flag.clone(), || text("a")).otherwise(|| {
        let n: Binding<i32> = Binding::default();
        Text::display(n)
    });

    // Fires — `ReactiveList::from` inside `Dynamic::watch`.
    let _ = Dynamic::watch(count.clone(), |v| {
        let items = ReactiveList::from(vec![1]);
        vstack((
            text(items.len().to_string()),
            match v {
                0 => text("none"),
                _ => text("some"),
            },
        ))
    });

    // Fires (`state_created_in_row_builder`) — `binding` inside the
    // `Lazy::for_each` row builder.
    let _ = Lazy::for_each(list, |item| {
        let selected: Binding<bool> = binding(false);
        text(item.id.to_string()).visible(selected)
    });

    // Fires (`state_created_in_row_builder`) — `Binding::container` inside
    // `VStack::for_each`, the macro-emitted stack row builder.
    let other = ReactiveList::from(vec![Item { id: 3 }]);
    let _ = VStack::for_each(other, |item| {
        let seen: Binding<bool> = Binding::container(false);
        text(item.id.to_string()).visible(seen)
    });

    // Silent — the state is created outside and moved into the scope.
    let draft: Binding<String> = binding(String::new());
    let _ = watch(count.clone(), move |v| {
        vstack((
            text(draft.clone()),
            match v {
                0 => text("none"),
                _ => text("some"),
            },
        ))
    });

    // Silent — the `binding` sits inside a nested `action` handler, which
    // runs at event time, not when the scope rebuilds.
    let _ = watch(flag.clone(), |f| {
        button("x").action(move || {
            if f {
                let _: Binding<i32> = binding(0);
            }
        })
    });

    // Silent — `.clone()` of an existing binding creates no state.
    let _ = watch(count.clone(), move |v| {
        let _again = count.clone();
        match v {
            0 => text("none"),
            _ => text("some"),
        }
    });
}
