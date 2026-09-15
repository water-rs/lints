//! `missing_translation`/`orphan_translation` fixture: keys used by `text!`
//! and `Text::localized` are checked against `i18n/en.toml` and
//! `i18n/fr.toml`; keys in the catalogs that nothing uses warn on the crate
//! root.

#![allow(unknown_lints)]
#![warn(missing_translation)]
#![warn(orphan_translation)]

use waterui::prelude::*;
use waterui::text::Text;

fn main() {
    // Silent — "Save" is in en.toml and fr.toml.
    let _ = text!("Save");
    // Fires — "Cancel" is in en.toml but missing from fr.toml.
    let _ = text!("Cancel");
    // Fires — "Delete" is in neither catalog.
    let _ = Text::localized("Delete");
    // Fires — "$about_blurb" is in neither catalog.
    let _ = text!("$about_blurb");
    // Orphan — "Unused key" is in en.toml but nothing uses it; the
    // diagnostic lands on the crate root, not here.
    let _ = Text::verbatim("not a key");
}
