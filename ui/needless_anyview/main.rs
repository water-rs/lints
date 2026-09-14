use waterui::prelude::*;
use waterui::widget::condition::when;

/// A `View`-bounded parameter accepts any concrete view, so erasing the
/// argument is never needed.
fn takes_view(view: impl View) -> impl View {
    view
}

/// A concrete `AnyView` parameter keeps the erasure required.
fn consume(view: AnyView) {
    let _ = view;
}

/// A struct field typed `AnyView` needs the erasure.
struct Holder {
    view: AnyView,
}

enum Row {
    Header,
    Item,
}

fn header() -> Text {
    text("header")
}

fn item() -> Button<fn(&Environment)> {
    button("item")
}

/// Fires: `-> impl View` tail expression.
fn tail() -> impl View {
    AnyView::new(text("a"))
}

/// Fires once: both `if` arms erase `Text`, and the tail position accepts
/// `impl View` — one diagnostic covers both arms.
fn pick(c: bool) -> impl View {
    if c {
        AnyView::new(text("a"))
    } else {
        AnyView::new(text("b"))
    }
}

/// Fires once: `match` arms erase the same type — one diagnostic covers both
/// arms.
fn pick_match(c: bool) -> impl View {
    match c {
        true => AnyView::new(text("a")),
        false => AnyView::new(text("b")),
    }
}

/// Fires twice: `return` and the tail inside `-> impl View` are each view
/// slots — stripping both keeps the opaque type one concrete type.
fn early(c: bool) -> impl View {
    if c {
        return AnyView::new(text("a"));
    }
    AnyView::new(text("b"))
}

/// Fires once, covering both arms and the signature: same-type `match` arms
/// under `-> AnyView` are fixed together — strip the wrappers and return
/// `impl View`.
fn pick_erased(c: bool) -> AnyView {
    match c {
        true => AnyView::new(text("a")),
        false => AnyView::new(text("b")),
    }
}

/// Fires: the body neither branches nor keeps an `AnyView` — the signature
/// can be `impl View`.
fn erased() -> AnyView {
    AnyView::new(text("a"))
}

/// Fires: `-> AnyView` with no erasure at all — the suggestion rewrites only
/// the signature.
fn passthrough() -> AnyView {
    AnyView::default()
}

/// Silent: gallery-style drawer — the arms erase different types, so
/// `AnyView` is what unifies them.
fn drawer(row: Row) -> AnyView {
    match row {
        Row::Header => AnyView::new(header()),
        Row::Item => AnyView::new(item()),
    }
}

/// Silent: `if` arms of different types — the erasure is the unification.
fn branch_erased(c: bool) -> AnyView {
    if c {
        AnyView::new(text("a"))
    } else {
        AnyView::new(item())
    }
}

/// Silent: `-> AnyView` with real branching — the signature stays.
fn keep_erased(c: bool) -> AnyView {
    if c {
        return AnyView::new(text("a"));
    }
    AnyView::new(text("b"))
}

/// Silent: `-> AnyView` whose body stores an `AnyView` in a field — the
/// signature intends erasure.
fn stores_field() -> AnyView {
    let _holder = Holder {
        view: AnyView::new(text("a")),
    };
    AnyView::new(text("b"))
}

/// Silent: `-> AnyView` whose body collects `AnyView`s.
fn stores_vec() -> AnyView {
    let _views: Vec<AnyView> = vec![AnyView::new(text("a"))];
    AnyView::new(text("b"))
}

struct Greeter;

impl Greeter {
    /// Fires: an inherent method's signature can be `impl View`.
    fn view(&self) -> AnyView {
        AnyView::new(text("a"))
    }
}

fn main() {
    // Fires: `AnyView::new` as an argument to an `impl View` parameter.
    let _ = scroll(AnyView::new(text("a")));
    // Fires: `.anyview()` in the same position.
    let _ = scroll(text("a").anyview());
    // Fires: argument to a generic `View` bound.
    let _ = takes_view(AnyView::new(text("a")));
    // Fires: element of a `TupleViews` argument — the tuple rides with its
    // container.
    let _ = vstack((AnyView::new(text("a")), text("b")));
    // Fires once: a same-type array argument to `TupleViews` — stripping must
    // keep the elements homogeneous, so one diagnostic covers both elements.
    let _ = vstack([AnyView::new(text("a")), AnyView::new(text("b"))]);
    // Fires: tail of a `ViewBuilder` closure argument.
    let _ = when(Binding::bool(true), || AnyView::new(text("a")));
    // Fires twice: `return` and tail inside a `ViewBuilder` closure argument
    // are each view slots.
    let flag = true;
    let _ = when(Binding::bool(flag), move || {
        if flag {
            return AnyView::new(text("a"));
        }
        AnyView::new(text("b"))
    });
    // Fires: `AnyView::new` as the receiver of `.anyview()` — `Self: View`.
    let _ = AnyView::new(text("a")).anyview();
    // `redundant_anyview` only: the argument position accepts `impl View`,
    // but the value erased is already an `AnyView`, so this lint steps aside.
    let v: AnyView = text("a").anyview();
    let _ = scroll(AnyView::new(v));

    // Silent: `.overlay`'s `Layer` parameter carries no `View` bound — the
    // bound lives on `impl View for Overlay` — so the call cannot be told
    // apart from `Vec::push` by the parameter's declared bounds.
    let _ = text("a").overlay(AnyView::new(text("b")));
    // Silent: a struct field initializer.
    let holder = Holder {
        view: AnyView::new(text("a")),
    };
    let _ = &holder.view;
    // Silent: a `Vec<AnyView>` literal.
    let views: Vec<AnyView> = vec![AnyView::new(text("a")), AnyView::new(text("b"))];
    let _ = views.len();
    // Silent: `.collect()` into `Vec<AnyView>` — the map closure's inferred
    // return keeps the erasure.
    let collected: Vec<AnyView> = [text("a"), text("b")]
        .into_iter()
        .map(|v| v.anyview())
        .collect();
    let _ = collected.len();
    // Silent: a concrete `AnyView` parameter.
    consume(AnyView::new(text("a")));
    // Silent: an `AnyView` binding — annotated and inferred locals may flow
    // somewhere that needs the erased type.
    let _v: AnyView = AnyView::new(text("a"));
    let _v = AnyView::new(text("a"));

    // Fires: `-> impl View` tail (see `tail`, `pick`, `pick_match`, `early`).
    let _ = tail();
    let _ = pick(true);
    let _ = pick_match(false);
    let _ = early(true);
    // Fires: `-> AnyView` signature rewrites (see `pick_erased`, `erased`,
    // `passthrough`, `Greeter::view`).
    let _ = pick_erased(true);
    let _ = erased();
    let _ = passthrough();
    let _ = Greeter.view();
    // Silent: `-> AnyView` kept by different arm types, branching, or an
    // `AnyView` that flows into a field or collection.
    let _ = drawer(Row::Header);
    let _ = drawer(Row::Item);
    let _ = branch_erased(true);
    let _ = keep_erased(false);
    let _ = stores_field();
    let _ = stores_vec();
}
