use waterui::prelude::*;

fn main() {
    // Fires: the outer `AnyView::new` wraps a value that is already an
    // `AnyView`. The inner `AnyView::new` sits in a `V: View` parameter, so
    // `needless_anyview` reports that one separately.
    let _v: AnyView = AnyView::new(AnyView::new(text("a")));

    // Fires: `.anyview()` produced the `AnyView`; `AnyView::new` wraps it a
    // second time.
    let v: AnyView = text("a").anyview();
    let _ = AnyView::new(v);

    // Fires: `.anyview()` on an `AnyView` receiver.
    let v: AnyView = text("b").anyview();
    let _ = v.anyview();

    // Fires: `ViewExt::anyview` called as an associated function.
    let v: AnyView = text("c").anyview();
    let _ = ViewExt::anyview(v);

    // Fires: the `match` arms are already `AnyView` values — the wrapper
    // around the `match` erases them a second time.
    let a: AnyView = text("a").anyview();
    let b: AnyView = text("b").anyview();
    let flag = true;
    let _ = AnyView::new(match flag {
        true => a,
        false => b,
    });

    // Silent: erasing a concrete view once, into an `AnyView` binding.
    let _v: AnyView = AnyView::new(text("a"));

    // Silent: a single `.anyview()` into an `AnyView` binding.
    let _v: AnyView = text("a").anyview();

    // Silent: `Vec<AnyView>` built from `.anyview()` calls — each element
    // erases a concrete `Text`, not an `AnyView`.
    let views: Vec<AnyView> = [text("a"), text("b")]
        .into_iter()
        .map(|v| v.anyview())
        .collect();
    let _ = views.len();
}
