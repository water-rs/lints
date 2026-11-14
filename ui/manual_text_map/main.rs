//! `manual_text_map` fixture: `signal.map(|v| format!(..))` shapes that feed a
//! text position warn; maps that never reach text stay silent.

use waterui::prelude::*;
use waterui::reactive::map::map;
use waterui::reactive::zip::zip;
use waterui::text::IntoText;

/// A plain `impl IntoText` parameter — a text position that isn't `text(..)`.
fn takes_text(_: impl IntoText) {}

/// A plain `String` sink — not a text position.
fn takes_string(_: String) {}

#[derive(Clone)]
struct User {
    name: String,
}

struct Model {
    count: Binding<i32>,
}

impl Model {
    fn badge(&self) -> impl View {
        // Fires — `self.count` is not a bare identifier, so the fix keeps the
        // closure's parameter name as the slot and aliases the receiver.
        text(self.count.map(|v| format!("{v} left")).computed())
    }
}

/// A module without the prelude — `text` resolves to nothing here, so the
/// fix inserts `use waterui::text;` alongside the replacement.
mod no_prelude {
    use waterui::Binding;
    use waterui::reactive::{SignalExt, binding};
    use waterui::text::IntoText;

    fn into_text_param(_: impl IntoText) {}

    pub(super) fn run() {
        let count: Binding<i32> = binding(7);
        // Fires — `impl IntoText` argument.
        into_text_param(count.map(|c| format!("{c} unread")).computed());
    }
}

fn main() {
    let count: Binding<i32> = binding(3);
    let name: Binding<String> = binding("Ada".to_string());
    let price: Binding<f64> = binding(9.99);
    let selection: Binding<i32> = binding(1);
    let first: Binding<String> = binding("Ada".to_string());
    let last: Binding<String> = binding("Lovelace".to_string());
    let width: Binding<usize> = binding(6usize);
    let total: Binding<i32> = binding(42);
    let user: Binding<User> = binding(User {
        name: "Ada".to_string(),
    });
    let log: Binding<String> = binding(String::new());

    // Fires — the issue's picker shape: a `let`-bound map whose only use is a
    // `text!` capture. Help only — the `let` and the capture both go away.
    let selection_text = selection.clone().map(|fruit| format!("{fruit:?}"));
    let _ = hstack((text("Selected: "), text!("{selection_text}")));

    // Fires — `format!` inside `map` feeding `text(..)`; the whole call is
    // replaced.
    let _ = text(count.map(|c| format!("{c} items")).computed());
    // Fires — `to_string` conversion of the parameter.
    let _ = text(name.map(|v| v.to_string()).computed());
    // Fires — `clone` over an already-string parameter.
    let _ = text(name.map(|v| v.clone()).computed());
    // Fires — precision is kept verbatim.
    let _ = text(price.map(|v| format!("{v:.1}")).computed());
    // Fires — alignment, width, and precision are kept verbatim.
    let _ = text(price.map(|v| format!("{v:>7.1}")).computed());
    // Fires — a positional argument bound to the parameter.
    let _ = text(count.map(|v| format!("{} items", v)).computed());
    // Fires — a named argument bound to the parameter.
    let _ = text(count.map(|v| format!("{n} named", n = v)).computed());
    // Fires — a `.into()` tail on the format is transparent.
    let _ = text(name.map(|v| -> Str { format!("{v}?").into() }).computed());
    // Fires — the free `zip(a, b)` spelling flattens to two sources.
    let _ = text(
        zip(first.clone(), last.clone())
            .map(|(f, l)| format!("{f} {l}"))
            .computed(),
    );
    // Fires — the `a.zip(&b)` method spelling.
    let _ = text(
        first
            .zip(&last)
            .map(|(f, l)| format!("{f} + {l}"))
            .computed(),
    );
    // Fires — the free `nami::map(sig, f)` spelling.
    let _ = text(map(count.clone(), |v| format!("{v}!")).computed());
    // Fires — another signal's `.get()` inside the format is captured by
    // name. The `move` clone keeps the closure `'static`.
    let total2 = total.clone();
    let _ = text(
        count
            .map(move |c| format!("{c} of {}", total2.get()))
            .computed(),
    );
    // Fires — a plain `impl IntoText` parameter; only the argument is
    // replaced.
    takes_text(count.map(|c| format!("{c} things")).computed());
    // Fires — an `IntoLabel` parameter; only the argument is replaced.
    let _ = button(count.map(|c| format!("Buy {c}")).computed());
    // Fires — the map is written inside a `text!` slot binding; the fix
    // merges it into the invocation.
    let _ = text!("{x}", x = count.map(|c| format!("{c} left")).computed());
    // Fires — escaped braces in the template survive the rewrite.
    let _ = text(count.map(|c| format!("{{{c}}} items")).computed());
    // Fires — merging into a `text!` that already has another placeholder.
    let _ = text!(
        "{name}: {x}",
        x = count.map(|c| format!("{c} left")).computed()
    );
    // Fires — a `let`-bound map whose use is a `text(..)` argument. Help only.
    let summary = total.clone().map(|t| format!("Total: {t}")).computed();
    let _ = text(summary);
    // Fires — help only: `u.name` can't name a `text!` slot.
    let _ = text(user.map(|u| format!("user {}", u.name)).computed());
    // Fires — help only: `{v:w$}` spends an argument as width.
    let _ = text(
        zip(count.clone(), width)
            .map(|(v, w)| format!("{v:w$}"))
            .computed(),
    );
    // Fires — `self.count` receiver; the alias keeps the parameter name.
    let _ = Model { count: binding(9) }.badge();
    // Fires — inserts `use waterui::text;` inside `no_prelude`.
    no_prelude::run();

    // Silent — the mapped string is set onto a `Binding`, not a text position.
    let log_line = count.map(|c| format!("count = {c}")).computed();
    log.set(log_line.get());
    // Silent — the map only produces a new `Binding<String>`'s value.
    let _stored: Binding<String> = binding(name.map(|v| format!("{v}!")).computed().get());
    // Silent — the `format!` never mentions the closure parameter.
    let _ = text(count.map(|_| format!("static {}", 0)).computed());
    // Silent — the closure doesn't produce a string.
    let _doubled = count.map(|c| c * 2);
    // Silent — a plain `String` sink is not a text position.
    takes_string(name.map(|v| v.to_string()).computed().get());
    // Silent — already `text!`; the macro's own maps are not the user's.
    let _ = text!("{count}");

    // Fires — a `Str::from` wrapper around `format!` still restates `s!`
    // in a text position.
    let _ = text(name.map(|v| Str::from(format!("<{v}>"))).computed());
}
