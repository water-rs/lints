// compile-flags: --test
//! `discarded_wait_result` fixture: a `bool` `wait_for_*` result that is
//! dropped — `let _ =`, a bare statement, or a binding never read — is
//! flagged on both `Query` and `SemanticApp`, while a result that is
//! asserted, read later, drives an `if`, or comes from a non-wait `bool`
//! method stays silent.
//!
//! The `--test` flag is required: rustc strips `#[test]` items — including
//! the `#[test] fn` the harness macro emits — in non-test builds, so the
//! wrappers exist only when the crate is compiled as a test.

use waterui::prelude::*;

// Imported under `cfg(test)` for the same reason the harness compiles this
// file with `--test`: the only uses are the `#[waterui::test]` bodies, which
// rustc strips in non-test builds.
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use waterui_testing::{Selector, SemanticApp};

fn main() {
    // `#[test]` items are stripped in non-test builds; name `home` here so it
    // is live either way.
    let _ = home;
}

fn home() -> impl View {
    text("a")
}

// Fires — `let _ =` silences `#[must_use]` and drops the `false`-on-timeout
// result.
#[waterui::test(home)]
fn discards_with_wildcard(app: &mut SemanticApp) {
    let _ = app
        .query()
        .label("a")
        .wait_for_existence(Duration::from_secs(1));
}

// Fires — a bare statement drops the result. Stock `unused_must_use` fires
// here too (`Query::wait_for_nonexistence` is `#[must_use]`); both
// diagnostics are expected in the blessed stderr.
#[waterui::test(home)]
fn discards_bare_statement(app: &mut SemanticApp) {
    app.query()
        .label("a")
        .wait_for_nonexistence(Duration::from_secs(1));
}

// Fires — `let _ =` on `Query::wait_for_value_eq`.
#[waterui::test(home)]
fn discards_value_eq(app: &mut SemanticApp) {
    let _ = app
        .query()
        .label("a")
        .wait_for_value_eq("1", Duration::from_secs(1));
}

// Fires — the `SemanticApp` wait methods carry no `#[must_use]`, so only this
// lint sees the discard.
#[waterui::test(home)]
fn discards_app_wait(app: &mut SemanticApp) {
    let _ = app.wait_for_existence(&Selector::default().label("a"), Duration::from_secs(1));
}

// Fires — `ok` is never read after this statement; stock `unused_variables`
// fires too, and both diagnostics are expected in the blessed stderr.
#[waterui::test(home)]
fn discards_unread_binding(app: &mut SemanticApp) {
    let ok = app
        .query()
        .label("a")
        .wait_for_existence(Duration::from_secs(1));
    let _ = app;
}

// Silent — the result is asserted.
#[waterui::test(home)]
fn asserts_wait(app: &mut SemanticApp) {
    assert!(
        app.query()
            .label("a")
            .wait_for_existence(Duration::from_secs(1))
    );
}

// Silent — the binding is read.
#[waterui::test(home)]
fn reads_binding(app: &mut SemanticApp) {
    let ok = app
        .query()
        .label("a")
        .wait_for_existence(Duration::from_secs(1));
    assert!(ok);
}

// Silent — the result drives the `if` condition.
#[waterui::test(home)]
fn waits_in_condition(app: &mut SemanticApp) {
    if app
        .query()
        .label("a")
        .wait_for_existence(Duration::from_secs(1))
    {
        app.query().label("a").assert_exists();
    }
}

// Silent — `Query::exists` returns `bool` but is not a wait.
#[waterui::test(home)]
fn discards_other_bool(app: &mut SemanticApp) {
    let _ = app.query().label("a").exists();
}
