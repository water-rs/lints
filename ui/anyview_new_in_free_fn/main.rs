use waterui::prelude::*;

enum DisplayState {
    Empty,
    Loaded(&'static str),
}

fn media_view(media: &'static str) -> Text {
    text(media)
}

// Fires: both `match` arms — the arms erase different types, so `AnyView`
// is required, but the postfix `.. .anyview()` is the spelling.
fn row(state: &DisplayState) -> AnyView {
    match state {
        DisplayState::Empty => AnyView::new(vstack((text("empty"),)).spacing(8.0)),
        DisplayState::Loaded(media) => AnyView::new(media_view(media)),
    }
}

// Fires: `AnyView::new(..)` as a free fn's tail expression.
fn padded() -> AnyView {
    AnyView::new(text("x").padding())
}

// Fires: an `if`/`else` argument — not postfix-safe, so the rewrite wraps
// it in parentheses: `(if flag { .. } else { .. }).anyview()`.
fn flag_view(flag: bool) -> AnyView {
    AnyView::new(if flag { text("a") } else { text("b") })
}

// Fires: a closure inside a free fn keeps the function's context.
fn closured() -> AnyView {
    let make = || AnyView::new(text("x"));
    make()
}

struct Card;

impl Card {
    // Silent: an inherent method — `impl` internals keep the constructor
    // form.
    fn body_view(&self) -> AnyView {
        let view: AnyView = AnyView::new(text("card"));
        view
    }

    // Fires inside `inner`: a `fn` item nested in a method body is a free
    // function.
    fn render(&self) -> AnyView {
        fn inner() -> AnyView {
            AnyView::new(text("nested"))
        }
        inner()
    }
}

impl View for Card {
    // Silent: a trait-`impl` method — component internals keep the
    // constructor form.
    fn body(self, _env: &Environment) -> impl View {
        let view: AnyView = AnyView::new(text("card"));
        view
    }
}

// Silent (this lint): the argument is already an `AnyView` —
// `redundant_anyview` reports that call instead.
fn rewrap(v: AnyView) -> AnyView {
    AnyView::new(v)
}

// Silent: already the postfix `.anyview()` form.
fn postfix() -> AnyView {
    let view: AnyView = text("x").anyview();
    view
}

// Silent: a `static` initialiser is not a free `fn`.
static LAZY: fn() -> AnyView = || AnyView::new(text("lazy"));

// Silent (this lint): the call is the receiver of `.anyview()` — the
// outer erasure is `redundant_anyview`'s.
fn double_wrapped() -> AnyView {
    let view: AnyView = AnyView::new(text("a")).anyview();
    view
}

// Silent: `AnyView::new` as a function value — a path argument, not a
// call's callee.
fn as_fn() -> Vec<AnyView> {
    [text("a"), text("b")]
        .into_iter()
        .map(AnyView::new)
        .collect()
}

macro_rules! erase {
    ($v:expr) => {
        AnyView::new($v)
    };
}

// Silent: the `AnyView::new(..)` is produced by the macro's expansion.
fn via_macro() -> AnyView {
    let view: AnyView = erase!(text("x"));
    view
}

trait BoolView {
    // Silent: a trait's default method body is not a free `fn`.
    fn default_view(&self) -> AnyView {
        let view: AnyView = AnyView::new(text("d"));
        view
    }
}

impl BoolView for Card {}

mod conflict {
    use waterui::AnyView;
    use waterui::text::{Text, text};

    /// A local trait taking the `ViewExt` name.
    trait ViewExt {
        fn local_tag(&self) -> &'static str;
    }

    impl ViewExt for Text {
        fn local_tag(&self) -> &'static str {
            "t"
        }
    }

    // Fires: `ViewExt` resolves to the local trait here — the fix imports
    // the real one anonymously: `use waterui::view::ViewExt as _;`.
    pub fn tail() -> AnyView {
        let view = text("x");
        let _ = view.local_tag();
        AnyView::new(view)
    }
}

mod no_prelude {
    use waterui::AnyView;
    use waterui::text::text;

    // Fires: `ViewExt` is not in scope in this module — the fix also adds
    // `use waterui::view::ViewExt;`.
    pub fn tail() -> AnyView {
        AnyView::new(text("x"))
    }
}

fn main() {
    let _ = row(&DisplayState::Empty);
    let _ = row(&DisplayState::Loaded("media"));
    let _ = padded();
    let _ = flag_view(true);
    let _ = closured();
    let _ = Card.body_view();
    let _ = Card.body(&Environment::new());
    let _ = Card.render();
    let _ = Card.default_view();
    let _ = rewrap(text("a").anyview());
    let _ = postfix();
    let _ = LAZY();
    let _ = double_wrapped();
    let _ = as_fn();
    let _ = via_macro();
    let _ = conflict::tail();
    let _ = no_prelude::tail();
}
