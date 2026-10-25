//! Colour schemes as po themes. A scheme is a plain `key = value` list (a 16-colour `palette`,
//! `background`, `foreground`, ...); this turns one into the JSON a po theme is written in,
//! deriving the UI colours from the terminal ones.

use serde_json::{Value, json};

/// Every scheme, as an `@@ <name>` line followed by its `key = value` lines..
const BUNDLE: &str = include_str!("../themes/schemes.txt");

type Rgb = [f32; 3];

/// The bundled scheme `name` as a po theme, matched ignoring case.
pub fn load(name: &str) -> Option<Value> {
    convert(name, bundled(name)?)
}

/// The names of the bundled schemes.
pub fn names() -> impl Iterator<Item = &'static str> {
    BUNDLE.lines().filter_map(|line| line.strip_prefix("@@ "))
}

fn bundled(name: &str) -> Option<&'static str> {
    let mut sections = BUNDLE.split("@@ ").skip(1);
    sections.find_map(|section| {
        let (title, body) = section.split_once('\n')?;
        title.eq_ignore_ascii_case(name).then_some(body)
    })
}

fn parse_hex(value: &str) -> Option<Rgb> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|b| f32::from(b) / 255.);
    Some([byte(0)?, byte(2)?, byte(4)?])
}

fn hex(c: Rgb) -> String {
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(c[0]), byte(c[1]), byte(c[2]))
}

/// `amount` of `b` blended into `a`.
fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * amount)
}

fn luminance(c: Rgb) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// A scheme as a po theme.
fn convert(name: &str, source: &str) -> Option<Value> {
    let mut palette: [Option<Rgb>; 16] = [None; 16];
    let (mut bg, mut fg, mut cursor, mut selection) = (None, None, None, None);
    for line in source.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "palette" => {
                if let Some((index, color)) = value.split_once('=')
                    && let Ok(index) = index.trim().parse::<usize>()
                    && index < 16
                {
                    palette[index] = parse_hex(color);
                }
            }
            "background" => bg = parse_hex(value),
            "foreground" => fg = parse_hex(value),
            "cursor-color" => cursor = parse_hex(value),
            "selection-background" => selection = parse_hex(value),
            _ => {}
        }
    }
    let (bg, fg) = (bg?, fg?);
    let dark = luminance(bg) < 0.5;
    // ANSI colour `i`, else the foreground so a sparse theme still renders.
    let ansi = |i: usize| palette[i].unwrap_or(fg);
    let tint = |amount: f32| mix(bg, fg, amount);

    let (red, green, yellow, blue, magenta, cyan) = (ansi(1), ansi(2), ansi(3), ansi(4), ansi(5), ansi(6));
    let accent = tint(0.12);
    let border = tint(0.15);
    let panel = tint(0.04);
    let muted_fg = tint(0.6);
    let comment = tint(0.45);
    let selection = selection.unwrap_or_else(|| tint(0.25));
    let caret = cursor.unwrap_or(fg);
    let link = palette[12].unwrap_or(blue);
    let on = |color: Rgb| if luminance(color) < 0.5 { fg } else { bg };

    let syntax = |color: Rgb| json!({ "color": hex(color) });
    let alpha = |color: Rgb, a: &str| format!("{}{a}", hex(color));
    Some(json!({
        "name": name,
        "mode": if dark { "dark" } else { "light" },
        "is_default": false,
        "highlight": {
            "editor.foreground": hex(fg),
            "editor.background": hex(bg),
            "editor.active_line.background": alpha(fg, "14"),
            "editor.line_number": hex(comment),
            "editor.active_line_number": hex(muted_fg),
            "editor.invisible": alpha(comment, "66"),
            "syntax": {
                "keyword": syntax(magenta),
                "string": syntax(green),
                "string.escape": syntax(cyan),
                "number": syntax(yellow),
                "boolean": syntax(yellow),
                "comment": syntax(comment),
                "comment.doc": syntax(muted_fg),
                "function": syntax(blue),
                "constructor": syntax(cyan),
                "type": syntax(cyan),
                "constant": syntax(yellow),
                "property": syntax(red),
                "tag": syntax(red),
                "attribute": syntax(yellow),
                "operator": syntax(muted_fg),
                "punctuation": syntax(muted_fg),
                "variable": syntax(fg),
                "title": syntax(blue),
                "link_text": syntax(cyan),
                "link_uri": syntax(blue),
            },
        },
        "colors": {
            "background": hex(bg),
            "foreground": hex(fg),
            "border": hex(border),
            "input.border": hex(border),
            "ring": hex(tint(0.3)),
            "caret": hex(caret),
            "link": hex(link),
            "link.hover": hex(mix(link, fg, 0.3)),
            "link.active": hex(mix(link, fg, 0.5)),
            "selection.background": alpha(selection, "66"),
            "muted.background": hex(panel),
            "muted.foreground": hex(muted_fg),
            "accent.background": hex(accent),
            "accent.foreground": hex(fg),
            "popover.background": hex(panel),
            "popover.foreground": hex(fg),
            "primary.background": hex(blue),
            "primary.foreground": hex(on(blue)),
            "danger.background": hex(red),
            "danger.foreground": hex(on(red)),
            "success.background": hex(green),
            "success.foreground": hex(on(green)),
            "warning.background": hex(yellow),
            "warning.foreground": hex(on(yellow)),
            "info.background": hex(cyan),
            "info.foreground": hex(on(cyan)),
            "list.background": hex(panel),
            "list.active.background": alpha(blue, "26"),
            "list.active.border": hex(blue),
            "list.head.background": hex(panel),
            "sidebar.background": hex(panel),
            "sidebar.foreground": hex(fg),
            "sidebar.border": hex(border),
            "title_bar.background": hex(panel),
            "title_bar.border": hex(border),
            "scrollbar.background": "#00000000",
            "scrollbar.thumb.background": alpha(comment, "80"),
            "scrollbar.thumb.hover.background": hex(muted_fg),
            "tab.background": hex(panel),
            "tab.active.background": hex(bg),
            "tab.foreground": hex(muted_fg),
            "tab.active.foreground": hex(fg),
            "tab_bar.background": hex(panel),
            "overlay": alpha(bg, "b8"),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_theme_converts() {
        let mut count = 0;
        for name in names() {
            let theme = load(name).unwrap_or_else(|| panic!("{name} does not convert"));
            assert!(theme["colors"]["background"].as_str().is_some_and(|c| c.len() == 7), "{name}");
            count += 1;
        }
        assert!(count > 400, "only {count} themes bundled");
    }

    #[test]
    fn names_match_ignoring_case() {
        let theme = load("catppuccin MOCHA").unwrap();
        assert_eq!(theme["mode"], "dark");
        assert_eq!(theme["colors"]["background"], "#1e1e2e");
        assert!(load("no such scheme").is_none());
    }

    #[test]
    fn mode_follows_the_background() {
        assert_eq!(load("Catppuccin Latte").unwrap()["mode"], "light");
    }
}
