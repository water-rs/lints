//! `format_in_text` fixture: a `format!` with letters in its template warns
//! in `IntoText`/`IntoLabel` positions (`plural_bypass` adds a second warning
//! when an integer is spliced in); data pass-throughs, locals, and `text!`
//! itself stay silent.

use waterui::prelude::*;

struct Record {
    id: u32,
}

fn main() {
    let id: u32 = 7;
    let name = String::from("x");
    let count: i32 = 3;
    let n: f32 = 1.5;
    let record = Record { id: 42 };

    // Fires: `format_in_text` + `plural_bypass` (`record.id` is a `u32`); the
    // `{:06}` spec is carried into the `text!` slot.
    let _ = text(format!("Record #{:06}", record.id));
    // Fires: `format_in_text` — the suggestion is `text(text!("Hello, {name}"))`.
    let _ = text(format!("Hello, {}", name));
    // Fires: both — `count` is a captured integer.
    let _ = text(format!("{count} unread"));
    // Fires: `format_in_text` — an `IntoLabel` parameter.
    let _ = button(format!("Delete {}", name));
    // Fires: `format_in_text` — help only, `n * 2.0` is not a simple path.
    let _ = text(format!("Total: {}", n * 2.0));
    // Fires: `format_in_text` — the `format!` reaches text through a
    // `.to_string()` carrier; the suggestion drops the wrapper.
    let _ = text(format!("Hi {}", name).to_string());

    // Silent: no letters in the template — a data pass-through.
    let _ = text(format!("{}", id));
    // Silent: a bare spec is data, not prose.
    let _ = text(format!("{:06}", record.id));
    // Silent: `text!` is the remedy — its internal `format!` expands under
    // the `text` macro, so the lint never sees it.
    let _ = text!("Record #{id:06}", id = record.id);
    // Silent: the text position reads a local — where the `format!` ran is
    // out of scope for a callsite lint.
    let s = format!("Hello {}", name);
    let _ = text(s);
    // Silent: a `format!` that never reaches a text position.
    let _: String = format!("Total {}", n);
}
