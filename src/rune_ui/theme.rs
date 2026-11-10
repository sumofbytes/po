//! Po's colour schemes as a Rune kit. The schemes are in
//! [`crate::schemes`].
use rune::{Color, color};
use rune_kit::{Kit, PaletteFocus, PaletteSeed};
use rune_kit::kit::StateColors;
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
    let mut kit = Kit::from_palette_focus(PaletteSeed { bg, fg, accent }, PaletteFocus::Accent);
    // The status roles come from the scheme's own red, green and yellow, not the kit's fixed ones.
    let role = |name: &str, old: StateColors| match get(&format!("{name}.background")) {
        Some(c) => StateColors { bg: bg.mix(c, 0.12), fg: c, border: bg.mix(c, 0.35), icon: c },
        None => old,
    };
    kit.c.success = role("success", kit.c.success);
    kit.c.warning = role("warning", kit.c.warning);
    kit.c.danger = role("danger", kit.c.danger);
    // The kit leaves the accent to a few glyphs and paints everything else in ink. Put it on the
    // surfaces that mean "this one": the primary button, the selected row, the user's own words.
    let on_accent = get("primary.foreground").unwrap_or(bg);
    kit.c.bg_icon_button_contrast = accent;
    kit.c.submit_fg = on_accent;
    kit.c.submit_icon = on_accent;
    kit.c.bg_accent = accent;
    crate::fonts::scale_type(&mut kit.ty);
    let dark = kit.dark;
    (kit, dark)
}

/// The bundled scheme `name`, else [`DEFAULT`].
pub fn load(name: &str) -> Value {
    crate::schemes::load(name).or_else(|| crate::schemes::load(DEFAULT)).expect("the default scheme is bundled")
}
