use std::rc::Rc;

use gpui::App;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use serde_json::{Value, json};

fn c(hex: u32) -> String {
    format!("#{hex:06x}")
}

/// The color with an alpha byte.
fn a(hex: u32, alpha: u8) -> String {
    format!("#{hex:06x}{alpha:02x}")
}

/// The named colors of a Rosé Pine variant.
struct Variant {
    base: u32,
    surface: u32,
    overlay: u32,
    muted: u32,
    subtle: u32,
    text: u32,
    love: u32,
    gold: u32,
    rose: u32,
    pine: u32,
    foam: u32,
    iris: u32,
    low: u32,
    med: u32,
    high: u32,
}

const MAIN: Variant = Variant {
    base: 0x191724, surface: 0x1f1d2e, overlay: 0x26233a, muted: 0x6e6a86, subtle: 0x908caa,
    text: 0xe0def4, love: 0xeb6f92, gold: 0xf6c177, rose: 0xebbcba, pine: 0x31748f,
    foam: 0x9ccfd8, iris: 0xc4a7e7, low: 0x21202e, med: 0x403d52, high: 0x524f67,
};

const MOON: Variant = Variant {
    base: 0x232136, surface: 0x2a273f, overlay: 0x393552, muted: 0x6e6a86, subtle: 0x908caa,
    text: 0xe0def4, love: 0xeb6f92, gold: 0xf6c177, rose: 0xea9a97, pine: 0x3e8fb0,
    foam: 0x9ccfd8, iris: 0xc4a7e7, low: 0x2a283e, med: 0x44415a, high: 0x56526e,
};

const DAWN: Variant = Variant {
    base: 0xfaf4ed, surface: 0xfffaf3, overlay: 0xf2e9e1, muted: 0x9893a5, subtle: 0x797593,
    text: 0x575279, love: 0xb4637a, gold: 0xea9d34, rose: 0xd7827e, pine: 0x286983,
    foam: 0x56949f, iris: 0x907aa9, low: 0xf4ede8, med: 0xdfdad9, high: 0xcecacd,
};

fn build(v: &Variant, mode: ThemeMode) -> Value {
    let dark = mode == ThemeMode::Dark;
    let raised = if dark { v.overlay } else { v.low };
    let line = if dark { v.overlay } else { v.med };
    let (name, mode_name) = if dark { ("Po Dark", "dark") } else { ("Po Light", "light") };
    let syntax = |color: u32| json!({ "color": c(color) });
    json!({
        "name": name,
        "mode": mode_name,
        "is_default": false,
        "font.size": 14,
        "mono_font.size": 13,
        "colors": {
            "background": c(v.base),
            "foreground": c(v.text),
            "border": c(line),
            "input.border": c(line),
            "ring": c(v.high),
            "caret": c(v.rose),
            "link": c(v.foam),
            "link.hover": c(v.rose),
            "link.active": c(v.iris),
            "selection.background": a(v.muted, 0x66),
            "muted.background": c(v.surface),
            "muted.foreground": c(v.subtle),
            "accent.background": c(raised),
            "accent.foreground": c(v.text),
            "popover.background": c(v.surface),
            "popover.foreground": c(v.text),
            "primary.background": c(v.iris),
            "primary.hover.background": c(v.rose),
            "primary.active.background": c(v.rose),
            "primary.foreground": c(v.base),
            "secondary.background": c(raised),
            "secondary.hover.background": c(v.med),
            "secondary.active.background": c(v.med),
            "secondary.foreground": c(v.text),
            "button.background": c(raised),
            "button.hover.background": c(v.med),
            "button.active.background": c(v.med),
            "button.foreground": c(v.text),
            "button.primary.background": c(v.iris),
            "button.primary.hover.background": c(v.rose),
            "button.primary.active.background": c(v.rose),
            "button.primary.foreground": c(v.base),
            "danger.background": c(v.love),
            "danger.foreground": c(v.base),
            "success.background": c(v.foam),
            "success.foreground": c(v.base),
            "warning.background": c(v.gold),
            "warning.foreground": c(v.base),
            "info.background": c(v.pine),
            "info.foreground": c(v.text),
            "list.background": c(v.surface),
            "list.hover.background": c(v.low),
            "list.active.background": a(v.iris, 0x26),
            "list.active.border": c(v.iris),
            "list.even.background": c(v.surface),
            "list.head.background": c(v.surface),
            "sidebar.background": c(v.surface),
            "sidebar.foreground": c(v.text),
            "sidebar.border": c(line),
            "sidebar.accent.background": c(raised),
            "sidebar.accent.foreground": c(v.text),
            "title_bar.background": c(v.surface),
            "title_bar.border": c(line),
            "scrollbar.background": "#00000000",
            "scrollbar.thumb.background": a(v.muted, 0x80),
            "scrollbar.thumb.hover.background": c(v.subtle),
            "tab.background": c(v.surface),
            "tab.active.background": c(v.base),
            "tab.foreground": c(v.subtle),
            "tab.active.foreground": c(v.text),
            "tab_bar.background": c(v.surface),
            "overlay": a(v.base, 0xb8),
        },
        "highlight": {
            "editor.foreground": c(v.text),
            "editor.background": c(v.base),
            "editor.active_line.background": a(v.muted, 0x1f),
            "editor.line_number": c(v.muted),
            "editor.active_line_number": c(v.subtle),
            "editor.invisible": a(v.muted, 0x66),
            "syntax": {
                "keyword": syntax(v.pine),
                "string": syntax(v.gold),
                "string.escape": syntax(v.pine),
                "number": syntax(v.rose),
                "boolean": syntax(v.rose),
                "comment": syntax(v.muted),
                "comment.doc": syntax(v.subtle),
                "function": syntax(v.rose),
                "constructor": syntax(v.foam),
                "type": syntax(v.foam),
                "constant": syntax(v.rose),
                "property": syntax(v.iris),
                "tag": syntax(v.foam),
                "attribute": syntax(v.iris),
                "operator": syntax(v.subtle),
                "punctuation": syntax(v.subtle),
                "variable": syntax(v.text),
                "title": syntax(v.rose),
                "link_text": syntax(v.foam),
                "link_uri": syntax(v.iris),
            },
        },
    })
}

/// Installs the Rosé Pine variant named by `PO_THEME` (`main`, `moon`, `dawn`); main by default.
pub fn apply(cx: &mut App) {
    let (variant, mode) = match std::env::var("PO_THEME").as_deref() {
        Ok("moon") => (&MOON, ThemeMode::Dark),
        Ok("dawn") => (&DAWN, ThemeMode::Light),
        _ => (&MAIN, ThemeMode::Dark),
    };
    let config: ThemeConfig = match serde_json::from_value(build(variant, mode)) {
        Ok(config) => config,
        Err(error) => return log::error!("theme: {error}"),
    };
    let config = Rc::new(config);
    Theme::update(cx, |theme| theme.apply_config(&config));
    Theme::change(mode, None, cx);
}
