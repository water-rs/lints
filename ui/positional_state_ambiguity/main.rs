//! `positional_state_ambiguity` fixture: two `State<T>` parameters of the
//! same `T` in one handler bind to `.state(..)` calls by position — warn.
//! Different `T`s, a single `State`, a `Use<T>` neighbour, and a handler
//! taking nothing stay silent.

use waterui::extract::Use;
use waterui::prelude::*;

#[derive(Clone)]
struct Config;

fn both(State(a): State<Binding<i32>>, State(b): State<Binding<i32>>) {
    a.set(1);
    b.set(2);
}

fn main() {
    let a: Binding<i32> = binding(0);
    let b: Binding<i32> = binding(1);
    let flag: Binding<bool> = binding(false);

    // Fires — two `State<Binding<i32>>` parameters share `T`.
    let _ = button("x")
        .action(
            |State(x): State<Binding<i32>>, State(y): State<Binding<i32>>| {
                x.set(1);
                y.set(2);
            },
        )
        .state(&a)
        .state(&b);

    // Fires — a named function in the same position.
    let _ = button("y").action(both).state(&a).state(&b);

    // Fires — of three `State` parameters, the first and third share `T`.
    let _ = button("z")
        .action(
            |State(x): State<Binding<i32>>,
             State(f): State<Binding<bool>>,
             State(y): State<Binding<i32>>| {
                x.set(1);
                f.set(true);
                y.set(2);
            },
        )
        .state(&a)
        .state(&flag)
        .state(&b);

    // Silent — different `T`s bind unambiguously.
    let _ = button("ok")
        .action(
            |State(x): State<Binding<i32>>, State(f): State<Binding<bool>>| {
                x.set(1);
                f.set(true);
            },
        )
        .state(&a)
        .state(&flag);

    // Silent — a single `State<Binding<i32>>` parameter.
    let _ = button("one")
        .action(|State(x): State<Binding<i32>>| x.set(1))
        .state(&a);

    // Silent — `Use<Config>` extracts an environment value by type, not position.
    let _ = button("cfg")
        .action(|State(x): State<Binding<i32>>, Use(config): Use<Config>| {
            x.set(1);
            let _ = config;
        })
        .state(&a)
        .with(Config);

    // Silent — a handler with no parameters.
    let _ = button("none").action(|| {});
}
