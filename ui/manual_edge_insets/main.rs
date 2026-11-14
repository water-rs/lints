use waterui::prelude::*;

const PAD: f32 = 8.0;
// Silent: a `const`, not a parameter argument.
const E: EdgeInsets = EdgeInsets::all(1.0);

fn takes(_: impl Into<EdgeInsets>) {}

struct S {
    insets: EdgeInsets,
}

fn main() {
    // Silent: a `let` binding, not a parameter argument.
    let e = EdgeInsets::all(1.0);
    let x = 2.0f32;
    // `p`'s type is pinned `f32` only by the `symmetric` call below.
    let p = 8.0;

    // Fires: `all` — the parameter already takes the scalar.
    let _ = text("a").padding_with(EdgeInsets::all(16.0));
    // Fires: `symmetric` with a zero vertical axis — the whole call becomes
    // `padding_horizontal`.
    let _ = text("b").padding_with(EdgeInsets::symmetric(0.0, 12.0));
    // Fires: `symmetric` with a zero horizontal axis — `padding_vertical`.
    let _ = text("c").padding_with(EdgeInsets::symmetric(8.0, 0.0));
    // Fires: `symmetric` — the `(vertical, horizontal)` pair.
    let _ = text("d").padding_with(EdgeInsets::symmetric(8.0, 16.0));
    // Fires: `new` — the `[top, bottom, leading, trailing]` array.
    let _ = text("e").padding_with(EdgeInsets::new(1.0, 2.0, 3.0, 4.0));
    // Fires: a path operand rewrites to itself.
    let _ = text("f").padding_with(EdgeInsets::all(PAD));
    // Fires: a path operand mixed with an unsuffixed float — the literal
    // keeps an `f32` pin in the pair (`(f32, f64)` has no `From` impl).
    let _ = text("g").padding_with(EdgeInsets::symmetric(PAD, 16.0));
    // Fires: `new` mixing a path and literals — array elements unify to
    // `f32`, no suffix needed.
    let _ = text("h").padding_with(EdgeInsets::new(PAD, 2.0, 3.0, 4.0));
    // Fires `MaybeIncorrect`: `p` is a `Res::Local` whose type re-infers
    // without `symmetric`'s `f32` pin — the plain pair is for a human.
    let _ = text("i").padding_with(EdgeInsets::symmetric(p, 16.0));
    // Fires: two `const` paths — nothing re-infers, `(PAD, PAD)` is `f32`.
    let _ = text("j").padding_with(EdgeInsets::symmetric(PAD, PAD));
    // Fires: a non-`padding_with` `impl Into<EdgeInsets>` sink — the plain
    // `all` rewrite.
    takes(EdgeInsets::all(4.0));
    // Fires: at a plain `impl Into<EdgeInsets>` position the pair always
    // compiles — `(PAD, 16.0)` stays unsuffixed and `MachineApplicable`.
    takes(EdgeInsets::symmetric(PAD, 16.0));

    // Silent: `EdgeInsets::default()` is not a flagged constructor.
    let _ = text("k").padding_with(EdgeInsets::default());
    // Silent: a computed operand — `x * 2.0` is not a literal or a path.
    let _ = text("l").padding_with(EdgeInsets::all(x * 2.0));
    // Silent: already a conversion form.
    let _ = text("m").padding_with(16.0);
    // Silent: a path — the `EdgeInsets` is the value, not the bug.
    let _ = text("n").padding_with(e);
    let _ = text("o").padding_with(E);
    // Silent: a struct field, not a parameter argument.
    let s = S {
        insets: EdgeInsets::all(1.0),
    };
    let _ = s.insets;
}
