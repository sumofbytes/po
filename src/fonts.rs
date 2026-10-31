//! Which fonts po draws with: the settings (`font.*` in the config over the theme's), checked
//! against the fonts installed, with the bundled Geist and Geist Mono as the default and the last resort.

use std::sync::Once;

use gpui::{App, SharedString};
use serde_json::Value;

const UI_SIZE: f32 = 15.0;
const MONO_SIZE: f32 = 14.0;
/// Sizes outside this are a typo, not a wish.
pub const SIZES: std::ops::RangeInclusive<f32> = 8.0..=48.0;

/// The fonts in the binary, so po looks the same whatever is installed.
const BUNDLED: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
];
const BUNDLED_UI_FAMILY: &str = "Geist";
const BUNDLED_MONO_FAMILY: &str = "Geist Mono";

/// The keys of the config's `font` block.
const KEYS: [&str; 4] = ["family", "size", "mono_family", "mono_size"];

/// The families to try, in order, and the size of one kind of text.
#[derive(Debug, PartialEq)]
struct Role {
    families: Vec<String>,
    size: Option<f32>,
}

/// What the theme and the config ask for, before looking at what is installed.
#[derive(Debug, PartialEq)]
struct Settings {
    ui: Role,
    mono: Role,
}

/// The fonts to draw with.
pub struct Fonts {
    pub ui: SharedString,
    pub ui_size: f32,
    pub mono: SharedString,
    pub mono_size: f32,
}

/// A family name, or a list of them to try in order.
fn families(value: &Value) -> Vec<String> {
    match value {
        Value::String(name) => vec![name.clone()],
        Value::Array(names) => names.iter().filter_map(|name| name.as_str().map(String::from)).collect(),
        Value::Null => Vec::new(),
        other => {
            log::warn!("font family must be a name or a list of names, not {other}");
            Vec::new()
        }
    }
}

/// A size in pixels, if `value` is a usable one.
fn size(value: &Value, what: &str) -> Option<f32> {
    match value.as_f64() {
        Some(n) if SIZES.contains(&(n as f32)) => Some(n as f32),
        Some(n) => {
            log::warn!("{what} {n} is outside {}..={}, ignoring it", SIZES.start(), SIZES.end());
            None
        }
        None => {
            if !value.is_null() {
                log::warn!("{what} must be a number, not {value}");
            }
            None
        }
    }
}

/// The first of the config's value and the theme's that is set.
fn role(config: &Value, config_family: &str, config_size: &str, theme: &Value, theme_family: &str, theme_size: &str) -> Role {
    let mut stack = families(&config[config_family]);
    if stack.is_empty() {
        stack = families(&theme[theme_family]);
    }
    let size = size(&config[config_size], &format!("font.{config_size}")).or_else(|| size(&theme[theme_size], theme_size));
    Role { families: stack, size }
}

fn settings(theme: &Value, config: &Value) -> Settings {
    if let Some(block) = config.get("font").and_then(Value::as_object) {
        for key in block.keys().filter(|key| !KEYS.contains(&key.as_str())) {
            log::warn!("unknown key font.{key} (expected one of {})", KEYS.join(", "));
        }
    }
    let config = &config["font"];
    Settings {
        ui: role(config, "family", "size", theme, "font.family", "font.size"),
        mono: role(config, "mono_family", "mono_size", theme, "mono_font.family", "mono_font.size"),
    }
}

/// The installed spelling of `family`, as fonts are looked up by that. Names starting with a dot are the system's own.
fn installed_name(installed: &[String], family: &str) -> Option<String> {
    if family.starts_with('.') {
        return Some(family.to_string());
    }
    installed.iter().find(|name| name.eq_ignore_ascii_case(family)).cloned()
}

/// The first family of `wanted` that is installed. Each one that isn't is reported, so a typo shows.
fn pick(wanted: &[String], installed: &[String], what: &str) -> Option<String> {
    wanted.iter().find_map(|family| {
        let found = installed_name(installed, family);
        if found.is_none() {
            log::warn!("{what} font {family:?} is not installed");
        }
        found
    })
}

/// Makes the bundled fonts available by name. Once, as the fonts are kept for the whole run.
pub fn register(cx: &App) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Err(error) = cx.text_system().add_fonts(BUNDLED.iter().map(|data| (*data).into()).collect()) {
            log::error!("bundled fonts: {error}");
        }
    });
}

/// The fonts for the theme `theme` (its JSON) under the user's `config`.
pub fn resolve(theme: &Value, config: &Value, cx: &App) -> Fonts {
    register(cx);
    let installed = cx.text_system().all_font_names();
    let Settings { ui, mono } = settings(theme, config);
    let ui_family = pick(&ui.families, &installed, "UI").unwrap_or_else(|| BUNDLED_UI_FAMILY.into());
    let mono_family = pick(&mono.families, &installed, "monospace").unwrap_or_else(|| BUNDLED_MONO_FAMILY.into());
    Fonts {
        ui: ui_family.into(),
        ui_size: ui.size.unwrap_or(UI_SIZE),
        mono: mono_family.into(),
        mono_size: mono.size.unwrap_or(MONO_SIZE),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn config_wins_over_the_theme() {
        let theme = json!({ "font.family": "Theme Sans", "font.size": 16, "mono_font.family": "Theme Mono" });
        let config = json!({ "font": { "family": ["A", "B"], "mono_size": 12 } });
        let got = settings(&theme, &config);
        assert_eq!(got.ui, Role { families: vec!["A".into(), "B".into()], size: Some(16.0) });
        assert_eq!(got.mono, Role { families: vec!["Theme Mono".into()], size: Some(12.0) });
    }

    #[test]
    fn bad_sizes_and_families_are_ignored() {
        let config = json!({ "font": { "size": 0, "mono_size": "big", "family": 3 } });
        let got = settings(&Value::Null, &config);
        assert_eq!(got.ui, Role { families: vec![], size: None });
        assert_eq!(got.mono.size, None);
    }

    #[test]
    fn picks_the_first_installed_family_ignoring_case() {
        let installed = vec!["Noto Sans".to_string(), "Fira Mono".to_string()];
        let wanted = vec!["Nope".to_string(), "fira mono".to_string(), "Noto Sans".to_string()];
        assert_eq!(pick(&wanted, &installed, "test").as_deref(), Some("Fira Mono"));
        assert_eq!(pick(&["Nope".to_string()], &installed, "test"), None);
        assert_eq!(pick(&[".SystemUIFont".to_string()], &installed, "test").as_deref(), Some(".SystemUIFont"));
    }
}
