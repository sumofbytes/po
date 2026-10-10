use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{self, read_json};
use std::rc::Rc;

use gpui::App;
use gpui_kit::component::{Theme, ThemeConfig};
use serde_json::Value;

const DEFAULT_THEME: &str = "main";
const FONT_SIZE: f64 = 14.0;
const MONO_FONT_SIZE: f64 = 13.0;

/// Themes shipped in the binary, by name; seeded into `<config>/themes/` on
/// startup and kept as a fallback.
const BUNDLED: [(&str, &str); 3] = [
    ("main", include_str!("../themes/main.json")),
    ("moon", include_str!("../themes/moon.json")),
    ("dawn", include_str!("../themes/dawn.json")),
];

/// Where themes are picked from: `<config>/themes/`.
fn themes_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("themes")
}

/// Copy bundled themes missing from `<config>/themes/` there, so they can be
/// edited without rebuilding; existing files are left alone.
fn seed_themes(config_dir: &Path) {
    let dir = themes_dir(config_dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        return log::warn!("theme: seeding {}: {error}", dir.display());
    }
    for (name, source) in BUNDLED {
        let path = dir.join(format!("{name}.json"));
        if path.exists() {
            continue;
        }
        if let Err(error) = fs::write(&path, source) {
            log::warn!("theme: seeding {name}: {error}");
        }
    }
}

/// The theme `name` asks for: a JSON file in `<config>/themes/`, else in the
/// config folder itself — `.json` optional, absolute paths work — else a
/// bundled one.
fn load_theme(name: &str, config_dir: Option<&Path>) -> Option<Value> {
    let try_file = |path: &Path| -> Option<Value> {
        read_json(path).or_else(|| {
            path.extension()
                .is_none()
                .then(|| path.with_extension("json"))
                .and_then(|path| read_json(&path))
        })
    };
    if let Some(dir) = config_dir {
        for base in [themes_dir(dir), dir.to_path_buf()] {
            if let Some(theme) = try_file(&base.join(name)) {
                return Some(theme);
            }
        }
    } else if let Some(theme) = try_file(Path::new(name)) {
        return Some(theme);
    }
    let key = name.strip_suffix(".json").unwrap_or(name);
    BUNDLED.iter().find(|(bundled, _)| *bundled == key).and_then(|(_, source)| serde_json::from_str(source).ok())
}

/// Applies the user's config (`theme`, `font.*`) over the bundled defaults.
pub fn apply(cx: &mut App) {
    let config = config::load();
    let name = std::env::var("PO_THEME")
        .ok()
        .or_else(|| config["theme"].as_str().map(String::from))
        .unwrap_or_else(|| DEFAULT_THEME.into());
    let dir = config::dir();
    let dir = dir.as_deref();
    if let Some(dir) = dir {
        seed_themes(dir);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder cleaned up when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("po-theme-{tag}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn write(&self, name: &str, json: &str) {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, json).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn picks_themes_from_the_config_folder() {
        let scratch = Scratch::new("pick");
        scratch.write("themes/solar.json", r#"{"mode":"light"}"#);
        scratch.write("flat.json", r#"{"mode":"dark"}"#);
        let dir = scratch.0.as_path();
        let theme = load_theme("solar", Some(dir)).unwrap();
        assert_eq!(theme, serde_json::json!({"mode": "light"}));
        assert_eq!(load_theme("solar.json", Some(dir)).unwrap(), theme);
        // A theme dropped straight into the config folder still works.
        assert_eq!(load_theme("flat", Some(dir)).unwrap(), serde_json::json!({"mode": "dark"}));
    }

    #[test]
    fn themes_folder_overrides_bundled() {
        let scratch = Scratch::new("override");
        scratch.write("themes/main.json", r#"{"mode":"light"}"#);
        assert_eq!(load_theme("main", Some(scratch.0.as_path())).unwrap()["mode"], "light");
    }

    #[test]
    fn seeds_missing_themes_into_the_config_folder() {
        let scratch = Scratch::new("seed");
        seed_themes(&scratch.0);
        for (name, source) in BUNDLED {
            let path = scratch.0.join("themes").join(format!("{name}.json"));
            assert_eq!(fs::read_to_string(&path).unwrap(), *source);
        }
        // Already-present files are left alone.
        scratch.write("themes/main.json", r#"{"mode":"light"}"#);
        seed_themes(&scratch.0);
        assert_eq!(fs::read_to_string(scratch.0.join("themes/main.json")).unwrap(), r#"{"mode":"light"}"#);
    }

    #[test]
    fn falls_back_to_bundled() {
        assert_eq!(load_theme("moon", None).unwrap()["mode"], "dark");
        assert_eq!(load_theme("moon.json", None).unwrap()["mode"], "dark");
        assert!(load_theme("no-such-theme", None).is_none());
    }
}
