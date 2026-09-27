//! `clone_refcell_field` fixture: a bare `RefCell`/`Cell` field on a
//! `#[derive(Clone)]` struct warns when a sibling field already shares
//! state through `Rc<RefCell<_>>`/`Rc<Cell<_>>`; structs that are not
//! `Clone`, `Clone` structs with no `Rc`-shared sibling, and structs whose
//! cells all live inside `Rc` stay silent.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ClosedTab {
    title: String,
    url: String,
}

// Fires — the dogfood shape: `AppState` is cloned between windows, the
// `Rc<RefCell<_>>` siblings share, but `undo` forks.
#[derive(Clone)]
struct AppState {
    tabs: Rc<RefCell<Vec<ClosedTab>>>,
    undo: RefCell<Vec<ClosedTab>>,
    focused: Option<usize>,
}

// Fires — `Cell` and `Rc<Cell<_>>` pair, tuple field counts too.
#[derive(Clone)]
struct Mixer {
    scratch: Cell<u32>,
    levels: Rc<Cell<u32>>,
    name: String,
}

// Fires — every bare cell field is reported.
#[derive(Clone)]
struct Document {
    pages: Rc<RefCell<Vec<u8>>>,
    cursor: RefCell<usize>,
    dirty: Cell<bool>,
}

// Silent — not `Clone`, nothing is forked.
struct SingleOwner {
    items: RefCell<Vec<u8>>,
    shared: Rc<RefCell<Vec<u8>>>,
}

// Silent — `Clone`, but no sibling shares through `Rc`.
#[derive(Clone)]
struct NoSharedSibling {
    items: RefCell<Vec<u8>>,
    count: Cell<u32>,
}

// Silent — the only cell already lives inside `Rc`.
#[derive(Clone)]
struct AllShared {
    items: Rc<RefCell<Vec<u8>>>,
    scratch: Rc<Cell<u32>>,
}

// Silent — a plain `Rc<Vec>` sibling is shared but not *mutable* shared
// state; the issue's sharing intent is `Rc` over a cell.
#[derive(Clone)]
struct RcNonCellSibling {
    items: RefCell<Vec<u8>>,
    shared: Rc<Vec<u8>>,
}

// Silent — `Arc` siblings point at thread-safe state; the lint keys on `Rc`.
#[derive(Clone)]
struct ArcSibling {
    items: RefCell<Vec<u8>>,
    shared: Arc<Mutex<Vec<u8>>>,
}

// Silent — `Clone` written by hand: what the impl does with the fields is
// the author's choice, not the mechanical field-copy this lint targets.
struct ManualClone {
    items: RefCell<Vec<u8>>,
    shared: Rc<RefCell<Vec<u8>>>,
}

impl Clone for ManualClone {
    fn clone(&self) -> Self {
        Self {
            items: RefCell::new(self.items.borrow().clone()),
            shared: self.shared.clone(),
        }
    }
}

fn main() {
    // Construct each struct so `dead_code` stays silent.
    let _ = AppState {
        tabs: Rc::new(RefCell::new(Vec::new())),
        undo: RefCell::new(Vec::new()),
        focused: None,
    };
    let _ = Mixer {
        scratch: Cell::new(0),
        levels: Rc::new(Cell::new(1)),
        name: String::new(),
    };
    let _ = Document {
        pages: Rc::new(RefCell::new(Vec::new())),
        cursor: RefCell::new(0),
        dirty: Cell::new(false),
    };
    let _ = SingleOwner {
        items: RefCell::new(Vec::new()),
        shared: Rc::new(RefCell::new(Vec::new())),
    };
    let _ = NoSharedSibling {
        items: RefCell::new(Vec::new()),
        count: Cell::new(0),
    };
    let _ = AllShared {
        items: Rc::new(RefCell::new(Vec::new())),
        scratch: Rc::new(Cell::new(0)),
    };
    let _ = RcNonCellSibling {
        items: RefCell::new(Vec::new()),
        shared: Rc::new(Vec::new()),
    };
    let _ = ArcSibling {
        items: RefCell::new(Vec::new()),
        shared: Arc::new(Mutex::new(Vec::new())),
    };
    let _ = ManualClone {
        items: RefCell::new(Vec::new()),
        shared: Rc::new(RefCell::new(Vec::new())),
    };
    let _ = ClosedTab {
        title: String::new(),
        url: String::new(),
    };
    let shared_log = Rc::new(RefCell::new(Vec::new()));
    let _ = Borrowed {
        log: &shared_log,
        shared: Rc::clone(&shared_log),
    };
}

// Silent — `&RefCell` borrows the shared cell: clones all point at the
// same state, exactly like the `Rc` sibling.
#[derive(Clone)]
struct Borrowed<'a> {
    log: &'a RefCell<Vec<u8>>,
    shared: Rc<RefCell<Vec<u8>>>,
}
