//! `on_tap_on_control` fixture: a tap-gesture modifier (`.on_tap`,
//! `.on_tap_gesture`, `.on_tap_gesture_count`, `.gesture(TapGesture, ..)`)
//! on a control with its own activation warns; the same modifiers on plain
//! views or on a *modified* control, the control's own `.action`, and a
//! non-tap `.gesture` stay silent.

use waterui::Str;
use waterui::gesture::{LongPressGesture, TapGesture};
use waterui::prelude::*;

fn main() {
    let flag: Binding<bool> = binding(false);
    let value: Binding<f64> = binding(0.0);
    let count: Binding<i32> = binding(0);
    let name: Binding<Str> = binding(Str::from(""));

    // Fires — a tap on a `Button` races the button's own activation.
    let _ = button("a").on_tap(|| {});
    // Fires — `Toggle` flips on tap.
    let _ = toggle("a", &flag).on_tap(|| {});
    // Fires — `Slider` owns the tap/drag surface.
    let _ = slider("a", &value).on_tap_gesture(|| {});
    // Fires — `Stepper` activates on tap.
    let _ = stepper("a", &count).on_tap_gesture_count(2, || {});
    // Fires — `TextField` focuses on tap.
    let _ = TextField::new("a", &name).on_tap(|| {});
    // Fires — `ListItem` selects on tap; the `TapGesture` arg is what makes
    // `gesture` a tap.
    let _ = ListItem::new(text("a")).gesture(TapGesture::new(), || {});
    // Fires — `Menu` opens on tap.
    let _ = Menu::new("a", button("b")).on_tap(|| {});
    // Fires — `Picker` opens on tap.
    let _ = Picker::new("a", Computed::constant(Vec::new()), &count).on_tap(|| {});
    // Fires — `NavigationLink` pushes its destination on tap.
    let _ = NavigationLink::new("a", || navigation("b", text("c"))).on_tap(|| {});

    // Silent — the receiver is `Padding`, not the control.
    let _ = button("a").padding().on_tap(|| {});
    // Silent — `text` has no activation of its own.
    let _ = text("a").on_tap(|| {});
    // Silent — `.action` is the control's own activation, not a competing
    // gesture.
    let _ = button("a").action(|| {});
    // Silent — a stack has no activation.
    let _ = hstack((text("a"),)).on_tap(|| {});
    // Silent — a long-press gesture does not compete for the tap.
    let _ = button("a").gesture(LongPressGesture::new(500), || {});
}
