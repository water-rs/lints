//! `non_reactive_ui_state` fixture: a handler closure mutating a captured
//! `RefCell`/`Cell`/`Mutex`/`RwLock`/atomic or touching a `static mut` /
//! `thread_local!` warns, as does such a field on a `View` type; a `State`
//! extractor parameter, a capture that is only read, a mutating closure that
//! never reaches a handler position, and a `Binding` field stay silent.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use waterui::accessibility::AccessibilityRole;
use waterui::prelude::*;

static mut COUNTER: u32 = 0;

std::thread_local! {
    static TLS: Cell<u32> = const { Cell::new(0) };
}

// Fires — `value` is an `Rc<RefCell<u32>>` field on a `View` type.
struct Counter {
    value: Rc<RefCell<u32>>,
}

impl View for Counter {
    fn body(self, _env: &Environment) -> impl View {
        let _ = self.value;
        text("a")
    }
}

// Silent — `title` is a `Binding<Str>`, which is reactive state.
struct Card {
    title: Binding<Str>,
}

impl View for Card {
    fn body(self, _env: &Environment) -> impl View {
        let _ = self.title;
        text("a")
    }
}

fn main() {
    // Fires — `borrow_mut` mutates the `RefCell` inside the captured `Rc`.
    let n = Rc::new(RefCell::new(0));
    let _ = button("a").action(move || *n.borrow_mut() += 1);

    // Fires — `Cell::set` mutates the `Cell` inside the captured `Rc`.
    let c = Rc::new(Cell::new(0));
    let _ = button("a").action(move || c.set(1));

    // Fires — `Mutex::lock` hands out the guard the captured `Mutex` mutates through.
    let m = Arc::new(Mutex::new(0));
    let _ = button("a").action(move || *m.lock().unwrap() += 1);

    // Fires — `RwLock::write` on the captured `RwLock`.
    let w = Arc::new(RwLock::new(0));
    let _ = button("a").action(move || *w.write().unwrap() += 1);

    // Fires — `AtomicU32::store` on the captured `AtomicU32`.
    let a = Arc::new(AtomicU32::new(0));
    let _ = text("a")
        .a11y_role(AccessibilityRole::Button)
        .a11y_label("x")
        .on_tap(move || a.store(1, Ordering::Relaxed));

    // Fires — a `static mut` written from a handler.
    let _ = button("a").action(|| unsafe { COUNTER += 1 });

    // Fires — a `thread_local!` mutated through `LocalKey::with`.
    let _ = button("a").action(|| TLS.with(|v| v.set(1)));

    // Construct the `View` types so `dead_code` stays silent.
    let _ = Counter {
        value: Rc::new(RefCell::new(0)),
    };
    let _ = Card {
        title: binding(Str::from("a")),
    };

    // Silent — `State` extracts the injected `Binding`; the handler captures
    // nothing.
    let count: Binding<i32> = binding(0);
    let _ = button("a")
        .action(|State(count): State<Binding<i32>>| count.set(1))
        .state(&count);

    // Silent — `borrow` only reads the `RefCell`; nothing is mutated.
    let n = Rc::new(RefCell::new(0));
    let _ = button("a").action(move || {
        let _ = *n.borrow();
    });

    // Silent — the mutating closure never reaches a handler position.
    let n = Rc::new(RefCell::new(0));
    let f = move || *n.borrow_mut() += 1;
    f();
}
