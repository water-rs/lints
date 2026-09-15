//! `image_without_label` fixture: a `Photo`/`Image`/`Svg` whose chain
//! carries no `.a11y_label(..)` and no `.a11y_hidden(true)` warns; a label
//! or `.a11y_hidden(true)` anywhere in the chain, and a value bound to a
//! name by `let`, stay silent.

use waterui::media::{Image, Photo, Url, photo::photo};
use waterui::prelude::*;
use waterui::svg::Svg;

fn main() {
    // Fires — a bare `photo(..)` enters the tree with no label.
    let _ = photo(Url::parse("https://water-rs.dev/a.png").unwrap());
    // Fires — a modifier chain without a label.
    let _ = Photo::new(Url::parse("https://water-rs.dev/b.png").unwrap()).padding();
    // Fires — `Image` has no label parameter either.
    let _ = Image::new(vec![0; 4], 1, 1);
    // Fires — `Svg` is an image too.
    let _ = Svg::new("M3 12h18");
    // Fires — `.a11y_hidden(false)` is not a label.
    let _ = photo(Url::parse("https://water-rs.dev/c.png").unwrap()).a11y_hidden(false);

    // Silent — the label sits in the same chain.
    let _ = photo(Url::parse("https://water-rs.dev/d.png").unwrap()).a11y_label("a river");
    // Silent — the label can appear anywhere in the chain.
    let _ = photo(Url::parse("https://water-rs.dev/e.png").unwrap())
        .padding()
        .a11y_label("a river");
    // Silent — `.a11y_hidden(true)` marks a decorative image.
    let _ = Image::new(vec![0; 4], 1, 1).a11y_hidden(true);
    // Silent — the binding can be labelled in a later statement.
    let p = photo(Url::parse("https://water-rs.dev/f.png").unwrap());
    let _ = p.a11y_label("a river");
    // Silent — a use of a binding is not a construction.
    let p2 = photo(Url::parse("https://water-rs.dev/g.png").unwrap());
    let _ = vstack((p2,));
}
