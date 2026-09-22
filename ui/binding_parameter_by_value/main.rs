//! `binding_parameter_by_value` fixture: signatures taking `Binding<T>`
//! by value where `&Binding<T>` would do, and the call sites / body uses
//! the fix rewrites with them.

#![allow(unknown_lints)]

use waterui::prelude::*;

// Fires: `pub` — `text` takes `name` by value, so the fix writes
// `text(name.clone())` alongside the signature's `&`.
pub fn counter(name: Binding<Str>) -> impl View {
    text(name)
}

// Fires: `pub(crate)` — `set` borrows the handle, so only the signature
// changes.
pub(crate) fn reset(flag: Binding<bool>) {
    flag.set(false);
}

// Fires: `pub` — the tail returns the handle, so the fix writes
// `count.clone()`.
pub fn passthrough(count: Binding<i32>) -> Binding<i32> {
    count
}

// Fires: `pub` — the `move` closure hands the handle to its caller, so
// the fix writes `flag.clone()` inside it.
pub fn keep(flag: Binding<bool>) -> impl FnOnce() -> Binding<bool> {
    move || flag
}

// Fires: a `type` alias normalizes to `Binding<i32>`.
type Count = Binding<i32>;

pub fn aliased(count: Count) -> i32 {
    count.get()
}

pub struct Row {
    selected: Binding<bool>,
}

impl Row {
    // Fires: `pub` inherent method of a `pub` type — the shorthand field
    // initializer stores the handle, rewritten `selected: selected.clone()`.
    pub fn new(selected: Binding<bool>) -> Row {
        Row { selected }
    }
}

// Fires: `pub` — an edition-2024 `-> impl View` return gets `+ use<>` in
// the suggestion, so the new `&`'s anonymous lifetime is not captured.
pub fn panel(name: Binding<Str>) -> impl View {
    text(name)
}

// Fires: `pub` — named generics are re-captured by name: `+ use<'a, T>`.
pub fn tag<'a, T: View>(name: Binding<Str>, mark: &'a str, note: &'a str, inner: T) -> impl View {
    let _ = name.get();
    let _ = (mark, note);
    inner
}

pub trait Watcher {
    // Fires: `pub` trait member — the note names every implementor.
    fn bind(&self, on: Binding<bool>);

    // Fires: `pub` trait member returning `impl View` — `use<Self>` keeps
    // the trait's self capture while dropping the new `&`'s lifetime.
    fn build(&self, on: Binding<bool>) -> impl View;
}

impl Watcher for Row {
    // Silent: `impl Watcher for Row` — the trait fixes the signature.
    fn bind(&self, on: Binding<bool>) {
        on.set(true);
    }

    fn build(&self, on: Binding<bool>) -> impl View {
        let _ = on.get();
        text("row")
    }
}

trait Private {
    // Silent: private trait, and its only call site does not clone.
    fn bind(&self, on: Binding<bool>);
}

impl Private for Row {
    fn bind(&self, on: Binding<bool>) {
        let _ = on.get();
    }
}

// Fires: private, but `main` passes `render(count.clone())` — the caller
// clones only because the signature takes the handle. The fix rewrites
// every argument at the parameter's position: `render(&count)`,
// `render(&other)`, `render(&Binding::i32(8))`.
fn render(item: Binding<i32>) -> i32 {
    item.get()
}

// Silent: private, and its only call site moves a fresh handle in rather
// than cloning.
fn private(b: Binding<i32>) -> i32 {
    b.get()
}

// Silent: `pub(self)` is private no matter where it is written.
#[allow(clippy::needless_pub_self)]
pub(self) fn self_vis(b: Binding<i32>) -> i32 {
    b.get()
}

// Silent: a `fn` item inside a body is not public API, and its only call
// site moves a fresh handle in rather than cloning.
fn outer(current: &Binding<i32>) -> i32 {
    fn inner(b: Binding<i32>) -> i32 {
        b.get()
    }
    let _ = current.get();
    inner(Binding::i32(0))
}

// Silent: already `&Binding`.
pub fn ok(b: &Binding<i32>) -> i32 {
    b.get()
}

// Silent: `State<Binding<T>>` is not a bare `Binding` — and `State(b)` is
// not a plain binding pattern either.
pub fn handler(State(b): State<Binding<i32>>) -> i32 {
    b.get()
}

// Silent: `Option<Binding<T>>` is not a bare `Binding`.
pub fn maybe(b: Option<Binding<i32>>) -> i32 {
    b.map(|b| b.get()).unwrap_or(0)
}

// Silent: `Vec<Binding<T>>` is not a bare `Binding`.
pub fn many(bs: Vec<Binding<i32>>) -> i32 {
    bs.iter().map(|b| b.get()).sum()
}

// Silent: `#[expect(binding_parameter_by_value)]` is fulfilled at the
// item — nothing is emitted.
#[expect(
    binding_parameter_by_value,
    reason = "the C-facing wrapper keeps `Binding`"
)]
pub fn expected(b: Binding<i32>) -> i32 {
    b.get()
}

// Silent: `b @ _` is not a plain binding — there is no text the `&` could
// feed.
#[allow(clippy::redundant_pattern)]
pub fn rebound(b @ _: Binding<i32>) -> i32 {
    b.get()
}

// Silent: `extern "C"` keeps its ABI.
#[expect(
    improper_ctypes_definitions,
    reason = "the point is the extern ABI, not FFI safety"
)]
pub extern "C" fn ffi(b: Binding<i32>) -> i32 {
    b.get()
}

// Silent: `#[unsafe(no_mangle)]` exports the signature as-is.
#[unsafe(no_mangle)]
pub fn exported(b: Binding<i32>) -> i32 {
    b.get()
}

// Silent: `const fn` — the fix's `.clone()` can never run in a const
// body, so the signature keeps the handle even with a `f(b.clone())`
// call site.
pub const fn konst(b: Binding<i32>) -> Binding<i32> {
    b
}

fn main() {
    let count = Binding::i32(7);
    let on = Binding::bool(true);
    let name = Binding::container(Str::from_static("n"));
    let other = Binding::i32(3);

    let _ = counter(name.clone());
    reset(on.clone());
    let _ = passthrough(count.clone());
    let _ = keep(on.clone());
    let _ = aliased(count.clone());
    let row = Row::new(on.clone());
    let _ = row.selected.get();
    Watcher::bind(&row, on.clone());
    Private::bind(&row, Binding::bool(false));
    let _ = render(count.clone());
    let _ = render(other);
    let _ = render(Binding::i32(8));
    let _ = private(Binding::i32(1));
    let _ = self_vis(Binding::i32(5));
    let _ = outer(&count);
    let _ = ok(&count);
    let _ = handler(State(Binding::i32(2)));
    let _ = maybe(Some(count.clone()));
    let _ = many(vec![count.clone()]);
    let _ = expected(count.clone());
    let _ = rebound(count.clone());
    let _ = ffi(count.clone());
    let _ = exported(count.clone());
    let _ = konst(count.clone());
    // Silent: a closure parameter is not a signature this lint rewrites.
    let read = |b: Binding<i32>| b.get();
    let _ = read(count.clone());
    let _ = count.get();
}
