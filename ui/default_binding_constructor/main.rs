//! `default_binding_constructor` fixture: `binding(..)`/`Binding::container(..)`
//! seeded with `T::default()`, a std `new` that is `T`'s `Default`, `vec![]`,
//! or `None` warns and rewrites to `Binding::<T>::default()` — `T` spelled
//! from the argument's path, the call's turbofish, or a `: Binding<T>`
//! annotation the fix then drops; when nothing spells `T` the fix is
//! `Binding::default()`. Literal and non-default seeds stay silent. `binding`
//! takes `impl Into<T>`, so each `binding(..)` here pins `T` by annotation,
//! turbofish, or a later `.set(..)`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use waterui::prelude::*;

#[derive(Clone, Default)]
struct WindowState {
    fullscreen: bool,
}

#[derive(Clone)]
struct Item;

fn main() {
    let _it = Item;
    // Fires — `WindowState::default()` spells `T`; the later `.set(..)` pins
    // the binding to `WindowState`.
    let _arg_path = binding(WindowState::default());
    _arg_path.set(WindowState { fullscreen: true });
    let _fs = _arg_path.get().fullscreen;
    // Fires — the call's turbofish also spells `T` (the argument wins).
    let _turbofish = binding::<WindowState>(WindowState::default());
    // Fires — `<WindowState as Default>::default()` spells `T` through the
    // qualified self type.
    let _qualified = binding::<WindowState>(<WindowState as Default>::default());
    // Fires — `String::new()` spells `String`; the `Binding<String>`
    // annotation is then redundant and the fix drops it.
    let _name: Binding<String> = binding(String::new());
    // Fires — `Vec::<Item>::new()` spells `T` on `Binding::container`.
    let _items = Binding::container(Vec::<Item>::new());
    // Fires — `vec![]` lowers to `Vec::new()`; the annotation spells
    // `Vec<Item>` and is dropped.
    let _vec: Binding<Vec<Item>> = binding(vec![]);
    // Fires — `None::<u8>` rewrites to `Option<u8>`; the annotation is
    // dropped.
    let _none: Binding<Option<u8>> = binding(None::<u8>);
    // Fires — `HashMap::new()` cannot spell `HashMap<K, V>`; the annotation
    // does and is dropped.
    let _map: Binding<HashMap<String, u8>> = Binding::container(HashMap::new());
    // Fires — the remaining std `new` shapes spell `T` themselves.
    let _deque = Binding::container(VecDeque::<u8>::new());
    let _set = Binding::container(HashSet::<u8>::new());
    let _btree_map = Binding::container(BTreeMap::<u8, u8>::new());
    let _btree_set = Binding::container(BTreeSet::<u8>::new());
    // Fires — bare `Vec` cannot spell `Vec<T>` and `Default::default()`
    // spells the trait; the later `.set(..)` pins `Vec<u8>`, so the fix is
    // `Binding::default()`.
    let _vec_default = binding(Vec::default());
    _vec_default.set(vec![0_u8]);
    // Fires — `Default::default()` spells the trait, not `T`; the
    // `Binding::<String>` path on `container` spells it instead.
    let _container_default = Binding::<String>::container(Default::default());
    // Fires — `Binding::<_>`'s `_` spells nothing, but the seed's own path
    // supplies `String`.
    let _infer = Binding::<_>::container(String::new());
    // Fires — the call sits at the macro call site and is rewritten, but the
    // `let` lives in the `macro_rules!` body: its `Binding<String>`
    // annotation must not be erased.
    macro_rules! seeded {
        ($seed:expr) => {{
            let x: Binding<String> = $seed;
            x
        }};
    }
    let _mac = seeded!(binding(String::new()));
    // Fires — `String::new()`'s span is in the macro definition, so its `T`
    // text cannot be quoted at the call site; nothing else spells `T` and
    // the fix is the untyped `Binding::default()` (the annotated re-let pins
    // `String`).
    macro_rules! def_string {
        () => {
            String::new()
        };
    }
    let _mac_seed = Binding::container(def_string!());
    let _mac_pin: Binding<String> = _mac_seed;
    // Silent — `0` is a literal seed; `typed_binding_constructor`'s case.
    let _zero: Binding<i32> = binding(0);
    // Silent — `false` likewise.
    let _flag: Binding<bool> = binding(false);
    // Silent — `String::from` is not a default value.
    let _from: Binding<String> = binding(String::from("x"));
    // Silent — `Vec::with_capacity` is not `Default`.
    let _cap: Binding<Vec<u8>> = binding(Vec::<u8>::with_capacity(4));
    // Silent — the suggested form itself.
    let _done = Binding::<String>::default();
}
