use waterui::prelude::*;
use waterui::theme::color::{Accent, Foreground, MutedForeground};

macro_rules! header {
    () => {
        text("h")
    };
}

fn main() {
    // Fires: the bare token.
    let _ = text("a").foreground(MutedForeground);
    // Fires: the prelude's `theme_color` module alias.
    let _ = text("a").foreground(theme_color::MutedForeground);
    // Fires: the fully qualified path.
    let _ = text("a").foreground(waterui::theme::color::MutedForeground);
    // Fires: mid-chain — only the `.foreground(..)` tail is rewritten.
    let _ = text("a").caption().foreground(MutedForeground).padding();
    // Fires: a `macro_rules!` receiver — the suggestion starts at the call site.
    let _ = header!().foreground(MutedForeground);

    // Silent: `Foreground` restores the primary foreground, it is not `.muted()`.
    let _ = text("a").foreground(Foreground);
    // Silent: a different theme token.
    let _ = text("a").foreground(Accent);
    // Silent: an explicit color.
    let _ = text("a").foreground(Srgb::from_hex("#FF0000"));
    // Silent: a local, even one bound to the token.
    let color = MutedForeground;
    let _ = text("a").foreground(color);
    // Silent: `.muted()` is the remedy, not the bug.
    let _ = text("a").muted();
}
