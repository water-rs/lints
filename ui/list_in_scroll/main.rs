//! `list_in_scroll` fixture: a `List`/`ListBuilder` passed to `scroll`,
//! `scroll_horizontal`, or `ScrollView::vertical`/`horizontal` — bare or under
//! `Metadata` modifiers — warns; stacks, `Lazy::for_each`, a sizing wrapper,
//! and a `List` on its own stay silent.

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

fn main() {
    let items = ReactiveList::from(vec![Item { id: 1 }]);

    // Fires — a `List` is the scroll view's content.
    let _ = scroll(List::for_each(items.clone(), |_| ListItem::new(text("a"))));
    // Fires — `Metadata` modifiers (`.on_appear`) are layout-transparent, the
    // list underneath still scrolls itself.
    let _ = scroll(List::for_each(items.clone(), |_| ListItem::new(text("a"))).on_appear(|| {}));
    // Fires — `.editing(..)` trades the `List` for a `ListBuilder`.
    let _ = scroll(List::for_each(items.clone(), |_| ListItem::new(text("a"))).editing(false));
    // Fires — `scroll_horizontal` is the horizontal spelling.
    let _ = scroll_horizontal(List::content((|| ListItem::new(text("a")),)));
    // Fires — `ScrollView::vertical` is the constructor `scroll(..)` wraps.
    let _ = ScrollView::vertical(List::for_each(items.clone(), |_| ListItem::new(text("a"))));
    // Fires — `ScrollView::horizontal` likewise.
    let _ = ScrollView::horizontal(List::for_each(items.clone(), |_| ListItem::new(text("a"))));

    // Silent — a `VStack` does not scroll itself.
    let _ = scroll(vstack((text("a"), text("b"))));
    // Silent — `Lazy::for_each` is the remedy inside a scroll view.
    let _ = scroll(Lazy::for_each(items.clone(), |_| text("a")));
    // Silent — `.padding()` sizes the list; the case is different and stays
    // silent by decision.
    let _ = scroll(List::for_each(items.clone(), |_| ListItem::new(text("a"))).padding());
    // Silent — a `List` on its own scrolls itself.
    let _ = List::for_each(items, |_| ListItem::new(text("a")));
}
