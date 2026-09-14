//! `blocking_in_ui_context`/`thread_sleep_in_ui` fixture: blocking calls
//! inside a `Handler` closure, an `async` block or `async fn` body, or
//! `View::body` are flagged; the same calls at `main` top level, behind a
//! plain `fn` boundary, or as the executor's own `task::sleep` stay silent.

use std::time::Duration;
use waterui::prelude::*;
use waterui::task;

fn main() {
    // Fires (deny) — `std::thread::sleep` in a `Handler` closure.
    let _ = button("a").action(|| std::thread::sleep(Duration::from_millis(1)));

    // Fires — a `std::fs` call in a `Handler` closure.
    let _ = button("a").action(|| {
        let _ = std::fs::read_to_string("/etc/hosts");
    });

    // Fires — a `std::fs` call in the `async` block `action_async` spawns.
    let _ = button("a").action_async(|| async {
        let _ = std::fs::read("/etc/hosts");
    });

    // Fires — `Mutex::lock` in a `Handler` closure. `non_reactive_ui_state`
    // reports this line too (locking a captured `Mutex` is also its shape);
    // both diagnostics are correct.
    let m = std::sync::Mutex::new(0);
    let _ = button("a").action(move || {
        drop(m.lock().unwrap());
    });

    // Silent — `main`'s top level is no UI context.
    std::thread::sleep(Duration::from_millis(1));

    // Silent — `task::sleep` is the executor's non-blocking sleep.
    let _ = button("a").action_async(|| async {
        task::sleep(Duration::from_millis(1)).await;
    });

    // Silent — the `std::fs::read` sits in `helper`'s body; only direct calls
    // in the UI context are in scope.
    let _ = button("a").action(|| helper(7));

    // `load`'s body is flagged below; the call here just keeps it used.
    let _load = load();
    let _slow = Slow;
}

fn helper(_n: u32) {
    let _ = std::fs::read("/etc/hosts");
}

// Fires — a `std::fs` call in an `async fn` body.
async fn load() {
    let _ = std::fs::read("/x");
}

struct Slow;

// Fires (deny) — `std::thread::sleep` in `View::body`.
impl View for Slow {
    fn body(self, _env: &Environment) -> impl View {
        std::thread::sleep(Duration::from_millis(1));
        text("a")
    }
}
