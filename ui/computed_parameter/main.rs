//! `computed_parameter` fixture: public signatures taking `Computed<T>` /
//! `&Computed<T>`, and private ones whose callers already erase signals.

#![allow(unknown_lints)]

use waterui::prelude::*;
use waterui::signal::IntoComputed;
use waterui::signal::Signal;

// Fires: `pub` — every caller must erase a signal before calling.
pub fn gauge(value: Computed<f32>) -> impl View {
    let _ = value.get();
    text("gauge")
}

// Fires: `pub(crate)` — any visibility but private counts.
pub(crate) fn label(text: &Computed<Str>) -> Str {
    text.get()
}

// Fires: `pub(crate)` — a restricted but written qualifier.
pub(crate) fn scoped(v: Computed<i32>) -> i32 {
    v.get()
}

// Silent: `pub(self)` is private no matter where it is written, and its
// only call site passes a place.
#[allow(clippy::needless_pub_self)]
pub(self) fn self_vis(v: Computed<i32>) -> i32 {
    v.get()
}

// Silent: `const fn` — `.into_computed()` can never run in a const body.
pub const fn konst(v: &Computed<i32>) -> i32 {
    let _ = v;
    7
}

// Fires: a `type` alias normalizes to `Computed<f32>`.
type Celsius = Computed<f32>;

pub fn aliased(v: Celsius) -> f32 {
    v.get()
}

pub struct Card;

impl Card {
    // Fires: `pub` inherent method of a public type.
    pub fn new(title: Computed<Str>) -> Card {
        let _ = title.get();
        Card
    }
}

struct Priv;

impl Priv {
    // Silent: `pub` on a private type names nothing outside the crate,
    // and a single erasing call site is under the private threshold.
    pub fn hidden(v: Computed<i32>) -> i32 {
        v.get()
    }
}

pub trait Widget {
    // Fires: `pub` trait method without a body — signature edit only.
    fn value(&self, v: Computed<i32>);

    // Fires: a provided body — the `let` splices in like any other body.
    fn level(&self, v: Computed<f32>) -> f32 {
        v.get()
    }
}

impl Widget for Card {
    // Silent: `impl Widget for Card` — the trait fixes the signature.
    fn value(&self, v: Computed<i32>) {
        let _ = v.get();
    }
}

impl View for Card {
    // Silent: `impl View for Card` — a trait impl's signature is fixed.
    fn body(self, _env: &Environment) -> impl View {
        text("card")
    }
}

// Fires: private, but two call sites erase a signal into `Computed` for it.
fn scaled(v: Computed<f32>) -> f32 {
    v.get() * 2.0
}

// Fires: private — two erasing call sites, one inside a closure; calls
// in any body count.
fn counted(v: Computed<i32>) -> i32 {
    v.get()
}

// Silent: private — a single erasing call site is under the threshold.
fn private(v: Computed<i32>) -> i32 {
    v.get()
}

// Silent: private — the only call site passes a place.
fn outer(current: &Computed<i32>) -> i32 {
    // Silent: a `fn` item inside a body is not public API.
    fn inner(v: Computed<i32>) -> i32 {
        v.get()
    }
    inner(current.clone())
}

// Silent: already `impl IntoComputed`.
pub fn ok(v: impl IntoComputed<i32>) -> i32 {
    v.into_computed().get()
}

// Silent: `impl Signal` already accepts any signal.
pub fn any_signal(v: impl Signal<Output = i32>) -> i32 {
    v.get()
}

// Silent: `Option<Computed<T>>` is not a bare `Computed`.
pub fn maybe(v: Option<Computed<i32>>) -> i32 {
    v.map(|v| v.get()).unwrap_or(0)
}

// Silent: `Vec<Computed<T>>` is not a bare `Computed`.
pub fn listed(v: Vec<Computed<i32>>) -> i32 {
    v.iter().map(|v| v.get()).sum()
}

// Silent: `Binding` is bidirectional — it stays as it is.
pub fn bound(b: Binding<i32>) -> i32 {
    b.get()
}

// Silent: `#[expect(computed_parameter)]` is fulfilled at the item —
// nothing is emitted.
#[expect(computed_parameter, reason = "the C-facing wrapper keeps `Computed`")]
pub fn expected(v: Computed<i32>) -> i32 {
    v.get()
}

// Silent: `v @ _` is not a plain binding — there is no text to rebind.
#[allow(clippy::redundant_pattern)]
pub fn rebound(v @ _: Computed<i32>) -> i32 {
    v.get()
}

