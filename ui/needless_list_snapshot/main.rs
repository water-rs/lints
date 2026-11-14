//! `needless_list_snapshot` fixture: a `snapshot()` that feeds one `List`
//! operation — `len`, `is_empty`, `get(i).cloned()`, `first().cloned()`,
//! `into_iter`, or a `for` head — warns; a snapshot bound to a `let`,
//! mutated, returned, indexed, iterated by reference, or read by any
//! other method stays silent.
//!
//! The top-level module exercises the help-only path: `Collection` is not
//! in scope, so `len`/`is_empty`/`get(i).cloned()`/`first().cloned()`
//! emit a `help` naming the rewrite and the import instead of a
//! suggestion. `mod with_trait` exercises the `MachineApplicable` path.

#![allow(unknown_lints)]
#![warn(needless_list_snapshot)]

use waterui::reactive::collection::List as ReactiveList;

fn keep(list: &ReactiveList<u32>, take: bool) -> Vec<u32> {
    if take {
        // Silent — the snapshot is the return value.
        return list.snapshot();
    }
    Vec::new()
}

fn takes_ref(list: &ReactiveList<u32>) {
    // Fires — `len` reads through `&List` the same way; help-only here.
    let _ = list.snapshot().len();
    // Fires — `&List` receivers spell the same `list.iter()`: the call
    // auto-refs and returns the owned iterator.
    for x in list.snapshot() {
        let _ = x;
    }
}

fn main() {
    let list = ReactiveList::from(vec![1u32, 2, 3]);

    // Fires — `len` is `Collection::len` on the list itself; help-only
    // here because `Collection` is not in scope.
    let _ = list.snapshot().len();
    // Fires — `is_empty` too.
    let _ = list.snapshot().is_empty();
    // Fires — `get(i).cloned()` is `list.get(i)`; help-only, and
    // necessarily so — with `Collection` in scope this line resolves
    // `get` to `Collection::get` on `Vec` (`Option<u32>`) and the
    // `.cloned()` does not compile at all.
    let _ = list.snapshot().get(1).cloned();
    // Fires — `first().cloned()` is `list.get(0)`; help-only here.
    let _ = list.snapshot().first().cloned();
    // Fires — `into_iter` is `list.iter()`; both are `vec::IntoIter<T>`.
    let _ = list.snapshot().into_iter();
    // Fires — `for x in list.iter()`; `List::iter` returns the owned
    // iterator, so the borrow ends at the call and the body may still
    // move or `&mut`-borrow the list.
    for x in list.snapshot() {
        let _ = x;
    }

    // Silent — `snapshot().iter()` yields `&T` where `List::iter` yields
    // owned `T`; no rewrite is type-preserving.
    let _ = list.snapshot().iter();
    // Silent — a `let`-bound snapshot is a held copy, however it is read.
    let items = list.snapshot();
    let _ = items.len();
    // Silent — indexing panics where `get` returns `None`.
    let _ = list.snapshot()[0];
    // Silent — the snapshot is mutated.
    list.snapshot().retain(|x| *x > 1);
    // Silent — `contains` is not an operation `List` exposes.
    let _ = list.snapshot().contains(&2);
    // Silent — `get` without `.cloned()` borrows the snapshot.
    let _ = list.snapshot().get(1);
    // Silent — `first` without `.cloned()` too.
    let _ = list.snapshot().first();

    takes_ref(&list);
    let _ = keep(&list, true);
    with_trait::scoped(&list);
}

mod with_trait {
    use waterui::reactive::collection::Collection;
    use waterui::reactive::collection::List as ReactiveList;

    pub fn scoped(list: &ReactiveList<u32>) {
        // `Collection` in scope means the `List` methods spell out — this
        // is what the rewrites below produce.
        let _ = list.len();
        // Fires — `Collection` is in scope, so `list.len()` is
        // machine-applicable here.
        let _ = list.snapshot().len();
        // Fires — `is_empty` likewise.
        let _ = list.snapshot().is_empty();
        // Fires — `first().cloned()` likewise. (`get(i).cloned()` cannot
        // appear in this module — with `Collection` in scope `get`
        // resolves to `Collection::get` on `Vec` and the line does not
        // compile.)
        let _ = list.snapshot().first().cloned();
        // Fires — `into_iter` needs no trait and stays machine-applicable
        // everywhere.
        let _ = list.snapshot().into_iter();
        // Fires — same for the `for` head.
        for x in list.snapshot() {
            let _ = x;
        }
    }
}
