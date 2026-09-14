//! `fixed_children_in_vec` / `push_loop_seed` fixture: `vstack`/`hstack`/
//! `zstack`/`VStack::new` calls whose contents are a literal `vec![..]` warn
//! with a tuple suggestion, and a `ReactiveList::new()` seeded by a
//! same-block `for`/`push` loop warns; tuple contents, dynamically built
//! vectors, the `vec![x; n]` repeat form, `ReactiveList::from(..)`, and loops
//! that do more than push stay silent.

#![allow(unknown_lints)]
#![warn(fixed_children_in_vec)]
#![warn(push_loop_seed)]

use waterui::prelude::*;
use waterui::reactive::collection::List as ReactiveList;

fn main() {
    // Fires — a literal, fixed set of children; the fix is the tuple.
    let _ = vstack(vec![text("a"), text("b"), text("c")]);

    // Fires — one child still takes the one-element tuple `(a,)`.
    let _ = hstack(vec![text("a")]);

    // Fires — `zstack` matches too.
    let _ = zstack(vec![text("a"), text("b")]);

    // Fires — `VStack::new`'s contents is its last argument.
    let _ = VStack::new(
        HorizontalAlignment::Center,
        10.0,
        vec![text("a"), text("b")],
    );

    // Silent — a tuple is already the fixed-set shape.
    let _ = vstack((text("a"), text("b")));

    // Silent — `names` is dynamic data collected into a `Vec`, not a literal.
    let names: Vec<String> = vec!["a".to_string(), "b".to_string()];
    let _ = vstack(names.iter().map(|n| text(n.clone())).collect::<Vec<_>>());

    // Silent — the `vec![x; n]` repeat form is out of scope.
    let _ = vstack(vec![text("a"); 3]);

    // Fires (push_loop_seed) — `new`, then a same-block `for` of pushes.
    let items = vec![1, 2, 3];
    let list: ReactiveList<u32> = ReactiveList::new();
    for i in items {
        list.push(i);
    }

    // Fires (push_loop_seed) — the same shape iterating `&more`.
    let more = vec![4, 5];
    let other: ReactiveList<u32> = ReactiveList::new();
    for i in &more {
        other.push(*i);
    }

    // Silent — `ReactiveList::from` already seeds in one move.
    let _ = ReactiveList::from(vec![1, 2]);

    // Silent — the loop body is a conditional, not a single `push`.
    let conditional: ReactiveList<u32> = ReactiveList::new();
    for i in [7, 8, 9] {
        if i > 7 {
            conditional.push(i);
        }
    }
}
