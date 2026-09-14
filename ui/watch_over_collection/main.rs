//! `watch_over_collection` fixture: a `watch`/`Dynamic::watch` whose watched
//! value's type implements `Collection` warns, whatever `f` is; scalar and
//! `Option` values, `Lazy::for_each`, and non-`watch` calls stay silent.

use waterui::Identifiable;
use waterui::component::lazy::Lazy;
use waterui::prelude::*;
use waterui::reactive::collection::List as ReactiveList;

#[derive(Identifiable, Clone)]
struct Item {
    #[id]
    id: u32,
}

/// A named `f`: the defect is the watched type, not what `f` does with it.
fn render(items: Vec<Item>) -> impl View {
    text(items.len().to_string())
}

fn main() {
    let items: Binding<Vec<Item>> = binding(vec![Item { id: 1 }]);
    let names: Computed<Vec<String>> = Computed::constant(vec!["a".to_string()]);
    let fixed: Binding<[i32; 3]> = binding([1, 2, 3]);
    let count: Binding<i32> = binding(3);
    let name: Binding<String> = binding("n".to_string());
    let selected: Binding<Option<Item>> = binding(None);
    let list = ReactiveList::from(vec![Item { id: 2 }]);

    // Fires — `Vec<Item>` is a `Collection`.
    let _ = watch(items.clone(), |items| {
        vstack((text(items.len().to_string()),))
    });
    // Fires — `Dynamic::watch` over a `Computed<Vec<String>>`.
    let _ = Dynamic::watch(names, |names| vstack((text(names.len().to_string()),)));
    // Fires — `f` as a named fn still watches a collection.
    let _ = watch(items.clone(), render);
    // Fires — `[i32; 3]` is a `Collection`.
    let _ = watch(fixed, |fixed| text(fixed.len().to_string()));

    // Silent — `i32` is not a collection.
    let _ = watch(count.clone(), |count| text(count.to_string()));
    // Silent — `String` is not a collection.
    let _ = watch(name, |name| text(name.len().to_string()));
    // Silent — `Option<Item>` is not a collection.
    let _ = watch(selected, |selected| text(selected.is_some().to_string()));
    // Silent — `Lazy::for_each` over a reactive `List` is the remedy, not a `watch`.
    let _ = Lazy::for_each(list, |item| text(item.id.to_string()));
    // Silent — `map` on a signal is not `watch`.
    let _ = count.map(|_| vec![1]);
}
