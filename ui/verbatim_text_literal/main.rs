//! `verbatim_text_literal` fixture: a string literal that reaches an
//! `IntoText`/`IntoLabel` parameter through a verbatim wrapper warns; a
//! bare literal, a non-literal receiver, `format!`, a wrapper outside the
//! position, and a non-text parameter stay silent.

use waterui::prelude::*;

/// A plain `String` parameter is not a text position.
fn takes_string(_: String) {}

fn main() {
    // Fires — `Str::from_static` stores the literal verbatim.
    let _ = text(Str::from_static("Hello"));
    // Fires — `to_string` produces a `String`, which is verbatim.
    let _ = text("Hello".to_string());
    // Fires — `to_owned` produces a `String`, which is verbatim.
    let _ = text("Hello".to_owned());
    // Fires — `String::from` over a literal.
    let _ = text(String::from("Hello"));
    // Fires — `Str::from` over a literal.
    let _ = text(Str::from("Hello"));
    // Fires — `Into::into` to `String` over a literal.
    let _ = text(Into::<String>::into("Hello"));
    // Fires — `Text::verbatim` never consults the catalog.
    let _ = text(Text::verbatim("Hello"));
    // Fires — `button` takes `impl IntoLabel`, also a text position.
    let _ = button("Save".to_string());

    // Silent — a bare `&'static str` literal is `Text::localized`.
    let _ = text("Hello");
    // Silent — the receiver is a variable, not a literal.
    let name = String::from("world");
    let _ = text(name.clone());
    let _ = name;
    // Silent — `format!` is `format_in_text`, a different lint.
    let n = 1;
    let _ = text(format!("count: {n}"));
    // Silent — the wrapper sits outside the text position.
    let s = "Hello".to_string();
    let _ = text(s);
    // Silent — a wrapped literal that never reaches a text position.
    let wrapped = Str::from_static("Hello");
    let _ = wrapped;
    // Silent — a verbatim literal into a `String` parameter, not text.
    takes_string("Hello".to_string());
}
