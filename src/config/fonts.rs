//! Which fonts po draws with: the bundled Geist and Geist Mono are the default and the last
//! resort, so po looks the same whatever is installed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use rune::{Family, TextStyle};
use rune_kit::Type;
use serde_json::Value;

/// Sizes outside this are a typo, not a wish.
pub const SIZES: std::ops::RangeInclusive<f32> = 8.0..=48.0;

/// The fonts in the binary.
pub const BUNDLED: [&[u8]; 12] = [
    include_bytes!("../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Italic.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../../assets/fonts/Literata-Variable.ttf"),
    include_bytes!("../../assets/fonts/Literata-Italic-Variable.ttf"),
];
pub const BUNDLED_UI_FAMILY: &str = "Geist";
pub const BUNDLED_MONO_FAMILY: &str = "Geist Mono";
/// The serif markdown is read in.
pub const BUNDLED_BOOK_FAMILY: &str = "Literata";

/// The UI size the type scale is measured from: a `font.size` of this leaves every style as drawn.
pub const BASE_UI_SIZE: f32 = 14.0;
/// The same for `font.mono_size`.
pub const BASE_MONO_SIZE: f32 = 13.0;

/// The family `font.book_family` names, or the bundled serif; what [`book_family`] hands out.
static BOOK_FAMILY: Mutex<&'static str> = Mutex::new(BUNDLED_BOOK_FAMILY);

/// `font.book_size` as bits; zero when the config doesn't name one.
static BOOK_SIZE: AtomicU32 = AtomicU32::new(0);

static UI_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static MONO_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// Takes the sizes the config names (`font.size`, `font.mono_size`); one it doesn't name, or names
/// outside [`SIZES`], is the base. Everything drawn through [`ui`], [`mono`] and [`scale_type`]
/// follows, from the next frame.
pub fn set_sizes(config: &Value) {
    let (ui, mono) = scales(config);
    UI_SCALE.store(ui.to_bits(), Ordering::Relaxed);
    MONO_SCALE.store(mono.to_bits(), Ordering::Relaxed);
    set_book_family(config);
    set_book_size(config);
}

/// Takes `font.book_size`, or none when the config names none or one outside [`SIZES`].
fn set_book_size(config: &Value) {
    let book = config["font"]["book_size"]
        .as_f64()
        .map(|n| n as f32)
        .filter(|n| SIZES.contains(n));
    BOOK_SIZE.store(book.unwrap_or(0.0).to_bits(), Ordering::Relaxed);
}

/// The size markdown is read at, in points, when `font.book_size` names one.
pub fn book_size() -> Option<f32> {
    Some(f32::from_bits(BOOK_SIZE.load(Ordering::Relaxed))).filter(|n| *n > 0.0)
}

/// Takes `font.book_family`, the face the transcript's markdown is read in: the first name it
/// gives, or the bundled serif. The name is leaked only when it changes.
fn set_book_family(config: &Value) {
    let named = match &config["font"]["book_family"] {
        Value::String(name) => name.split(',').next(),
        Value::Array(list) => list.iter().find_map(Value::as_str),
        _ => None,
    };
    let name = named.map(str::trim).filter(|n| !n.is_empty());
    let mut current = BOOK_FAMILY.lock().expect("book family");
    match name {
        Some(n) if n != *current => *current = Box::leak(n.to_string().into_boxed_str()),
        None => *current = BUNDLED_BOOK_FAMILY,
        _ => {}
    }
}

/// The family markdown is read in.
pub fn book_family() -> &'static str {
    *BOOK_FAMILY.lock().expect("book family")
}

/// The UI and the monospace scale the config's sizes make.
fn scales(config: &Value) -> (f32, f32) {
    let size = |key: &str, base: f32| {
        config["font"][key]
            .as_f64()
            .map(|n| n as f32)
            .filter(|n| SIZES.contains(n))
            .unwrap_or(base)
            / base
    };
    (
        size("size", BASE_UI_SIZE),
        size("mono_size", BASE_MONO_SIZE),
    )
}

fn ui_scale() -> f32 {
    f32::from_bits(UI_SCALE.load(Ordering::Relaxed))
}

fn mono_scale() -> f32 {
    f32::from_bits(MONO_SCALE.load(Ordering::Relaxed))
}

