//! `long_text_key` fixture: `text!` format literals and
//! `Text::localized`/`localized_or` keys over `long_text_key_words`
//! (default 12) warn; short keys, verbatim text, and a key at the limit
//! stay silent.

#![allow(unknown_lints)]
#![warn(long_text_key)]

use waterui::prelude::*;
use waterui::text::Text;

fn main() {
    let name: Binding<Str> = binding(Str::from_static("lexo"));

    // Fires — a 15-word `text!` literal.
    let _ = text!("This is a long sentence that has more than twelve words in it for sure");
    // Fires — a 15-word `Text::localized` key.
    let _ =
        Text::localized("This is a long sentence that has more than twelve words in it for sure");
    // Fires — the `{name}` slot counts as a word; the key is 14 words.
    let _ = text!(
        "Hello {name} this sentence with a slot is also longer than twelve words total",
        name = name.clone()
    );
    // Fires — `localized_or`'s first argument is the key.
    let _ = Text::localized_or(
        "This is a long sentence that has more than twelve words in it for sure",
        "fallback",
    );

    // Silent — one word.
    let _ = text!("Save");
    // Silent — the `$` short-key convention is exactly what the lint asks for.
    let _ = text!("$about_blurb");
    // Silent — a two-word key.
    let _ = Text::localized("short key");
    // Silent — `verbatim` never consults the catalog, so its content is not a
    // key.
    let _ = Text::verbatim(
        "This verbatim sentence is long but it is not a key so nothing is translated here",
    );
    // Silent — exactly at the 12-word limit.
    let _ = text!("this key is exactly twelve words long and stays under the limit");
}
