use ely_gpui_component::theme::{Mode, Palette};
use gpui::{Hsla, rgb, rgba};

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
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

fn build(v: &Variant, mode: Mode) -> Palette {
    let dark = mode == Mode::Dark;
    let mut p = if dark { Palette::dark(false) } else { Palette::light(false) };
    p.bg = c(v.base);
    p.surface = c(v.surface);
    p.sunken = c(if dark { v.base } else { v.low });
    p.overlay = c(v.overlay);
    p.hover = c(v.low);
    p.active = c(v.med);
    p.border = c(if dark { v.overlay } else { v.med });
    p.border_strong = c(if dark { v.med } else { v.high });
    p.fg = c(v.text);
    p.fg_muted = c(v.subtle);
    p.fg_subtle = c(v.muted);
    p.fg_disabled = c(v.high);
    p.accent = c(v.iris);
    p.accent_hover = c(v.rose);
    p.on_accent = c(v.base);
    p.focus = c(v.high);
    p.link = c(v.foam);
    p.selection = (c(v.muted).opacity(0.4)).into();
    p.success = c(v.foam);
    p.warning = c(v.gold);
    p.danger = c(v.love);
    p.info = c(v.pine);
    p.success_subtle = c(v.foam).opacity(0.15);
    p.warning_subtle = c(v.gold).opacity(0.15);
    p.danger_subtle = c(v.love).opacity(0.15);
    p.info_subtle = c(v.pine).opacity(0.15);
    p.glass = rgba((v.base << 8) | 0xb8).into();
    p.tooltip_bg = c(v.med);
    p.tooltip_fg = c(v.text);
    p.chart = [v.foam, v.iris, v.gold, v.love, v.pine, v.rose, v.subtle, v.muted].map(c);
    p.ansi = [
        v.overlay, v.love, v.pine, v.gold, v.foam, v.iris, v.rose, v.text,
        v.muted, v.love, v.pine, v.gold, v.foam, v.iris, v.rose, v.text,
    ]
    .map(c);
    p.syntax.keyword = c(v.pine);
    p.syntax.string = c(v.gold);
    p.syntax.number = c(v.rose);
    p.syntax.comment = c(v.muted);
    p.syntax.function = c(v.rose);
    p.syntax.type_name = c(v.foam);
    p.syntax.constant = c(v.rose);
    p.syntax.property = c(v.iris);
    p.syntax.tag = c(v.foam);
    p.syntax.attribute = c(v.iris);
    p.syntax.operator = c(v.subtle);
    p.syntax.punctuation = c(v.subtle);
    p.syntax.variable = c(v.text);
    p
}

/// The Rosé Pine variant named by `PO_THEME` (`main`, `moon`, `dawn`); main by default.
pub fn pick() -> (Mode, Palette) {
    match std::env::var("PO_THEME").as_deref() {
        Ok("moon") => (Mode::Dark, build(&MOON, Mode::Dark)),
        Ok("dawn") => (Mode::Light, build(&DAWN, Mode::Light)),
        _ => (Mode::Dark, build(&MAIN, Mode::Dark)),
    }
}
