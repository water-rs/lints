//! `manual_signal_zip_op` fixture: `a.zip(&b).map(|(x, y)| x op y)` warns
//! when the two-operand closure restates `a.and(&b)`, `a.or(&b)`, or the
//! `a op b` signal operator; anything else stays silent.

use waterui::prelude::*;

struct Scaler {
    factor: Binding<i32>,
}

impl Scaler {
    fn apply(&self, count: Binding<i32>) {
        // Fires — `*` is the operator; `self.factor` is a field, so the fix
        // clones it: `count.clone() * self.factor.clone()`.
        let _ = count.zip(&self.factor).map(|(x, y)| x * y);
    }
}

fn main() {
    let flag: Binding<bool> = Binding::bool(false);
    let other_flag: Binding<bool> = Binding::bool(true);
    let third_flag: Binding<bool> = Binding::bool(false);
    let count: Binding<i32> = Binding::i32(1);
    let other: Binding<i32> = Binding::i32(2);
    let base: Binding<i32> = Binding::i32(0);

    // Fires — `x && y` on bools is `flag.and(&other_flag)`.
    let _ = flag.zip(&other_flag).map(|(x, y)| x && y);
    // Fires — `x || y` on bools is `flag.or(&other_flag)`.
    let _ = flag.zip(&other_flag).map(|(x, y)| x || y);
    // Fires — `x + y` is the operator: `count.clone() + other.clone()`.
    let _ = count.zip(&other).map(|(x, y)| x + y);
    // Fires — the `&b.clone()` spelling peels to `other`.
    let _ = count.zip(&other.clone()).map(|(x, y)| x + y);
    // Fires — a temporary receiver stays verbatim:
    // `count.map(|v| v) - other.clone()`.
    let _ = count.map(|v| v).zip(&other).map(|(x, y)| x - y);
    // Fires — the pair read through `t.0`/`t.1`.
    let _ = count.zip(&other).map(|t| t.0 + t.1);
    // Fires — a `&`-typed zip argument is respelled without `&` for
    // `and`/`or` and dereferenced for the operators.
    let borrowed_flag: &Binding<bool> = &other_flag;
    let borrowed_count: &Binding<i32> = &other;
    let _ = flag.zip(borrowed_flag).map(|(x, y)| x && y);
    let _ = count.zip(borrowed_count).map(|(x, y)| x + y);
    // Fires — a `&`-typed receiver derefs to the place:
    // `(*borrowed_count).clone()`.
    let _ = borrowed_count.zip(&count).map(|(x, y)| x + y);
    // Fires — the rewrite feeds a method receiver, so it parenthesizes:
    // `(count.clone() + other.clone()).map(|v| v * 2)`.
    let _ = count.zip(&other).map(|(x, y)| x + y).map(|v| v * 2);
    // Fires — a `Binary` rhs would rebind a bare `-`, so the fix
    // parenthesizes: `base - (count.clone() - other.clone())`.
    let _ = base - count.zip(&other).map(|(x, y)| x - y);

    Scaler {
        factor: Binding::i32(3),
    }
    .apply(count.clone());

    // Silent — `x == y` is no named signal operation.
    let _ = count.zip(&other).map(|(x, y)| x == y);
    // Silent — swapped operands: `y && x` reads the pair out of order.
    let _ = flag.zip(&other_flag).map(|(x, y)| y && x);
    // Silent — `x` read twice is not the `x op y` pair.
    let _ = count.zip(&other).map(|(x, _y)| x + x);
    // Silent — a nested zip's `((x, y), z)` is not a `(x, y)` pair.
    let _ = flag
        .zip(&other_flag)
        .zip(&third_flag)
        .map(|((x, y), z)| x && y && z);
    // Silent — the body also reads a captured binding.
    let _ = flag
        .zip(&other_flag)
        .map(move |(x, y)| x && y && third_flag.get());
    // Silent — `Iterator::zip`/`map` are not signal combinators.
    let _ = [1, 2].into_iter().zip([3, 4]).map(|(x, y)| x + y);
    // Silent — `Option::zip`/`map` are not signal combinators.
    let _ = Some(1).zip(Some(2)).map(|(x, y)| x + y);
}
