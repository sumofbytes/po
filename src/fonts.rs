//! Which fonts po draws with: the settings (`font.*` in the config over the theme's), checked
//! against the fonts installed, with a bundled monospace font as the last resort.

use std::sync::{Once, OnceLock};

use gpui::{App, SharedString, font};
use serde_json::Value;

const UI_SIZE: f32 = 15.0;
const MONO_SIZE: f32 = 14.0;
/// Sizes outside this are a typo, not a wish.
pub const SIZES: std::ops::RangeInclusive<f32> = 8.0..=48.0;
const SYSTEM: &str = ".SystemUIFont";

/// The monospace font in the binary, so the viewer's columns line up even where no monospace font is installed.
const BUNDLED_MONO: &[u8] = include_bytes!("../assets/fonts/FiraMono-Medium.ttf");
const BUNDLED_MONO_FAMILY: &str = "Fira Mono";

/// The keys of the config's `font` block.
const KEYS: [&str; 6] = ["family", "size", "mono_family", "mono_size", "transcript_family", "transcript_size"];

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
    /// The conversation's text; what it leaves unset follows `ui`.
    transcript: Role,
}

/// The fonts to draw with.
pub struct Fonts {
    pub ui: SharedString,
    pub ui_size: f32,
    pub mono: SharedString,
    pub mono_size: f32,
    /// The conversation's own family and size, when set; it draws with the UI font otherwise.
    pub transcript: Option<SharedString>,
    pub transcript_size: Option<f32>,
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
        transcript: role(config, "transcript_family", "transcript_size", &Value::Null, "", ""),
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

/// The monospace fonts the system prefers: what fontconfig says `monospace` is, which is
/// the user's own choice, else a few well-known ones per platform.
fn system_mono() -> &'static [String] {
    static FOUND: OnceLock<Vec<String>> = OnceLock::new();
    FOUND.get_or_init(system_mono_uncached)
}

fn system_mono_uncached() -> Vec<String> {
    let mut found = Vec::new();
    if cfg!(all(unix, not(target_os = "macos"))) {
        // A broken fontconfig can hang; give it a moment rather than hold up startup.
        let child = std::process::Command::new("fc-match")
            .args(["monospace", "-f", "%{family}"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            let (tx, rx) = std::sync::mpsc::channel();
            if let Some(mut stdout) = child.stdout.take() {
                std::thread::spawn(move || {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut stdout, &mut text).ok();
                    tx.send(text).ok();
                });
            }
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(output) => found.extend(output.split(',').map(|name| name.trim().to_string()).filter(|name| !name.is_empty())),
                Err(_) => log::warn!("fonts: fc-match did not answer; using the well-known monospace fonts"),
            }
            // A hung one is stopped, which also ends the thread reading from it.
            child.kill().ok();
            child.wait().ok();
        }
    }
    let known: &[&str] = if cfg!(target_os = "macos") {
        &["SF Mono", "Menlo", "Monaco"]
    } else if cfg!(windows) {
        &["Cascadia Mono", "Consolas", "Courier New"]
    } else {
        &["DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono", "Ubuntu Mono"]
    };
    found.extend(known.iter().map(|name| name.to_string()));
    found
}

/// The family the system UI font resolves to, so text measured by name matches what is drawn.
fn system_ui(installed: &[String], cx: &App) -> SharedString {
    let text_system = cx.text_system();
    let resolved = text_system.get_font_for_id(text_system.resolve_font(&font(SYSTEM))).map(|f| f.family);
    match resolved {
        Some(family) if family != SYSTEM && installed.contains(&family.to_string()) => family,
        _ => SYSTEM.into(),
    }
}

/// Makes the bundled font available by name. Once, as the fonts are kept for the whole run.
pub fn register(cx: &App) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Err(error) = cx.text_system().add_fonts(vec![BUNDLED_MONO.into()]) {
            log::error!("bundled font: {error}");
        }
    });
}

/// The fonts for the theme `theme` (its JSON) under the user's `config`.
pub fn resolve(theme: &Value, config: &Value, cx: &App) -> Fonts {
    register(cx);
    let installed = cx.text_system().all_font_names();
    let Settings { ui, mono, transcript } = settings(theme, config);
    let ui_family = match pick(&ui.families, &installed, "UI") {
        Some(family) => family.into(),
        None => system_ui(&installed, cx),
    };
    let mono_family = match pick(&mono.families, &installed, "monospace") {
        Some(family) => family.into(),
        None => system_mono()
            .iter()
            .find_map(|family| installed_name(&installed, family))
            .unwrap_or_else(|| BUNDLED_MONO_FAMILY.into())
            .into(),
    };
    let transcript_family = pick(&transcript.families, &installed, "transcript").map(SharedString::from);
    Fonts {
        ui: ui_family,
        ui_size: ui.size.unwrap_or(UI_SIZE),
        mono: mono_family,
        mono_size: mono.size.unwrap_or(MONO_SIZE),
        transcript: transcript_family,
        transcript_size: transcript.size,
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
    fn the_transcript_has_its_own_font_only_when_asked() {
        let got = settings(&Value::Null, &json!({ "font": { "transcript_family": "Serif", "transcript_size": 17 } }));
        assert_eq!(got.transcript, Role { families: vec!["Serif".into()], size: Some(17.0) });
        assert_eq!(settings(&Value::Null, &json!({})).transcript, Role { families: vec![], size: None });
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
        assert_eq!(pick(&[SYSTEM.to_string()], &installed, "test").as_deref(), Some(SYSTEM));
    }
}