// Silent: `extern "C"` — an `impl Trait` parameter has no C ABI.
#[expect(
    improper_ctypes_definitions,
    reason = "the point is the extern ABI, not FFI safety"
)]
pub extern "C" fn ffi(v: Computed<i32>) -> i32 {
    v.get()
}

// Silent: `#[unsafe(no_mangle)]` exports the signature as-is.
#[unsafe(no_mangle)]
pub fn exported(v: Computed<i32>) -> i32 {
    v.get()
}

// Fires: `mut` parameter — the inserted binding is `let mut`.
pub fn knob(mut level: Computed<f32>) -> f32 {
    let _ = level.get();
    level = Computed::constant(0.0);
    level.get()
}

// Fires: the body holds only a comment — the `let` is spliced inside the
// braces so the comment survives.
pub fn noop(_v: Computed<i32>) {
    /* keep me */
}

trait Meter {
    // Silent: the trait is private, and its only call site passes a place.
    fn read(&self, v: Computed<i32>);
}

impl Meter for Card {
    fn read(&self, v: Computed<i32>) {
        let _ = v.get();
    }
}

macro_rules! via_ty {
    ($t:ty) => {
        // Fires at the invocation — `$t` keeps call-site context, but the
        // generated body is macro-owned, so the suggestion edits the
        // signature only and notes that.
        pub fn generated(v: $t) -> i32 {
            let _ = v.clone();
            v.get()
        }
    };
}

via_ty!(Computed<i32>);

mod inner {
    use waterui::prelude::*;

    // Fires: `IntoComputed` is not in scope here — the fix inserts
    // `use waterui::signal::IntoComputed;`.
    pub fn helper(v: Computed<i32>) -> i32 {
        v.get()
    }

    // Fires: `pub(super)` — a written qualifier with restricted reach.
    pub(super) fn to_parent(v: Computed<i32>) -> i32 {
        v.get()
    }

    mod deep {
        use waterui::prelude::*;

        // Fires: `pub(in crate::inner)` — a path qualifier two levels up.
        pub(in crate::inner) fn scoped(v: Computed<i32>) -> i32 {
            v.get()
        }
    }

    /// Reaches `deep::scoped`, which is visible only inside this module.
    pub fn call_scoped() -> i32 {
        deep::scoped(Computed::constant(1))
    }
}

mod conflict {
    use waterui::prelude::*;

    /// A local trait taking the `IntoComputed` name.
    pub trait IntoComputed {
        fn mark(&self) -> i32;
    }

    impl IntoComputed for i32 {
        fn mark(&self) -> i32 {
            *self
        }
    }

    // Fires: `IntoComputed` resolves to the local trait — the signature is
    // rewritten `impl waterui::signal::IntoComputed<i32>`; the `impl` bound
    // puts `.into_computed()` in scope for the parameter, so no import is
    // needed.
    pub fn helper(v: Computed<i32>) -> i32 {
        v.get()
    }

    /// Keeps the local `IntoComputed` trait in use.
    pub fn marked() -> i32 {
        7.mark()
    }
}

fn main() {
    let current: Computed<i32> = Computed::constant(7);
    let total: Computed<f32> = Computed::constant(2.5);
    let name: Computed<Str> = Computed::constant(Str::from_static("n"));
    let count = Binding::f32(1.0);

    let _ = gauge(total.clone());
    let _ = label(&name);
    let _ = scoped(current.clone());
    let _ = self_vis(current.clone());
    let _ = konst(&current);
    let _ = aliased(total.clone());
    let _ = knob(total.clone());
    let _ = Card::new(name.clone());
    Card.value(Computed::constant(1));
    let _ = Card.level(total.clone());
    Card.read(current.clone());
    let _ = scaled(total.clone());
    let _ = scaled(Computed::constant(3.0_f32));
    let _ = scaled(count.map(|v| v + 1.0).computed());
    let _ = scaled(total);
    let _ = counted(Computed::constant(1));
    let f = || counted(Computed::constant(2));
    let _ = f();
    let _ = private(current.clone());
    let _ = private(Computed::constant(1));
    let _ = outer(&current);
    let _ = ok(4);
    let _ = any_signal(current.clone());
    let _ = maybe(Some(current.clone()));
    let _ = listed(vec![current.clone()]);
    let _ = bound(Binding::i32(5));
    let _ = expected(current.clone());
    let _ = rebound(current.clone());
    let _ = ffi(current.clone());
    let _ = exported(current.clone());
    noop(current.clone());
    let _ = Priv::hidden(Computed::constant(9));
    let _ = inner::helper(current.clone());
    let _ = inner::to_parent(current.clone());
    let _ = inner::call_scoped();
    let _ = conflict::helper(current.clone());
    let _ = conflict::marked();
    let _ = generated(current.clone());
}
