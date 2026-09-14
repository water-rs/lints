use waterui::shape::{RoundedRectangle, UnevenRoundedRectangle};

const R: f32 = 8.0;

fn main() {
    let _ = RoundedRectangle::new(12.0);
    let _ = RoundedRectangle::new(0.25);
    let _ = RoundedRectangle::new(0.5);
    let _ = RoundedRectangle::new(R);
    let _ = UnevenRoundedRectangle::new(0.5, 0.25, 0.4, 0.3);
}
