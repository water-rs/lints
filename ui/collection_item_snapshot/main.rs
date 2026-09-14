//! `collection_item_snapshot` fixture: a non-signal field of a `for_each`
//! item read into an `IntoSignal`/`IntoComputed`/`IntoSignalF32` parameter
//! warns; `Binding` fields, text positions, literal conditions, a signal
//! derived from a live field, and `Iterator::map` closures stay silent.

use waterui::Identifiable;
use waterui::component::lazy::Lazy;
use waterui::component::list::{List, ListItem};
use waterui::prelude::*;
use waterui::reactive::collection::List as ReactiveList;
use waterui::widget::condition::when;

#[derive(Identifiable, Clone)]
struct Item {
    #[id]
    id: u32,
    unread: bool,
    alpha: f32,
    title: String,
    live: Binding<bool>,
}

fn item(id: u32) -> Item {
    Item {
        id,
        unread: true,
        alpha: 0.5,
        title: "a".to_string(),
        live: binding(true),
    }
}

fn main() {
    let list = ReactiveList::from(vec![item(1)]);

    // Fires — `row.unread` is a snapshot at `visible`'s `IntoComputed<bool>`
    // parameter.
    let _ = Lazy::for_each(list.clone(), |row| {
        text(row.title.clone()).visible(row.unread)
    });
    // Fires — `row.alpha` at `opacity`'s `IntoSignalF32` parameter.
    let _ = Lazy::for_each(list.clone(), |row| text("a").opacity(row.alpha));
    // Fires — `row.unread` at `when`'s `IntoComputed<bool>` condition.
    let _ = Lazy::for_each(list.clone(), |row| when(row.unread, || text("dot")));
    // Fires — `row.unread` at `disabled`'s `IntoComputed<bool>` parameter,
    // inside a `List::for_each` `ListItem` row.
    let _ = List::for_each(list.clone(), |row| {
        ListItem::new(text("a").disabled(row.unread))
    });
    // Fires — `unread` bound by destructuring is still a field snapshot.
    let _ = Lazy::for_each(list.clone(), |Item { unread, .. }| {
        text("a").visible(unread)
    });

    // Silent — `row.live` is a `Binding`: the field itself is live.
    let _ = Lazy::for_each(list.clone(), |row| text("a").visible(row.live.clone()));
    // Silent — a `String` field at `text`'s `IntoText` position is ordinary
    // row content.
    let _ = Lazy::for_each(list.clone(), |row| text(row.title.clone()));
    // Silent — a literal condition reads no field.
    let _ = Lazy::for_each(list.clone(), |_| text("a").visible(true));
    // Silent — `Iterator::map` is not a row builder.
    let _ = vec![item(2)]
        .into_iter()
        .map(|row| text("a").visible(row.unread))
        .collect::<Vec<_>>();
    // Silent — `row.live.map(..)` derives a signal from a live field.
    let _ = Lazy::for_each(list, |row| text("a").visible(row.live.map(|l| !l)));
}
