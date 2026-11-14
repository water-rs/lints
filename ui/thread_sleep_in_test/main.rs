// compile-flags: --test
//! `thread_sleep_in_test` fixture: `std::thread::sleep` in a
//! `#[waterui::test]`/`#[waterui::bench]` body is flagged — directly, in a
//! loop, in a closure literal, or in the `async` body `block_on` drives —
//! while a sleep in a plain `#[test]`, in a helper `fn` the test calls, or a
//! sanctioned `pump_for`/`wait_for_existence` wait stays silent.
//!
//! The `--test` flag is required: rustc strips `#[test]` items — including
//! the `#[test] fn` the harness macros emit — in non-test builds, so the
//! wrappers exist only when the crate is compiled as a test.

use std::time::Duration;

use waterui::prelude::*;

// Imported under `cfg(test)` for the same reason the harness compiles this
// file with `--test`: the only uses are the `#[waterui::test]`/`bench`
// signatures, which rustc strips in non-test builds.
#[cfg(test)]
use waterui_testing::{OffscreenApp, PerfApp, SemanticApp, UiBuilder};

fn main() {
    // `#[test]` items are stripped in non-test builds; name the helpers here
    // so `home`/`helper` are live either way.
    let _ = home;
    let _ = helper;
}

fn home() -> impl View {
    text("a")
}

fn helper() {
    std::thread::sleep(Duration::from_millis(1));
}

// Fires — `std::thread::sleep` in a `#[waterui::test]` body.
#[waterui::test(home)]
fn sleeps(app: &mut SemanticApp) {
    std::thread::sleep(Duration::from_millis(16));
    let _ = app;
}

// Fires — the manual-mount form takes the `UiBuilder` by value.
#[waterui::test]
fn manual_sleeps(ui: UiBuilder) {
    std::thread::sleep(Duration::from_millis(1));
    let _ = ui;
}

// Fires — a sleep inside a `for` loop in a test body.
#[waterui::test(home)]
fn sleeps_in_loop(app: &mut SemanticApp) {
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(4));
    }
    let _ = app;
}

// Fires — a sleep inside a closure literal in a test body.
#[waterui::test(home)]
fn sleeps_in_closure(app: &mut SemanticApp) {
    let wait = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
    wait(1);
    let _ = app;
}

// Fires — the `async` body `block_on` drives is the test's body; the sleep is
// reported here and `thread_sleep_in_ui` stays silent (a test context is not
// a UI context), so the call gets one diagnostic.
#[waterui::test(home)]
async fn async_sleeps(app: &mut SemanticApp) {
    std::thread::sleep(Duration::from_millis(16));
    let _ = app;
}

// Fires — a sleep in a `#[waterui::bench]` body.
#[waterui::bench(home)]
fn bench_sleeps(perf: &mut PerfApp) {
    std::thread::sleep(Duration::from_millis(16));
    let _ = perf;
}

// Silent — a plain `#[test]` is no harness wrapper.
#[test]
fn plain_test_sleeps() {
    std::thread::sleep(Duration::from_millis(1));
}

// Silent — the sleep sits in `helper`'s body; the lint is callsite-scoped.
#[waterui::test(home)]
fn calls_helper(app: &mut SemanticApp) {
    helper();
    let _ = app;
}

// Silent — `pump_for` is the sanctioned way to advance the animation clock
// (offscreen sessions own `pump_for`).
#[waterui::test(home, offscreen)]
fn pumps(app: &mut OffscreenApp) {
    app.pump_for(Duration::from_millis(16));
}

// Silent — waiting on a condition is the other sanctioned shape.
#[waterui::test(home)]
fn waits(app: &mut SemanticApp) {
    assert!(
        app.query()
            .label("a")
            .wait_for_existence(Duration::from_secs(1))
    );
}
