//! `manual_string_signal` fixture: `signal.map(..)` shapes that build a
//! `String`/`Str` signal by hand — `format!`, `+`/`push_str` concatenation,
//! `from`/`to_string` conversions — warn outside text positions; maps whose
//! result reaches an `IntoText`/`IntoLabel` parameter (`manual_text_map`'s
//! `text!` case), lone conversions, and literal-only branches stay silent.

use waterui::prelude::*;
use waterui::reactive::constant;
use waterui::reactive::map::map;
use waterui::reactive::s;

/// The issue's named-function shape: branches that construct `Str` from
/// `to_string`/`format!`.
fn count_label(count: i32) -> Str {
    if count == 0 {
        Str::from("none")
    } else if count < 10_000 {
        Str::from(count.to_string())
    } else {
        Str::from(format!("{}k", count / 1000))
    }
}

/// A record storing the produced string signal — the map's result is kept,
/// not drawn.
struct Row {
    label: Computed<String>,
}

fn main() {
    let count: Binding<i32> = Binding::i32(3);
    let icount: Binding<i32> = Binding::i32(7);
    let flag: Binding<bool> = Binding::bool(false);
    let name: Binding<String> = binding("Ada".to_string());
    let first: Binding<String> = binding("Ada".to_string());
    let last: Binding<String> = binding("Lovelace".to_string());
    let handle: Binding<Str> = binding(Str::from("x"));
    let pair: Binding<(i32, i32)> = binding((1, 2));
    // `u32` itself is a `Signal` (nami constant-signal impls) — a bound
    // slot needs a type that is not.
    let total = core::num::NonZeroU32::new(8).unwrap();

    // Fires — a named `fn` whose body branches and builds `Str` from
    // `to_string`/`format!`; the condition should become a signal.
    let _label = count.map(count_label);
    // Fires — `format!` over the signal's value, stored in a struct field.
    let row = Row {
        label: count.map(|c| format!("{c} items")).computed(),
    };
    let _ = row.label;
    // Fires — `Str::from` of a `format!`, materialized into a `Binding<Str>`.
    handle.set(name.map(|n| Str::from(format!("@{n}"))).get());
    // Fires — `to_string`/`+` concatenation across a zip's two sources.
    let _ = first
        .zip(&last)
        .map(|(a, b)| a.to_string() + " / " + &b.to_string());
    // Fires — `push_str` appending parameter-derived text.
    let _ = icount.map(|c| {
        let mut s = String::from("n=");
        s.push_str(&c.to_string());
        s
    });
    // Fires — `+=` concatenation onto a hand-built `String`.
    let _ = icount.map(|c| {
        let mut s = String::new();
        s += &c.to_string();
        s
    });
    // Fires — the free `nami::map` spelling, a branch that formats.
    let _ = map(icount.clone(), |c| {
        if c == 0 {
            String::new()
        } else {
            format!("{c} items")
        }
    });
    // Fires — one bound slot next to a captured signal: `s!` named mode
    // needs every placeholder named, so the sketch binds `c = &count` and
    // `total = &constant(total)`. The sketch itself compiles — see below.
    let _ = count.map(move |c| format!("{c}/{total}"));
    // Fires — a positional capture mixed with a bound slot spells the
    // capture `count = &count`.
    let _ = count.map(move |c| format!("{}/{}", c, total));
    // Fires — a tuple field cannot be a slot name: `t.0` binds as `arg0`.
    let _ = pair.map(|t| format!("{}", t.0));
    // Fires — `+=` accumulates onto the parameter itself.
    let _ = name.map(|mut v: String| {
        v += "!";
        v
    });

    // Silent — the map's result reaches an `IntoText` parameter; that case
    // is `manual_text_map`'s `text!` rewrite (which does fire below).
    let _ = text(count.map(|c| format!("{c}")).computed());
    // Silent — the `let`-bound result's only use reaches a text position.
    let summary = name.map(|n| format!("{n}!"));
    let _ = text(summary.computed());
    // Silent — a lone `v.to_string()` is a plain conversion, `map_into`
    // territory.
    let _ = name.map(|v| v.to_string());
    // Silent — `Str::from(..)` of a plainly converted value builds nothing.
    let _ = name.map(|v| Str::from(v.clone()));
    // Silent — a lone `v.to_string()` stays a plain conversion under
    // `.into()`/`Str::from(..)` wrappers too.
    let _ = name.map(|v| -> Str { v.to_string().into() });
    let _ = name.map(|v| Str::from(v.to_string()));
    // Silent — the `s!` spellings the sketches above emit; the fixture
    // compiling is the proof they are valid nami-derive 0.3.1. Named
    // values go through `ToOwned::to_owned(..)`, which takes a reference.
    let _ = s!("{c}/{total}", c = &count, total = &constant(total));
    let _ = s!("{count}/{total}", count = &count, total = &constant(total));
    let _ = s!("{arg0}", arg0 = &pair.map(|t| t.0));
    // Silent — a constant-signal value auto-captures by name alone.
    let raw: u32 = 8;
    let _ = s!("{count}/{raw}");
    // Silent — literal-only branches build nothing by hand
    // (`manual_signal_combinator`'s `select` case, not this lint's).
    let _ = flag.map(|f| if f { "on" } else { "off" });
    // Silent — `s!` is already the reactive `format!`.
    let _sig = s!("{count}");
    // Silent — the `format!` never reads the parameter.
    let _ = icount.map(|_| format!("static {}", 0));
    // Silent — `Iterator::map` is not `SignalExt::map`.
    let _ = [1, 2]
        .into_iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>();
}
