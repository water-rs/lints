//! `spawn_local_in_app` fixture: a `spawn_local` reached while `fn app` is
//! constructing the app — directly in its body, or in a function that body
//! calls directly — warns; one inside a `.task` future or a handler closure,
//! or in a function the entry never calls, stays silent.

#![allow(dead_code)]

use waterui::app::App;
use waterui::prelude::*;
use waterui::task::spawn_local;

fn seed_demo() {
    // Fires — `app` calls `seed_demo` directly, so this runs during
    // construction.
    spawn_local(async {}).detach();
}

fn uncalled() {
    // Silent — `app` never calls this.
    spawn_local(async {}).detach();
}

fn content() -> impl View {
    // Silent — the `.task` future runs after the runner installs the local
    // executor.
    let _ = text("a").task(async {
        spawn_local(async {}).detach();
    });
    // Silent — a handler body runs at event time, not during construction.
    button("b").action(|| {
        spawn_local(async {}).detach();
    })
}

pub fn app(env: Environment) -> App {
    // Fires — this `spawn_local` runs before the runner installs the local
    // executor.
    spawn_local(async {}).detach();
    // Silent — inside a `.task` future lexically contained in `app`.
    let _ = text("a").task(async {
        spawn_local(async {}).detach();
    });
    // Silent — inside a handler closure lexically contained in `app`.
    let _ = button("b").action(|| {
        spawn_local(async {}).detach();
    });
    seed_demo();
    App::new(content, env)
}

fn main() {}
