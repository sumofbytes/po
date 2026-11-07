//! Po's colour schemes as a Rune kit. The schemes are the ones the gpui theme reads
//! ([`crate::schemes`]), so both UIs start from the same colours.
use rune::{Color, color};
use rune_kit::{Kit, PaletteSeed};
use serde_json::Value;

/// What the theme was not given.
pub const DEFAULT: &str = "Rose Pine";

/// The kit for the scheme or theme file `theme` (a `colors` object like [`crate::schemes::load`]
/// returns), and whether it is a dark one.
pub fn kit(theme: &Value) -> (Kit, bool) {
    let colors = &theme["colors"];
    let get = |key: &str| colors.get(key).and_then(Value::as_str).and_then(color::hex);
    let bg = get("background").unwrap_or(Color::BLACK);
    let fg = get("foreground").unwrap_or(Color::WHITE);
    let accent = get("primary.background").unwrap_or(fg);
    let kit = Kit::from_palette(PaletteSeed { bg, fg, accent });
    let dark = kit.dark;
    (kit, dark)
}

/// The bundled scheme `name`, else [`DEFAULT`].
pub fn load(name: &str) -> Value {
    crate::schemes::load(name).or_else(|| crate::schemes::load(DEFAULT)).expect("the default scheme is bundled")
}
