//! `spacer_in_zstack` fixture: a `Spacer` element in the contents tuple of
//! `zstack((..))`/`ZStack::new(.., (..))` warns — one diagnostic per spacer;
//! spacers inside `vstack`/`hstack` contents, a spacer nested one stack
//! deeper, and spacer-free `zstack` contents stay silent.

use waterui::prelude::*;

fn main() {
    // Fires — `spacer()` is a direct child of `zstack`.
    let _ = zstack((text("a"), spacer()));
    // Fires twice — `spacer()` and `spacer_min(8.0)` are both `Spacer`s.
    let _ = zstack((spacer(), text("a"), spacer_min(8.0)));
    // Fires — `ZStack::new` takes the same contents tuple.
    let _ = ZStack::new(Alignment::Center, (text("a"), spacer()));

    // Silent — `vstack` has a main axis the spacer expands along.
    let _ = vstack((text("a"), spacer(), text("b")));
    // Silent — `hstack` has a main axis.
    let _ = hstack((spacer(), text("a")));
    // Silent — the spacer is a grandchild inside a stack with an axis.
    let _ = zstack((text("a"), vstack((spacer(), text("b")))));
    // Silent — no spacer child.
    let _ = zstack((text("a"), text("b")));
}
