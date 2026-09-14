//! `qualified_waterui_path` fixture: qualified `waterui`/`waterui_*`/`nami`
//! paths warn in every syntactic position; `use` items, bare names,
//! non-family crates, and path-like text in attributes stay silent.

use waterui as w;
use waterui::layout;
use waterui::layout::stack::hstack;
use waterui::prelude::*;

/// `waterui::layout::stack::vstack` inside a doc attribute — attribute contents are
/// not paths, so this must stay silent.
#[doc = "mentions waterui::layout::stack::vstack and waterui::text! in prose"]
struct Kiosk;

fn main() {
    let _k = Kiosk;
    let _ = inner::build();

    // Expression position — the locale example's `waterui::regional::` calls.
    let _tz = waterui::regional::current_settings().timezone().to_string();
    let _ctx = waterui::regional::current_settings();
    waterui::regional::set_locale_tag("en-US").expect("locale tag must parse");
    let on_pick =
        |code: &str| waterui::regional::set_locale_tag(code).expect("locale tag must parse");
    on_pick("zh-TW");

    // The prelude already binds these names — the fix is a bare strip.
    let _stack = waterui::layout::stack::vstack((text("a"), text("b")));

    // `row` collides (local fn + the prelude's list `row`) — alias import.
    let _grid = waterui::layout::row((text("cell"),));
    let _typed: waterui::layout::Divider = waterui::layout::Divider;
    let waterui::layout::Divider = _typed;
    let _inline = waterui::text::Text::new("inline");
    let _greeting = waterui::text!("welcome");

    // `hstack` is imported explicitly — strip only, no second `use`.
    let _direct = hstack((text("d"),));
    let _pair = waterui::layout::stack::hstack((text("l"), text("r")));

    // `use waterui as w` — the crate root is found by resolution, not text.
    let _stack_w = w::layout::stack::vstack((text("w"),));
    // `locale` is a prelude name that happens to be a re-exported crate root;
    // it reads as a module path like `layout::Divider` below, so the value
    // side stays silent while the `w::` type is still flagged.
    let _loc: w::locale::Locale = locale::locales::EN_US.clone();
    let _rule: layout::Divider = layout::Divider;

    // A local `fn row` collides — the fix must import the list `row` and the
    // grid `row` under aliases.
    fn row() {}
    row();
    let _list = waterui::component::list::row("Serial", "A-42");

    // Trait-bound position.
    show(waterui::layout::Divider);

    // `nami` is in the same family — flag and import like `waterui::`.
    let _const = nami::constant(0);

    // Macro invocations — `text` is in the prelude, `debug` is not.
    waterui::log::debug!("kiosk ready");

    // Silent: bare imported names, a non-family crate, a local module.
    let _plain = vstack((text("bare"),));
    let _s = std::string::String::new();
    let _m = helpers::marker();
}

fn show(_v: impl waterui::view::View) {}

mod helpers {
    pub fn marker() -> u32 {
        1
    }
}

// No `use` items in this module — the fix inserts at the module head.
mod inner {
    pub fn build() -> waterui::text::Text {
        waterui::text::text("i")
    }
}