fn scaled(style: TextStyle, by: f32) -> TextStyle {
    TextStyle {
        size: style.size * by,
        line_height: style.line_height * by,
        ..style
    }
}

/// A UI text style of `size` points at the base size, as the configured size makes it.
pub fn ui(size: f32, weight: f32) -> TextStyle {
    scaled(TextStyle::new(size, weight), ui_scale())
}

/// A style of the system monospace font, `size` points on a `line` high line at the base size, as
/// the configured monospace size makes it.
pub fn mono(size: f32, line: f32) -> TextStyle {
    scaled(
        TextStyle {
            family: Family::SystemMono,
            line_height: line,
            ..TextStyle::new(size, 400.0)
        },
        mono_scale(),
    )
}

/// The kit's type, at the configured sizes: the kit's own controls and Po's draw alike.
pub fn scale_type(ty: &mut Type) {
    let ui = ui_scale();
    for style in [
        &mut ty.ui,
        &mut ty.ui_medium,
        &mut ty.ui_compact,
        &mut ty.small,
        &mut ty.small_medium,
        &mut ty.body,
        &mut ty.body_medium,
        &mut ty.text_small,
        &mut ty.text_small_medium,
        &mut ty.text_xsmall,
        &mut ty.text_xsmall_medium,
    ] {
        *style = scaled(*style, ui);
    }
    ty.mono = scaled(ty.mono, mono_scale());
}

/// The families the config names as one CSS list, over the bundled `base` and the system's `last`
/// resort. A name or a list of names, as `font.family` and `font.mono_family` take them.
pub fn family_list(named: &Value, base: &str, last: &str) -> String {
    let names: Vec<&str> = match named {
        Value::String(name) => vec![name.as_str()],
        Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    names
        .into_iter()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .chain([base, last])
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn families_come_from_a_name_or_a_list_over_the_bundled_one() {
        assert_eq!(
            family_list(&json!("Iosevka"), "Geist", "system-ui"),
            "Iosevka, Geist, system-ui"
        );
        assert_eq!(
            family_list(&json!(["A", " B ", ""]), "Geist", "system-ui"),
            "A, B, Geist, system-ui"
        );
        assert_eq!(
            family_list(&json!([]), "Geist", "system-ui"),
            "Geist, system-ui"
        );
        assert_eq!(
            family_list(&Value::Null, "Geist", "system-ui"),
            "Geist, system-ui"
        );
    }

    #[test]
    fn the_book_family_is_the_first_name_given_or_the_bundled_serif() {
        set_book_family(&json!({ "font": { "book_family": ["Geist", "X"] } }));
        assert_eq!(book_family(), "Geist");
        set_book_family(&json!({ "font": { "book_family": " Iosevka , Y" } }));
        assert_eq!(book_family(), "Iosevka");
        set_book_family(&json!({}));
        assert_eq!(book_family(), BUNDLED_BOOK_FAMILY);
    }

    #[test]
    fn the_book_size_is_only_taken_in_range() {
        set_book_size(&json!({ "font": { "book_size": 17 } }));
        assert_eq!(book_size(), Some(17.0));
        set_book_size(&json!({ "font": { "book_size": 99 } }));
        assert_eq!(book_size(), None);
        set_book_size(&json!({}));
        assert_eq!(book_size(), None);
    }

    #[test]
    fn sizes_scale_from_the_base_and_a_bad_one_is_the_base() {
        assert_eq!(scales(&Value::Null), (1.0, 1.0));
        assert_eq!(
            scales(&json!({ "font": { "size": 28, "mono_size": 26 } })),
            (2.0, 2.0)
        );
        assert_eq!(
            scales(&json!({ "font": { "size": 7, "mono_size": "big" } })),
            (1.0, 1.0),
            "outside SIZES or not a number"
        );
        assert_eq!(
            scales(&json!({ "font": { "size": 14, "mono_size": 12 } })).1,
            12.0 / 13.0
        );
    }

    #[test]
    fn a_style_scales_its_size_and_its_line() {
        let style = scaled(
            TextStyle {
                line_height: 19.0,
                ..TextStyle::new(13.0, 400.0)
            },
            2.0,
        );
        assert_eq!((style.size, style.line_height), (26.0, 38.0));
        assert_eq!(
            scaled(TextStyle::new(13.0, 400.0), 2.0).line_height,
            0.0,
            "0 stays the kit's own 1.25 × size"
        );
    }
}
