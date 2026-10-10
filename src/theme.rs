use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::App;
use gpui_kit::component::{Theme, ThemeConfig};
use serde_json::Value;

const DEFAULT_THEME: &str = "main";
const FONT_SIZE: f64 = 14.0;
const MONO_FONT_SIZE: f64 = 13.0;

/// Themes shipped in the binary, by name.
const BUNDLED: [(&str, &str); 3] = [
    ("main", include_str!("../themes/main.json")),
    ("moon", include_str!("../themes/moon.json")),
    ("dawn", include_str!("../themes/dawn.json")),
];

/// `$XDG_CONFIG_HOME/po/config.json`, or `~/.config/po/config.json`.
fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("po").join("config.json"))
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            log::error!("{}: {error}", path.display());
            None
        }
    }
}

/// The theme `name` asks for: a bundled one, or a JSON file (relative paths
/// resolve against the config folder).
fn load_theme(name: &str, config_dir: Option<&Path>) -> Option<Value> {
    if let Some((_, source)) = BUNDLED.iter().find(|(key, _)| *key == name) {
        return serde_json::from_str(source).ok();
    }
    let path = config_dir.map_or_else(|| PathBuf::from(name), |dir| dir.join(name));
    read_json(&path)
}

/// Applies the user's config (`theme`, `font.*`) over the bundled defaults.
pub fn apply(cx: &mut App) {
    let path = config_path();
    let config = path.as_deref().and_then(read_json).unwrap_or(Value::Null);
    let name = std::env::var("PO_THEME")
        .ok()
        .or_else(|| config["theme"].as_str().map(String::from))
        .unwrap_or_else(|| DEFAULT_THEME.into());
    let dir = path.as_deref().and_then(Path::parent);
    let mut theme = load_theme(&name, dir)
        .or_else(|| {
            log::warn!("theme {name:?} not found, using {DEFAULT_THEME}");
            load_theme(DEFAULT_THEME, dir)
        })
        .expect("bundled theme");
    let font = &config["font"];
    let fields = [
        ("font.family", font["family"].clone()),
        ("font.size", font["size"].as_f64().unwrap_or(FONT_SIZE).into()),
        ("mono_font.family", font["mono_family"].clone()),
        ("mono_font.size", font["mono_size"].as_f64().unwrap_or(MONO_FONT_SIZE).into()),
    ];
    for (key, value) in fields.into_iter().filter(|(_, value)| !value.is_null()) {
        theme[key] = value;
    }
    let config: ThemeConfig = match serde_json::from_value(theme) {
        Ok(config) => config,
        Err(error) => return log::error!("theme: {error}"),
    };
    let mode = config.mode;
    let config = Rc::new(config);
    Theme::update(cx, |theme| theme.apply_config(&config));
    Theme::change(mode, None, cx);
}
