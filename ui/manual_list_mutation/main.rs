//! `manual_list_mutation` fixture: a snapshot–mutate–replace round trip on a
//! `List` warns when the one mutation between `snapshot` and `replace` is a
//! method `List` exposes (`push`/`pop`/`insert`/`remove`/`clear`/`sort`)
//! with its result discarded, and `replace` of an empty vector warns as
//! `List::clear`. Two mutations, a bound mutation result, a snapshot read
//! after `replace`, a method `List` lacks (`retain`), a used `replace`
//! result, a `replace` on a different list, a statement-carrying tail or
//! receiver block, and a rebinding argument stay silent; a comment inside
//! the replaced span still warns but the fix drops to `MaybeIncorrect`.
//!
//! `List::replace` is `#[must_use]`, so most rows discard through
//! `let _ =`; `bare_replace` carries `#[expect(unused_must_use)]` for the
//! issue's canonical bare `list.replace(v);` spelling.

#![allow(unknown_lints)]
#![warn(manual_list_mutation)]

use waterui::reactive::collection::List as ReactiveList;

struct State {
    items: ReactiveList<u32>,
}

fn touch() {}

fn main() {
    let mut list = ReactiveList::from(vec![1u32, 2, 3]);

    // Fires — collapses to `list.push(9);`.
    let mut v = list.snapshot();
    v.push(9);
    let _ = list.replace(v);

    // Fires — `let _ =` discard on `pop` collapses to `let _ = list.pop();`
    // (`List::pop` is `#[must_use]`).
    let mut v = list.snapshot();
    let _ = v.pop();
    let _ = list.replace(v);

    // Fires — `List::remove` is `#[must_use]`, so the bare discard
    // collapses to `let _ = list.remove(0);`.
    let mut v = list.snapshot();
    v.remove(0);
    let _ = list.replace(v);

    // Fires — collapses to `list.insert(0, 9);`.
    let mut v = list.snapshot();
    v.insert(0, 9);
    let _ = list.replace(v);

    // Fires — collapses to `list.clear();`.
    let mut v = list.snapshot();
    v.clear();
    let _ = list.replace(v);

    // Fires — collapses to `list.sort();`.
    let mut v = list.snapshot();
    v.sort();
    let _ = list.replace(v);

    // Fires — the block spelling; collapses to `list.push(8);`.
    let _ = list.replace({
        let mut v = list.snapshot();
        v.push(8);
        v
    });

    // Fires — block-spelling `remove` inside `let _ =`: the binding
    // already discards, so the rewrite is `list.remove(0)` — not
    // `let _ = list.remove(0)`.
    let _ = list.replace({
        let mut v = list.snapshot();
        v.remove(0);
        v
    });

    // Fires — `list.replace(Vec::new())` is `list.clear()`.
    let _ = list.replace(Vec::new());
    // Fires — `vec![]` lowers to `Vec::new()`.
    let _ = list.replace(vec![]);
    // Fires — `Default::default()` in `Vec` position.
    let _ = list.replace(Default::default());
    // Fires — `Vec::default()` resolves to `Default::default`.
    let _ = list.replace(Vec::default());

    // Fires — field receiver; collapses to `state.items.push(5);`.
    let state = State {
        items: ReactiveList::from(vec![0u32]),
    };
    let mut v = state.items.snapshot();
    v.push(5);
    let _ = state.items.replace(v);

    // Fires at `MaybeIncorrect` — the comment inside the replaced span
    // would be deleted by the fix.
    let mut v = list.snapshot();
    // a comment the suggestion would delete
    v.push(6);
    let _ = list.replace(v);

    // Silent — two mutations between the snapshot and the `replace`.
    let mut v = list.snapshot();
    v.push(1);
    v.push(2);
    let _ = list.replace(v);

    // Silent — the mutation's result is bound, not discarded.
    let mut v = list.snapshot();
    let last = v.pop();
    let _ = list.replace(v);
    let _ = last;

    // Silent — `v` is read after the `replace` (reinitialized first, since
    // `replace` moved it).
    let mut v = list.snapshot();
    v.push(7);
    let _ = list.replace(v);
    v = Vec::new();
    let _ = v.len();

    // Silent — `retain` has no `List` counterpart.
    let mut v = list.snapshot();
    v.retain(|x| *x > 1);
    let _ = list.replace(v);

    // Silent — the `replace` result (the previous vector) is used.
    let mut v = list.snapshot();
    v.push(6);
    let old = list.replace(v);
    let _ = old;

    // Silent — the `replace` runs on a different list than the snapshot.
    let other = ReactiveList::from(vec![0u32]);
    let mut v = list.snapshot();
    v.push(4);
    let _ = other.replace(v);

    // Silent — the argument block's tail is itself a block with
    // statements; the rewrite would delete `touch()`.
    let _ = list.replace({
        let mut v = list.snapshot();
        v.push(1);
        {
            touch();
            v
        }
    });

    // Silent — the mutation's receiver is a block with statements; the
    // rewrite would delete `touch()`.
    let mut v = list.snapshot();
    {
        touch();
        &mut v
    }
    .push(1);
    let _ = list.replace(v);

    // Silent — the mutation argument rebinds `list` between the snapshot's
    // receiver evaluation and the `replace`'s.
    let mut v = list.snapshot();
    v.push({
        list = ReactiveList::from(vec![4u32]);
        7
    });
    let _ = list.replace(v);

    bare_replace();
}

/// The issue's canonical spelling — `list.replace(v);` as a bare
/// statement. `List::replace` is `#[must_use]`, so the whole fn carries
/// the expectation.
#[expect(
    unused_must_use,
    reason = "the issue's canonical bare `replace` statement"
)]
fn bare_replace() {
    let list = ReactiveList::from(vec![1u32, 2, 3]);

    // Fires — bare `list.replace(v);` collapses to `list.push(2);`.
    let mut v = list.snapshot();
    v.push(2);
    list.replace(v);

    // Fires — the bare block spelling; the suggestion fills the call's
    // expression slot, producing `list.push(3);`.
    list.replace({
        let mut v = list.snapshot();
        v.push(3);
        v
    });

    // Fires — `pop` under a bare statement spells `let _ =` in the fix:
    // `let _ = list.pop();`.
    list.replace({
        let mut v = list.snapshot();
        v.pop();
        v
    });
}
