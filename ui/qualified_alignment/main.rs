//! `qualified_alignment` fixture: `.alignment(X::CONST)` on a container
//! warns to the position method, `X::CONST` at any other
//! `impl Into<..>` alignment parameter warns to the bare token; token
//! arguments, `let`-bound alignments, `const` items, and non-constant
//! expressions stay silent.

use waterui::prelude::*;

/// A non-container `impl Into<Alignment>` sink — the fix names the token.
fn takes(_: impl Into<Alignment>) {}

/// A custom alignment value — a `const` item, not an associated constant.
const MY: HorizontalAlignment = HorizontalAlignment::Leading;

/// `T` occurs twice across the inputs and again in the output — rewriting
/// `Alignment::Center` would shift inference, so the lint stays silent.
fn reused<T: Into<Alignment>>(a: T, b: T) -> T {
    let _: Alignment = b.into();
    a
}

/// `T` occurs in the output — same reason for silence.
fn returned<T: Into<Alignment>>(a: T) -> T {
    a
}

fn main() {
    // Fires — `vstack` aligns horizontally; `Leading` is `.leading()`.
    let _ = vstack((text("a"),)).alignment(HorizontalAlignment::Leading);

    // Fires — `hstack` aligns vertically; `Top` is `.top()`.
    let _ = hstack((text("a"),)).alignment(VerticalAlignment::Top);

    // Fires — `zstack` is two-dimensional; `TopLeading` is `.top_leading()`.
    let _ = zstack((text("a"),)).alignment(Alignment::TopLeading);

    // Fires — `Center` maps to `.centered()`.
    let _ = vstack((text("a"),)).alignment(HorizontalAlignment::Center);

    // Fires — `FirstBaseline` is `.first_baseline()`.
    let _ = hstack((text("a"),)).alignment(VerticalAlignment::FirstBaseline);

    // Fires — `.overlay(..)` yields `Overlay`, a two-dimensional container.
    let _ = text("a")
        .overlay(text("b"))
        .alignment(Alignment::BottomTrailing);

    // Fires — `Frame` is a two-dimensional container.
    let _ = frame::Frame::new(text("a")).alignment(Alignment::Center);

    // Fires — `Grid` is a two-dimensional container.
    let _ = grid(1, [GridRow::new((text("a"),))]).alignment(Alignment::Top);

    // Fires — `OverlayLayout` is a `Layout`, not a view container: it has
    // no position methods, so the fix is the token — `.alignment(Top)`.
    let _ = OverlayLayout::default().alignment(Alignment::Top);

    // Fires — a non-container `impl Into<Alignment>` position takes the
    // bare token; the prelude already binds `Center`.
    takes(Alignment::Center);

    // Silent — the argument is already the token.
    let _ = vstack((text("a"),)).alignment(Leading);
    let _ = hstack((text("a"),)).alignment(Center);

    // Silent — a `let`-bound alignment is not a constant path.
    let a = HorizontalAlignment::Leading;
    let _ = vstack((text("a"),)).alignment(a);

    // Silent — a `const` item is not an associated-constant path.
    let _ = vstack((text("a"),)).alignment(MY);

    // Silent — a non-constant expression.
    let c = true;
    let _ = vstack((text("a"),)).alignment(if c {
        HorizontalAlignment::Leading
    } else {
        HorizontalAlignment::Trailing
    });

    // Silent — `reused`'s `T` occurs twice across the inputs and in the
    // output; `returned`'s `T` occurs in the output.
    let _ = reused(Alignment::Center, Alignment::Trailing);
    let _ = returned(Alignment::Center);

    no_prelude::g();
    value_conflict::g();
    type_conflict::g();
}

/// No prelude glob — `Center` is unbound here, so the fix inserts
/// `use waterui::layout::Center;` and rewrites to `f(Center)`
/// (`MachineApplicable`).
mod no_prelude {
    use waterui::layout::{Alignment, stack::ZStack};

    fn f(_: impl Into<Alignment>) {}

    pub fn g() {
        let _z: Option<ZStack<()>> = None;
        f(Alignment::Center)
    }
}

/// `Center` is a unit struct — bound in `ValueNS` as well as `TypeNS` —
/// so the fix imports the token under an alias (`MaybeIncorrect`).
mod value_conflict {
    use waterui::layout::Alignment;

    struct Center;

    fn f(_: impl Into<Alignment>) {}

    pub fn g() {
        let _ = Center;
        f(Alignment::Center)
    }
}

/// `Center` is a braced struct — bound only in `TypeNS`, so `ValueNS`
/// looks free, but inserting `use waterui::layout::Center;` would collide
/// (E0255); the fix still takes the alias arm (`MaybeIncorrect`).
mod type_conflict {
    use waterui::layout::Alignment;

    struct Center {
        x: u8,
    }

    fn f(_: impl Into<Alignment>) {}

    pub fn g() {
        let _ = Center { x: 0 }.x;
        f(Alignment::Center)
    }
}
