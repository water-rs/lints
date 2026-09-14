//! `manual_identifiable` fixture: a hand-written `impl Identifiable` whose
//! `id` returns a field, and `use_id`/`self_id` wrappers on local structs,
//! warn; enums, unions, computed bodies, a `type Id` that is not the field's
//! type, foreign receivers, and already-`Identifiable` types stay silent.

/// `use waterui::Identifiable` resolves a bare `Identifiable` to the derive
/// macro, so fixes in this module spell it `Identifiable`.
mod imported {
    use std::hash::Hash;

    use waterui::Identifiable;
    use waterui::id::IdentifiableExt;
    use waterui::text::text;
    use waterui::{Binding, binding};

    struct CopyId {
        id: u64,
    }

    // Fires — `id` returns a field by value.
    impl Identifiable for CopyId {
        type Id = u64;
        fn id(&self) -> Self::Id {
            self.id
        }
    }

    struct CloneId {
        id: String,
    }

    // Fires — `id` clones a field.
    impl Identifiable for CloneId {
        type Id = String;
        fn id(&self) -> Self::Id {
            self.id.clone()
        }
    }

    struct Slot(u64);

    // Fires — a tuple field; the fix marks it `#[id] 0`.
    impl Identifiable for Slot {
        type Id = u64;
        fn id(&self) -> Self::Id {
            self.0
        }
    }

    #[derive(Debug, Clone)]
    struct Derived {
        id: u64,
    }

    // Fires — the fix extends the existing `#[derive(..)]` list.
    impl Identifiable for Derived {
        type Id = u64;
        fn id(&self) -> Self::Id {
            self.id
        }
    }

    struct TreeNode<T, ID: Hash + Ord + Clone> {
        id: ID,
        data: T,
        children: Vec<TreeNode<T, ID>>,
    }

    // Fires — a generic struct; `type Id = ID` is the field's type.
    impl<T, ID: Hash + Ord + Clone> Identifiable for TreeNode<T, ID> {
        type Id = ID;
        fn id(&self) -> Self::Id {
            self.id.clone()
        }
    }

    struct Attributed {
        /// The node's key.
        #[doc(hidden)]
        key: u64,
        value: String,
    }

    // Fires — `#[id]` goes before the field's own attributes.
    impl Identifiable for Attributed {
        type Id = u64;
        fn id(&self) -> Self::Id {
            self.key
        }
    }

    struct Keyed {
        key: u64,
    }

    struct Single {
        token: String,
    }

    struct Pair {
        a: u64,
        b: u64,
    }

    #[derive(Identifiable)]
    struct Ready {
        #[id]
        key: u64,
    }

    enum Row {
        A(u64),
        B(u64),
    }

    // Silent — the enum impl computes ids from the variant.
    impl Identifiable for Row {
        type Id = u64;
        fn id(&self) -> Self::Id {
            match self {
                Row::A(i) => *i,
                Row::B(i) => *i + 1000,
            }
        }
    }

    struct Computed {
        a: u64,
        b: u64,
    }

    // Silent — `id` computes from two fields.
    impl Identifiable for Computed {
        type Id = u64;
        fn id(&self) -> Self::Id {
            self.a * 100 + self.b
        }
    }

    struct Column {
        title: String,
    }

    impl Column {
        fn semantic_id(&self) -> String {
            self.title.clone()
        }
    }

    // Silent — `id` delegates to a method, it doesn't read a field.
    impl Identifiable for Column {
        type Id = String;
        fn id(&self) -> Self::Id {
            self.semantic_id()
        }
    }

    struct Cast {
        id: u32,
    }

    // Silent — `type Id` is `u64`, not the field's `u32`.
    impl Identifiable for Cast {
        type Id = u64;
        fn id(&self) -> Self::Id {
            self.id as u64
        }
    }

    union Bits {
        a: u64,
        b: u64,
    }

    // Silent — unions can't take `#[id]`.
    impl Identifiable for Bits {
        type Id = u64;
        fn id(&self) -> Self::Id {
            unsafe { self.a }
        }
    }

    pub(super) fn run() {
        let copy = CopyId { id: 1 };
        let _ = (&copy.id, copy.id());

        let cloned = CloneId {
            id: "x".to_string(),
        };
        let _ = (&cloned.id, cloned.id());

        let slot = Slot(7);
        let _ = (&slot.0, slot.id());

        let derived = Derived { id: 2 };
        let _ = (&derived.id, derived.id());

        let node: TreeNode<&str, u64> = TreeNode {
            id: 3,
            data: "n",
            children: Vec::new(),
        };
        let _ = (&node.id, &node.data, &node.children, node.id());

        let attributed = Attributed {
            key: 4,
            value: "v".to_string(),
        };
        let _ = (&attributed.key, &attributed.value, attributed.id());

        let keyed = Keyed { key: 1 };
        let _ = &keyed.key;
        // Fires — `use_id` selects a field of a local struct.
        let _ = keyed.use_id(|v| v.key);

        let single = Single {
            token: "tok".to_string(),
        };
        let _ = &single.token;
        // Fires — `self_id` on a single-field local struct.
        let _ = single.self_id();

        // Silent — `use_id` on a foreign `String`.
        let _ = "foreign".to_string().use_id(|v| v.clone());
        // Silent — `use_id` on a foreign waterui type.
        let _ = text("x").use_id(|_| 0_u64);
        // Silent — `self_id` on a foreign `nami` binding.
        let signal: Binding<i32> = binding(3);
        let _ = signal.self_id();
        // Silent — the closure computes a new id.
        let _ = Pair { a: 1, b: 2 }.use_id(|v| v.a + v.b);
        // Silent — `self_id` on a multi-field struct.
        let _ = Pair { a: 3, b: 4 }.self_id();

        // Silent — both impls keep their bodies.
        let _ = (Row::A(1).id(), Row::B(2).id());
        let _ = Computed { a: 1, b: 2 }.id();
        let column = Column {
            title: "t".to_string(),
        };
        let _ = (column.id(), column.semantic_id());
        let _ = Cast { id: 1 }.id();
        let bits = Bits { a: 5 };
        let _ = unsafe { bits.b };

        let ready = Ready { key: 9 };
        let _ = &ready.key;
        // Silent — `Ready` is already `Identifiable`; the wrapper stays.
        let _ = ready.use_id(|v| v.key);
    }
}

/// Only `waterui::id::{Identifiable, IdentifiableExt}` are in scope — the
/// fix spells the derive `waterui::Identifiable`.
mod qualified {
    use waterui::id::{Identifiable, IdentifiableExt};

    struct Token {
        token: String,
    }

    // Fires — `Identifiable` here is `waterui::id::Identifiable`, which is
    // not the derive macro, so the fix uses the qualified spelling.
    impl Identifiable for Token {
        type Id = String;
        fn id(&self) -> Self::Id {
            self.token.clone()
        }
    }

    pub(super) fn run() {
        let token = Token {
            token: "t".to_string(),
        };
        let _ = (&token.token, token.id());

        // Silent — `Token` is `Identifiable` (the manual impl, or the derive
        // after the fix), so the `use_id` wrapper is a choice, not a
        // workaround.
        let _ = Token {
            token: "u".to_string(),
        }
        .use_id(|v| v.token.clone());
    }
}

fn main() {
    imported::run();
    qualified::run();
}
