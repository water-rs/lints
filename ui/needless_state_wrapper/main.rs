//! `needless_state_wrapper` fixture: a `State<T>` closure parameter warns when
//! `T`'s `Extractor` impl reads the `.state(&value)` channel — framework types
//! (`SnackbarManager`, `Navigator<Route>`) or a local `#[state]`/`#[state]`-
//! able `Clone` struct or enum. `Binding<_>`, `Vec<_>`, `Option<_>`, tuples,
//! `Environment`, generics, and type parameters of the enclosing fn stay
//! silent.

use std::marker::PhantomData;

use waterui::Extractor;
use waterui::navigation::Navigator;
use waterui::prelude::*;

/// The route type `Navigator<Route>` extracts over.
#[derive(Clone)]
struct Route;

/// A local state type — the fix marks it `#[state]`.
#[derive(Clone)]
struct Editor;

impl Editor {
    fn save(&self) {}
}

/// A local enum — `#[state]` applies to enums the same way.
#[derive(Clone)]
enum Mode {
    View,
    Edit,
}

/// Already marked `#[state]` — the fix rewrites only the parameter.
#[state]
#[derive(Clone)]
struct Store;

/// Silent — `W<T>` is local but generic: `#[state]` would emit an
/// unconditional `impl` where `W<T>: Clone + 'static` cannot hold.
#[derive(Clone)]
struct W<T> {
    marker: PhantomData<T>,
}

fn main() {
    // Fires — `SnackbarManager` implements `Extractor` via `#[state]`.
    let _ = button("save").action(|State(manager): State<SnackbarManager>| {
        manager.show(Snackbar::new("saved"));
    });

    // Fires — `Navigator<Route>`'s hand impl delegates to `State<Self>`.
    let _ = button("back").action(|State(nav): State<Navigator<Route>>| drop(nav));

    // Fires — `Editor` is a local `Clone` struct; the fix adds `#[state]`.
    let _ = button("save").action(|State(editor): State<Editor>| editor.save());

    // Fires — `use_env` takes the same extractor parameters.
    let _ = env::use_env(|State(editor): State<Editor>| {
        editor.save();
        text("done")
    });

    // Fires — a `s: State<T>` parameter rewrites every `s.0` use to `s`.
    let _ = button("save").action(|s: State<Editor>| s.0.save());

    // Fires — `Mode` is a local `Clone` enum; the fix adds `#[state]`.
    let _ = button("mode").action(|State(mode): State<Mode>| {
        let _ = mode;
    });
    let _ = (Mode::View, Mode::Edit);

    // Fires — `State(mut m)` rewrites to `mut m`, keeping the binding mode.
    let _ = button("mut").action(|State(mut m): State<SnackbarManager>| {
        let _ = &mut m;
    });

    // Fires — `Store` already carries `#[state]`; only the parameter is
    // rewritten, no second attribute.
    let _ = button("store").action(|State(s): State<Store>| {
        let _ = s;
    });

    // Silent — `Binding<String>` is foreign; the wrapper is its only door.
    let _ = button("set").action(|State(b): State<Binding<String>>| drop(b));

    // Silent — `Vec<u8>` is foreign and not an `Extractor`.
    let _ = button("vec").action(|State(v): State<Vec<u8>>| drop(v));

    // Silent — `Environment`'s impl returns the ambient environment, not the
    // injected `State` value.
    let _ = button("env").action(|State(e): State<Environment>| {
        let _ = e;
    });

    // Silent — `Option<Editor>` extracts an `Editor`, not the stored `Option`.
    let _ = button("opt").action(|State(o): State<Option<Editor>>| {
        let _ = o;
    });

    // Silent — `(Editor, Route)` extracts two values, not the stored tuple.
    let _ = button("pair").action(|State(p): State<(Editor, Route)>| {
        let _ = p;
    });

    // Silent — `W<i32>` is local but generic; `#[state]` would not compile.
    let _ = button("gen").action(|State(w): State<W<i32>>| {
        let _ = w;
    });

    generic::<Route>();
    bounded::<Store>();
    inner::run();
}

/// Silent — `T` is a type parameter of the enclosing function.
fn generic<T: Clone + 'static>() {
    let _ = button("t").action(|State(t): State<T>| drop(t));
}

/// Silent — `T: Extractor` does not prove the impl reads the `State` channel.
fn bounded<T: Extractor + Clone + 'static>() {
    let _ = button("t").action(|State(t): State<T>| drop(t));
}

/// A scope without the prelude glob — `state` is not nameable here, so the
/// fix inserts `use waterui::state;` alongside the `#[state]` attribute.
mod inner {
    use waterui::State;
    use waterui::component::button;

    /// Local to `inner` — the fix adds `#[state]` and the macro import.
    #[derive(Clone)]
    struct Local;

    pub fn run() {
        let _ = button("in").action(|State(l): State<Local>| {
            let _ = l;
        });
    }
}
