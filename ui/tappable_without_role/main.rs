//! `tappable_without_role` fixture: a tap-gesture modifier (`.on_tap`,
//! `.on_tap_gesture`, `.gesture(TapGesture, ..)`) on a non-control view whose
//! chain carries no `.a11y_role`/`.a11y_label` warns; a role or label
//! anywhere in the chain — below or above the tap — stays silent.

#![allow(unknown_lints)]
#![warn(tappable_without_role)]

use waterui::accessibility::AccessibilityRole;
use waterui::gesture::TapGesture;
use waterui::prelude::*;

fn main() {
    // Fires — plain `text` is not a control and the chain carries no role.
    let _ = text("a").on_tap(|| {});
    // Fires — a stack has no accessibility identity of its own.
    let _ = hstack((text("a"), text("b"))).on_tap_gesture(|| {});
    // Fires — `.padding()` sits between `text` and the tap; still no role.
    let _ = text("a").padding().on_tap(|| {});
    // Fires — `gesture` with a `TapGesture` argument is a tap.
    let _ = text("a").gesture(TapGesture::new(), || {});
    // Fires — `a11y_hidden` is not a role or a label.
    let _ = text("a").on_tap(|| {}).a11y_hidden(true);

    // Silent — role and label both sit below the tap.
    let _ = text("a")
        .a11y_role(AccessibilityRole::Button)
        .a11y_label("Open")
        .on_tap(|| {});
    // Silent — the role applied above the tap still covers it.
    let _ = text("a").on_tap(|| {}).a11y_role(AccessibilityRole::Button);
    // Silent — a label alone satisfies the rule.
    let _ = text("a").a11y_label("Open").on_tap(|| {});

    // `button("a").on_tap(..)` stays out of this fixture on purpose: the
    // receiver is a control so this lint is silent, but `on_tap_on_control`
    // fires on it — that case lives in `ui/on_tap_on_control/main.rs`.
}
