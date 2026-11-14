//! `task_handle_dropped` fixture: a `task::spawn`/`spawn_local` handle dropped
//! at once fires — a bare `spawn(..);` statement, `let _ = spawn(..);`, or a
//! `let t = spawn(..);` never read in the rest of its block. A handle that is
//! awaited, detached, moved, stored, or returned stays silent.

use waterui::task::{spawn, spawn_local};

async fn run() {
    // Silent — the handle is awaited.
    let t = spawn_local(async {});
    t.await;
}

fn keep() -> impl Future<Output = ()> {
    // Silent — the handle is returned to the caller.
    spawn_local(async {})
}

fn main() {
    // Fires — a bare spawn statement drops the handle at the semicolon.
    spawn_local(async {});
    // Fires — `let _ =` drops the handle at the semicolon.
    #[allow(clippy::let_underscore_future)]
    let _ = spawn_local(async {});
    // Fires — `_t` is a named binding that is never read.
    let _t = spawn(async { 1 });
    // Fires — the binding is never read in the rest of the block.
    let _handle = spawn_local(async {});
    let _ = 1;
    // Silent — the handle is detached before scope end.
    let t = spawn_local(async {});
    t.detach();
    // Silent — detached in the same expression.
    spawn_local(async {}).detach();
    // Silent — `drop(t)` reads the handle (a deliberate cancellation).
    let t = spawn(async { 1 });
    drop(t);
    // Silent — the handle is moved into a tuple, still owned.
    let t = spawn_local(async {});
    let _keep = (t,);
    let _r = run();
    let _k = keep();
}
