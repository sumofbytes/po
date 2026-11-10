//! Which fonts po draws with: the bundled Geist and Geist Mono are the default and the last
//! resort, so po looks the same whatever is installed.

use serde_json::Value;

/// Sizes outside this are a typo, not a wish.
pub const SIZES: std::ops::RangeInclusive<f32> = 8.0..=48.0;

/// The fonts in the binary.
pub const BUNDLED: [&[u8]; 10] = [
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
];
pub const BUNDLED_UI_FAMILY: &str = "Geist";
pub const BUNDLED_MONO_FAMILY: &str = "Geist Mono";

/// The families the config names as one CSS list, over the bundled `base` and the system's `last`
/// resort. A name or a list of names, as `font.family` and `font.mono_family` take them.
pub fn family_list(named: &Value, base: &str, last: &str) -> String {
    let names: Vec<&str> = match named {
        Value::String(name) => vec![name.as_str()],
        Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    names.into_iter().map(str::trim).filter(|name| !name.is_empty()).chain([base, last]).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn families_come_from_a_name_or_a_list_over_the_bundled_one() {
        assert_eq!(family_list(&json!("Iosevka"), "Geist", "system-ui"), "Iosevka, Geist, system-ui");
        assert_eq!(family_list(&json!(["A", " B ", ""]), "Geist", "system-ui"), "A, B, Geist, system-ui");
        assert_eq!(family_list(&json!([]), "Geist", "system-ui"), "Geist, system-ui");
        assert_eq!(family_list(&Value::Null, "Geist", "system-ui"), "Geist, system-ui");
    }
}
