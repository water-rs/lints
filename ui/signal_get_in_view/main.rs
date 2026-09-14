use waterui::handler::AnyViewBuilder;
use waterui::prelude::*;
use waterui::signal::{IntoComputed, IntoSignal};
use waterui::text::IntoText;

/// `.get()` in a plain function is a legitimate read — the lint keys on the
/// callee's parameter bounds, so nothing here is flagged.
fn count_now(signal: &Computed<i32>) -> i32 {
    signal.get()
}

/// A local type with an `IntoText` impl: a struct literal carrying a `.get()`
/// field still reports.
struct Wrapped {
    value: String,
}

impl IntoText for Wrapped {
    fn into_text(self) -> Text {
        text(self.value)
    }
}

/// The bound is on the callee's parameter, so plain generic functions fire
/// the same way waterui's own constructors do.
fn takes_signal(value: impl IntoSignal<f32>) -> Computed<f32> {
    value.into_signal().computed()
}

fn takes_computed(value: impl IntoComputed<i32>) -> Computed<i32> {
    value.into_computed()
}

fn main() {
    let fade: Binding<f32> = binding(0.5_f32);
    let count: Binding<i32> = binding(3_i32);
    let name: Binding<String> = binding("hello".to_string());

    // Fires: `.get()` snapshot passed to a parameter bound by `IntoSignalF32`.
    let _ = text("hello").opacity(fade.get());
    // Fires: `Signal::get` through a `Computed`.
    let _ = text("hello").opacity(fade.computed().get());
    // Fires: `.get()` inside `format!` feeding `IntoText`.
    let _ = text(format!("{}", count.get()));
    // Fires: `.get()` inside a method call feeding `IntoText`.
    let _ = text(count.get().to_string());
    // Fires: `.get()` snapshot passed to a parameter bound by `IntoLabel`.
    let _ = button(name.get());
    // Fires: `.get()` through an `as` cast.
    let _ = text("hello").opacity(fade.get() as f64);
    // Fires: `.get()` through arithmetic.
    let _ = text("hello").opacity(fade.get() * 2.0);
    // Fires: `.get()` through `.into()`.
    let _ = text(Into::<String>::into(name.get()));
    // Fires: `.get()` inside a struct-literal field feeding `IntoText`.
    let _ = text(Wrapped { value: name.get() });
    // Fires: `.get()` snapshot passed to a parameter bound by `IntoSignal`.
    let _ = takes_signal(fade.get());
    // Fires: `.get()` snapshot passed to a parameter bound by `IntoComputed`.
    let _ = takes_computed(count.get());
    // Fires: `.get()` snapshot passed to a parameter bound by `View`.
    let title = name.computed().map(text);
    let _ = AnyView::new(title.get());
    // Fires: `.get()` snapshot passed to a parameter bound by `ViewBuilder`.
    let builder = name.computed().map(|s| move || text(s.clone()));
    let _ = AnyViewBuilder::new(builder.get());

    // Silent: `.get()` inside a `.map` closure stays reactive.
    let fade_for_map = fade.clone();
    let _ = text("hello").opacity(fade.map(move |_| fade_for_map.get()));
    // Silent: `.get()` inside a plain function.
    let _ = count_now(&count.computed());
    // Silent: `text!` subscribes to `count` itself; its expansion's reads are
    // not the user's.
    let _ = AnyView::new(text!("{count}"));
    // Silent: `.get()` inside a button handler closure is a one-shot read.
    let _ = button("increment").action(move || count.set(count.get() + 1));
    // Silent: no snapshot at all.
    let _ = text("hello").opacity(0.5);
}
