//! `collection_in_tuple_stack` fixture: a `for_each`/`List` collection as
//! an element of a `vstack`/`hstack`/`zstack` tuple argument warns; plain
//! views as elements, the collection as the stack's own content, and a
//! collection inside `scroll(..)` stay silent.

#![allow(dead_code)]

use waterui::Identifiable;
use waterui::component::lazy::Lazy;
use waterui::component::list::{List, ListItem};
use waterui::prelude::*;
use waterui::reactive::collection::List as ReactiveList;

#[derive(Identifiable, Clone)]
struct Item {
    #[id]
    id: u32,
}

fn row(item: Item) -> impl View {
    text(item.id.to_string())
}

fn main() {
    let items = ReactiveList::from(vec![Item { id: 1 }]);
    let other = ReactiveList::from(vec![Item { id: 2 }]);
    let more = ReactiveList::from(vec![Item { id: 3 }]);

    // Fires — the dogfood shape: `VStack::for_each` as a tuple element.
    let _ = vstack((text("header"), VStack::for_each(items.clone(), row)));

    // Fires — `Lazy::for_each` returns an opaque `impl View`.
    let _ = vstack((text("header"), Lazy::for_each(more.clone(), row)));

    // Fires — `hstack`/`zstack` tuples and `List::for_each` are the same
    // shape.
    let _ = hstack((
        text("header"),
        List::for_each(items.clone(), |item| ListItem::new(row(item))),
    ));
    let _ = zstack((text("header"), ZStack::for_each(other.clone(), row)));

    // Fires — a `List` value element, however it was built.
    let list = List::for_each(more.clone(), |item| ListItem::new(row(item)));
    let _ = vstack((text("header"), list));

    // Fires — a one-element tuple is still a tuple.
    let _ = vstack((Lazy::for_each(items.clone(), row),));

    // Silent — plain views as tuple elements.
    let _ = vstack((text("header"), text("body"), text("footer")));

    // Silent — inside `scroll(..)` the collection is the scroll view's
    // content.
    let _ = vstack((text("header"), scroll(VStack::for_each(other, row))));

    // Silent — `for_each` as the stack's own content, no tuple at all.
    let _ = VStack::for_each(more, row);
}
