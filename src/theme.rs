use std::path::{Path, PathBuf};

use crate::config::{self, read_json};
use crate::fonts::{self, Fonts};
use crate::schemes;

use std::collections::HashMap;

use gpui::{App, BoxShadow, FontStyle, FontWeight, Global, HighlightStyle, Hsla, Pixels, Rgba, SharedString, hsla, point, px};
use serde_json::Value;

const DEFAULT_THEME: &str = "Rose Pine";

/// Design tokens that don't vary by theme. Draw with these, not literals, so the UI stays uniform.
pub mod tokens {
    use gpui::{Pixels, Rems, px, rems};

    /// Height of a list row (sidebar projects and threads).
    pub const ROW_HEIGHT: Rems = rems(1.875);
    /// Radius that turns a square element into a circle or pill.
    pub const RADIUS_FULL: Pixels = px(999.);
    /// Radius of tiny marks, like the dot beside a file.
    pub const RADIUS_MARK: Pixels = px(2.);
    /// Alpha of a status tint behind a row (an added or removed diff line).
    pub const TINT: f32 = 0.16;
    /// Alpha of the faintest tint (the empty side of a split diff).
    pub const TINT_FAINT: f32 = 0.06;
    /// Alpha of a neutral tint (a diff hunk header).
    pub const TINT_SUBTLE: f32 = 0.12;
    /// Alpha of a muted or de-emphasised foreground.
    pub const MUTED: f32 = 0.6;
    /// Alpha of an indent guide line over the border colour.
    pub const GUIDE: f32 = 0.85;
    /// Alpha of dimmed content, like a model's reasoning.
    pub const DIM: f32 = 0.7;
    /// Alpha of a label that isn't the current one.
    pub const INACTIVE: f32 = 0.78;
}

/// Where themes are picked from: `<config>/themes/`.
fn themes_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("themes")
}

/// The theme `name` asks for: a JSON file in `<config>/themes/`, else in the
/// config folder itself — `.json` optional, absolute paths work — else one of
/// the bundled colour schemes.
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
    schemes::load(name.strip_suffix(".json").unwrap_or(name))
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
    let theme = load_theme(&name, dir)
        .or_else(|| {
            log::warn!("theme {name:?} not found, using {DEFAULT_THEME}");
            load_theme(DEFAULT_THEME, dir)
        })
        .expect("default scheme");
    let fonts = fonts::resolve(&theme, &config, cx);
    let glass = std::env::var("PO_GLASS").map(|value| glass_env(&value)).unwrap_or_else(|_| config["glass"].clone());
    let mut palette = Palette::new(&theme, fonts);
    if let Some(opacity) = glass_opacity(&glass) {
        palette.frost(opacity);
    }
    cx.set_global(palette);
}

/// A button's colors in one state.
#[derive(Clone, Copy)]
pub struct ButtonColors {
    pub bg: Hsla,
    pub border: Hsla,
    pub fg: Hsla,
}

/// A button variant's colors in each state, resolved as gpui-kit's buttons resolve them.
#[derive(Clone, Copy)]
pub struct ButtonStyle {
    pub normal: ButtonColors,
    pub hover: ButtonColors,
    pub active: ButtonColors,
    pub selected: ButtonColors,
    pub disabled: ButtonColors,
}

/// The button variants po uses.
pub struct Buttons {
    /// Filled, as the kit's default button.
    pub default: ButtonStyle,
    /// The default button outlined.
    pub outline: ButtonStyle,
    pub ghost: ButtonStyle,
    pub primary: ButtonStyle,
    /// Bare text or icon, no background or padding.
    pub text: ButtonStyle,
}

impl Buttons {
    fn new(c: &Base) -> Self {
        let col = |bg: Hsla, border: Hsla, fg: Hsla| ButtonColors { bg, border, fg };
        let t = c.transparent;
        let disabled_fg = c.muted_foreground.opacity(0.5);
        let default = ButtonStyle {
            normal: col(c.button, c.input, c.button_foreground),
            hover: col(c.button_hover, c.input, c.button_foreground),
            active: col(c.button_active, c.input, c.button_foreground),
            selected: col(c.button_active, c.input, c.button_foreground),
            disabled: col(c.input_background.opacity(0.5), c.input.opacity(0.5), disabled_fg),
        };
        let outline_active = mix_oklab(c.input, t, 0.7);
        let outline = ButtonStyle {
            normal: col(c.input_background, c.input, c.button_foreground),
            hover: col(mix_oklab(c.input, t, 0.5), c.input, c.button_foreground),
            active: col(outline_active, c.input, c.button_foreground),
            selected: col(outline_active, c.input, c.button_foreground),
            disabled: col(c.input_background.opacity(0.5), c.input.opacity(0.5), disabled_fg),
        };
        let ghost_hover = if c.dark { c.accent.opacity(0.5) } else { c.accent };
        let ghost = ButtonStyle {
            normal: col(t, t, c.secondary_foreground),
            hover: col(ghost_hover, t, c.accent_foreground),
            active: col(c.button_active, t, c.secondary_foreground),
            selected: col(c.secondary_active, t, c.secondary_foreground),
            disabled: col(t, t, disabled_fg),
        };
        let primary = ButtonStyle {
            normal: col(c.button_primary, c.primary, c.button_primary_foreground),
            hover: col(c.button_primary_hover, c.primary, c.button_primary_foreground),
            active: col(c.button_primary_active, c.primary, c.button_primary_foreground),
            selected: col(c.button_primary_active, c.primary, c.button_primary_foreground),
            disabled: col(c.button_primary.opacity(0.15), c.button_primary.opacity(0.15), disabled_fg),
        };
        let text = ButtonStyle {
            normal: col(t, t, c.foreground.opacity(0.9)),
            hover: col(t, t, c.foreground),
            active: col(t, t, c.foreground.opacity(0.7)),
            selected: col(t, t, c.foreground.opacity(0.7)),
            disabled: col(t, t, disabled_fg),
        };
        Self { default, outline, ghost, primary, text }
    }
}

/// A color as the themes write it: `#rrggbb` or `#rrggbbaa`.
fn parse_color(value: &Value) -> Option<Hsla> {
    Rgba::try_from(value.as_str()?).ok().map(Hsla::from)
}

/// Mixes two colors in the Oklab space, `factor` of the first, like CSS `color-mix(in oklab)`.
fn mix_oklab(a: Hsla, b: Hsla, factor: f32) -> Hsla {
    let factor = factor.clamp(0., 1.);
    let alpha = a.a * factor + b.a * (1. - factor);
    if alpha == 0. {
        return hsla(0., 0., 0., 0.);
    }
    let (la, lb) = (oklab(Rgba::from(a)), oklab(Rgba::from(b)));
    let mix = |x: f32, y: f32| (x * a.a * factor + y * b.a * (1. - factor)) / alpha;
    let mut rgb = from_oklab(mix(la.0, lb.0), mix(la.1, lb.1), mix(la.2, lb.2));
    rgb.a = alpha;
    Hsla::from(rgb)
}

fn oklab(rgb: Rgba) -> (f32, f32, f32) {
    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = (lin(rgb.r), lin(rgb.g), lin(rgb.b));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    (
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    )
}

fn from_oklab(l: f32, a: f32, b: f32) -> Rgba {
    let (l_, m_, s_) = (l + 0.396_337_78 * a + 0.215_803_76 * b, l - 0.105_561_346 * a - 0.063_854_17 * b, l - 0.089_484_18 * a - 1.291_485_5 * b);
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let (r, g, b) = (
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.0041960863 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    );
    let gamma = |c: f32| (if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 }).clamp(0., 1.);
    Rgba { r: gamma(r), g: gamma(g), b: gamma(b), a: 1. }
}

/// The same hue and saturation at a lower lightness.
fn darken(c: Hsla, amount: f32) -> Hsla {
    Hsla { l: c.l * (1. - amount.clamp(0., 1.)), ..c }
}

/// Every color the app draws with, resolved from a theme's `colors` the way gpui-kit did:
/// what the theme names, else a fallback derived from the colors already resolved.
struct Base {
    dark: bool,
    transparent: Hsla,
    background: Hsla,
    foreground: Hsla,
    border: Hsla,
    input: Hsla,
    input_background: Hsla,
    muted: Hsla,
    muted_foreground: Hsla,
    primary: Hsla,
    secondary: Hsla,
    secondary_foreground: Hsla,
    secondary_active: Hsla,
    accent: Hsla,
    accent_foreground: Hsla,
    button: Hsla,
    button_foreground: Hsla,
    button_hover: Hsla,
    button_active: Hsla,
    button_primary: Hsla,
    button_primary_foreground: Hsla,
    button_primary_hover: Hsla,
    button_primary_active: Hsla,
}

/// What a theme missing a color falls back to, by mode.
struct Defaults {
    background: &'static str,
    foreground: &'static str,
    border: &'static str,
    muted: &'static str,
    primary: &'static str,
    secondary: &'static str,
}

fn defaults(dark: bool) -> Defaults {
    if dark {
        Defaults { background: "#09090b", foreground: "#fafafa", border: "#27272a", muted: "#18181b", primary: "#fafafa", secondary: "#27272a" }
    } else {
        Defaults { background: "#ffffff", foreground: "#09090b", border: "#e4e4e7", muted: "#f4f4f5", primary: "#18181b", secondary: "#f4f4f5" }
    }
}

fn color(hex: &str) -> Hsla {
    Rgba::try_from(hex).map_or_else(|_| hsla(0., 0., 0., 1.), Hsla::from)
}

impl Base {
    fn new(colors: &Value, dark: bool) -> Self {
        let get = |key: &str, fallback: Hsla| colors.get(key).and_then(parse_color).unwrap_or(fallback);
        let d = defaults(dark);
        let transparent = hsla(0., 0., 0., 0.);
        let background = get("background", color(d.background));
        let foreground = get("foreground", color(d.foreground));
        let border = get("border", color(d.border));
        let input = get("input.border", border);
        let muted = get("muted.background", color(d.muted));
        let muted_foreground = get("muted.foreground", muted.blend(foreground.opacity(0.7)));
        let primary = get("primary.background", color(d.primary));
        let primary_foreground = get("primary.foreground", foreground);
        let secondary = get("secondary.background", color(d.secondary));
        let secondary_foreground = get("secondary.foreground", foreground);
        let active_darken = if dark { 0.2 } else { 0.1 };
        let secondary_active = get("secondary.active.background", darken(secondary, active_darken));
        let input_background = if dark { mix_oklab(input, transparent, 0.3) } else { background };
        let primary_hover = get("primary.hover.background", background.blend(primary.opacity(0.9)));
        let primary_active = get("primary.active.background", darken(primary, active_darken));
        let accent = get("accent.background", secondary);
        Self {
            dark,
            transparent,
            background,
            foreground,
            border,
            input,
            input_background,
            muted,
            muted_foreground,
            primary,
            secondary,
            secondary_foreground,
            secondary_active,
            accent,
            accent_foreground: get("accent.foreground", foreground),
            button: get("button.background", input_background),
            button_foreground: get("button.foreground", foreground),
            button_hover: get("button.hover.background", mix_oklab(input, transparent, 0.5)),
            button_active: get("button.active.background", mix_oklab(input, transparent, 0.7)),
            button_primary: get("button.primary.background", primary),
            button_primary_foreground: get("button.primary.foreground", primary_foreground),
            button_primary_hover: get("button.primary.hover.background", primary_hover),
            button_primary_active: get("button.primary.active.background", primary_active),
        }
    }
}

/// A font weight named in a theme: `bold`, `semibold`, or a number.
fn weight(value: &Value) -> Option<FontWeight> {
    if let Some(n) = value.as_f64() {
        return Some(FontWeight(n as f32));
    }
    let name = value.as_str()?.to_lowercase().replace(['_', '-'], "");
    Some(FontWeight(match name.as_str() {
        "thin" => 100.,
        "extralight" => 200.,
        "light" => 300.,
        "normal" => 400.,
        "medium" => 500.,
        "semibold" => 600.,
        "bold" => 700.,
        "extrabold" => 800.,
        "black" => 900.,
        _ => return None,
    }))
}

/// Reapplies the theme and fonts whenever a `.json` file in the config folder or its `themes`
/// folder changes, so edits show without a restart.
pub fn watch(cx: &mut App) {
    use futures::StreamExt;
    use notify::Watcher;

    let Some(dir) = config::dir() else { return };
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !event.kind.is_access()
            && event.paths.iter().any(|path| path.extension().is_some_and(|ext| ext == "json"))
        {
            tx.unbounded_send(()).ok();
        }
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            log::error!("watch config: {error}");
            return;
        }
    };
    // The themes folder may not exist yet; watching the config folder recursively covers it appearing.
    if let Err(error) = watcher.watch(&dir, notify::RecursiveMode::Recursive) {
        log::warn!("watch {}: {error}", dir.display());
        return;
    }
    cx.spawn(async move |cx| {
        // Moved in so the watcher lives as long as this task.
        let _watcher = watcher;
        while rx.next().await.is_some() {
            // Editors save in several steps; let them finish.
            cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
            while rx.try_recv().is_ok() {}
            cx.update(|cx| {
                apply(cx);
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

/// How opaque glass panes are by default: enough to keep text crisp over a busy desktop.
const GLASS_OPACITY: f32 = 0.72;

/// The `glass` setting: `true` for the default opacity, or a number from 0 to 1 for the panes'.
/// `PO_GLASS` as the `glass` setting: off for empty, `0`, `false`, `off` or `no`, on for `1`,
/// `true`, `on` or `yes`, a number as the panes' opacity, anything else on.
fn glass_env(value: &str) -> Value {
    match value.trim().to_lowercase().as_str() {
        "" | "0" | "false" | "off" | "no" => Value::Bool(false),
        "1" | "true" | "on" | "yes" => Value::Bool(true),
        value => value.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::Bool(true), Value::Number),
    }
}

fn glass_opacity(value: &Value) -> Option<f32> {
    match value {
        Value::Bool(true) => Some(GLASS_OPACITY),
        Value::Number(n) => n.as_f64().map(|n| (n as f32).clamp(0.2, 1.)),
        _ => None,
    }
}

/// The frosted look of macOS's Liquid Glass: the window blurs what's behind it, panes are
/// translucent tints over that blur, and floating panes catch light along their rim.
#[derive(Clone, Copy)]
pub struct Glass {
    /// The bright hairline around a floating pane.
    pub rim: Hsla,
    /// The light along a floating pane's top edge, drawn inside it.
    pub sheen: Hsla,
    /// The gap between a floating pane and the window's edges.
    pub inset: Pixels,
}

impl Glass {
    /// A floating pane's sheen, inside its top edge. No drop shadow: gpui fills a shadow under
    /// the whole pane, so it would show through the glass as a grey cast.
    pub fn shadow(&self) -> Vec<BoxShadow> {
        vec![BoxShadow { color: self.sheen, offset: point(px(0.), px(1.)), blur_radius: px(0.), spread_radius: px(0.), inset: true }]
    }
}

/// The colors markdown is drawn with, for the agent's replies and for `.md` files alike. Each
/// comes from the theme's `markdown` object (`heading`, `code`, `code_background`,
/// `inline_code_background`, `quote`, `quote_border`, `rule`, `marker`, `strong`, `emphasis`)
/// if it names one, else from the syntax colors the theme already has, else a neutral one.
#[derive(Clone, Copy)]
pub struct MarkdownColors {
    pub heading: Hsla,
    /// Inline code's text.
    pub code: Hsla,
    pub code_background: Hsla,
    pub inline_code_background: Hsla,
    pub quote: Hsla,
    pub quote_border: Hsla,
    pub rule: Hsla,
    /// List bullets and numbers.
    pub marker: Hsla,
    pub strong: Option<Hsla>,
    pub emphasis: Option<Hsla>,
}

/// The colors and fonts po's own components draw with, taken from the applied theme. Kept
/// apart from the kit's theme so those components don't depend on it.
pub struct Palette {
    pub buttons: Buttons,
    /// What the window itself is painted with, under every pane: the background, or with glass
    /// a faint tint over the blurred desktop.
    pub window: Hsla,
    /// Set when the glass look is on.
    pub glass: Option<Glass>,
    pub background: Hsla,
    pub foreground: Hsla,
    pub primary: Hsla,
    pub secondary: Hsla,
    pub sidebar: Hsla,
    pub success: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    pub warning: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    /// The hairline around popovers and menus.
    pub popover_ring: Hsla,
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub muted_foreground: Hsla,
    pub link: Hsla,
    pub markdown: MarkdownColors,
    pub table_head: Hsla,
    pub table_head_foreground: Hsla,
    pub input_background: Hsla,
    pub input_border: Hsla,
    /// A list row under the pointer.
    pub list_hover: Hsla,
    /// The selected list row.
    pub list_selected: Hsla,
    pub resize_handle: Hsla,
    pub resize_handle_active: Hsla,
    pub scrollbar_track: Hsla,
    pub scrollbar_thumb: Hsla,
    pub scrollbar_thumb_hover: Hsla,
    pub border: Hsla,
    pub selection: Hsla,
    pub caret: Hsla,
    pub editor_background: Hsla,
    pub editor_foreground: Hsla,
    pub editor_active_line: Option<Hsla>,
    pub editor_line_number: Hsla,
    /// The corner radius of small parts, like scrollbar thumbs.
    pub radius: Pixels,
    /// Larger parts, like cards and sidebar groups: a third more than `radius`.
    pub radius_lg: Pixels,
    /// The largest: chat bubbles and the composer, twice `radius`.
    pub radius_xl: Pixels,
    /// Green for added and unread. Themes may lack one (Rosé Pine's success is teal), so it is fixed.
    pub added: Hsla,
    pub font_family: SharedString,
    pub font_size: Pixels,
    pub mono_font_family: SharedString,
    pub mono_font_size: Pixels,
    syntax: HashMap<String, HighlightStyle>,
}

impl Global for Palette {}

impl Palette {
    /// Resolves the colors, fonts and syntax styles of the theme `theme` (its JSON).
    pub fn new(theme: &Value, fonts: Fonts) -> Self {
        let dark = theme["mode"].as_str() != Some("light");
        let colors = &theme["colors"];
        let base = Base::new(colors, dark);
        let get = |key: &str, fallback: Hsla| colors.get(key).and_then(parse_color).unwrap_or(fallback);
        let clamp_alpha = |c: Hsla, max: f32| c.alpha(c.a.min(max));

        let radius = px(theme["radius"].as_f64().unwrap_or(6.) as f32);
        let accent = base.accent;
        let list_head = get("list.head.background", get("list.background", base.background));
        let scrollbar_thumb = get("scrollbar.thumb.background", accent);
        let highlight = &theme["highlight"];
        let editor = |key: &str| highlight.get(key).and_then(parse_color);
        let syntax = highlight["syntax"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(name, style)| {
                        let style = HighlightStyle {
                            color: style.get("color").and_then(parse_color),
                            font_weight: style.get("font_weight").and_then(weight),
                            font_style: match style.get("font_style").and_then(Value::as_str) {
                                Some("italic") => Some(FontStyle::Italic),
                                Some("normal") => Some(FontStyle::Normal),
                                _ => None,
                            },
                            ..Default::default()
                        };
                        (name.clone(), style)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let primary = base.primary;
        let syntax_color = |name: &str| -> Option<Hsla> {
            let styles: &HashMap<String, HighlightStyle> = &syntax;
            styles.get(name).or_else(|| styles.get(name.split('.').next()?))?.color
        };
        let exact_syntax_color = |name: &str| -> Option<Hsla> {
            let styles: &HashMap<String, HighlightStyle> = &syntax;
            styles.get(name)?.color
        };
        let md = &theme["markdown"];
        let md_get = |key: &str, fallback: Hsla| md.get(key).and_then(parse_color).unwrap_or(fallback);
        let markdown = MarkdownColors {
            heading: md_get("heading", syntax_color("title").unwrap_or(base.foreground)),
            code: md_get("code", syntax_color("string").unwrap_or(base.foreground)),
            code_background: md_get("code_background", base.muted),
            inline_code_background: md_get("inline_code_background", accent),
            quote: md_get("quote", base.muted_foreground),
            quote_border: md_get("quote_border", syntax_color("punctuation").unwrap_or(base.border)),
            rule: md_get("rule", base.border),
            marker: md_get("marker", syntax_color("punctuation").unwrap_or(base.muted_foreground)),
            strong: md.get("strong").and_then(parse_color).or_else(|| exact_syntax_color("emphasis.strong")),
            emphasis: md.get("emphasis").and_then(parse_color).or_else(|| syntax_color("emphasis")),
        };

        Self {
            buttons: Buttons::new(&base),
            window: base.background,
            glass: None,
            background: base.background,
            foreground: base.foreground,
            primary,
            secondary: base.secondary,
            sidebar: get("sidebar.background", base.background.blend(base.border.opacity(0.15))),
            success: get("success.background", color("#22c55e")),
            danger: get("danger.background", color("#ef4444")),
            info: get("info.background", color("#06b6d4")),
            warning: get("warning.background", color("#eab308")),
            popover: get("popover.background", base.background),
            popover_foreground: get("popover.foreground", base.foreground),
            popover_ring: base.foreground.alpha(0.1),
            accent,
            accent_foreground: base.accent_foreground,
            muted_foreground: base.muted_foreground,
            link: get("link", primary),
            markdown,
            table_head: get("table.head.background", list_head),
            table_head_foreground: get("table.head.foreground", base.muted_foreground),
            input_background: base.input_background,
            input_border: base.input,
            list_hover: get("list.hover.background", accent.opacity(0.6)),
            list_selected: clamp_alpha(get("list.active.background", base.background.blend(primary.opacity(0.1))), 0.2),
            resize_handle: base.border,
            resize_handle_active: get("drag.border", primary.opacity(0.65)),
            scrollbar_track: get("scrollbar.background", base.background),
            scrollbar_thumb,
            scrollbar_thumb_hover: get("scrollbar.thumb.hover.background", scrollbar_thumb),
            border: base.border,
            selection: clamp_alpha(get("selection.background", primary), 0.3),
            caret: get("caret", primary),
            editor_background: editor("editor.background").unwrap_or(base.background),
            editor_foreground: editor("editor.foreground").unwrap_or(base.foreground),
            editor_active_line: editor("editor.active_line.background"),
            editor_line_number: editor("editor.line_number").unwrap_or(base.muted_foreground),
            radius,
            radius_lg: radius * (4. / 3.),
            radius_xl: radius * 2.,
            added: hsla(0.37, 0.5, 0.5, 1.0),
            font_family: fonts.ui,
            font_size: px(fonts.ui_size),
            mono_font_family: fonts.mono,
            mono_font_size: px(fonts.mono_size),
            syntax,
        }
    }

    /// Turns on the glass look: the panes become `opacity` tints over the blurred desktop, the
    /// sidebar alike, and the lines between them fade so the panes read as
    /// sheets of glass rather than boxes.
    pub fn frost(&mut self, opacity: f32) {
        let dark = self.background.l < 0.5;
        let clear = hsla(0., 0., 0., 0.);
        self.window = self.background.opacity(opacity * 0.35);
        self.background = self.background.opacity(opacity);
        // The editor draws inside a pane that's already tinted; another layer would thicken it.
        self.editor_background = clear;
        self.sidebar = self.sidebar.opacity(opacity);
        // Menus float over text and nothing blurs it, so they stay nearly solid.
        self.popover = self.popover.opacity(opacity.max(0.94));
        self.secondary = self.secondary.opacity(opacity * 0.85);
        self.scrollbar_track = clear;
        self.border = self.border.opacity(0.6);
        self.resize_handle = clear;
        let (rim, sheen) = if dark { (0.14, 0.1) } else { (0.55, 0.7) };
        let glass = Glass {
            rim: hsla(0., 0., 1., rim),
            sheen: hsla(0., 0., 1., sheen),
            inset: px(8.),
        };
        // A white rim would vanish around a light menu; light themes keep their dark hairline.
        if dark {
            self.popover_ring = glass.rim;
        }
        self.glass = Some(glass);
    }

    /// The style for a tree-sitter capture: its own, else its first part's, as `keyword.modifier`
    /// falls back to `keyword`.
    pub fn syntax(&self, name: &str) -> Option<HighlightStyle> {
        self.syntax.get(name).or_else(|| self.syntax.get(name.split('.').next()?)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn themes_folder_overrides_schemes() {
        let scratch = Scratch::new("override");
        scratch.write("themes/Dracula.json", r#"{"mode":"light"}"#);
        assert_eq!(load_theme("Dracula", Some(scratch.0.as_path())).unwrap()["mode"], "light");
        assert_eq!(load_theme("Dracula", None).unwrap()["mode"], "dark");
    }

    #[test]
    fn picks_schemes_by_name() {
        let theme = load_theme("Catppuccin Mocha", None).unwrap();
        assert_eq!(theme["colors"]["background"], "#1e1e2e");
        assert_eq!(load_theme("catppuccin mocha.json", None).unwrap()["colors"], theme["colors"]);
        assert_eq!(load_theme("Catppuccin Latte", None).unwrap()["mode"], "light");
        assert!(load_theme("no-such-theme", None).is_none());
    }

    #[test]
    fn glass_is_opt_in() {
        use serde_json::json;
        assert_eq!(glass_opacity(&Value::Null), None);
        assert_eq!(glass_opacity(&json!(false)), None);
        assert_eq!(glass_opacity(&json!(true)), Some(GLASS_OPACITY));
        assert_eq!(glass_opacity(&json!(0.5)), Some(0.5));
        // Too clear to read text on is held back.
        assert_eq!(glass_opacity(&json!(0)), Some(0.2));
        for off in ["", "0", "false", "OFF", "no"] {
            assert_eq!(glass_opacity(&glass_env(off)), None, "{off:?}");
        }
        assert_eq!(glass_opacity(&glass_env("1")), Some(GLASS_OPACITY));
        assert_eq!(glass_opacity(&glass_env("true")), Some(GLASS_OPACITY));
        assert_eq!(glass_opacity(&glass_env("0.4")), Some(0.4));
    }

    #[test]
    fn default_theme_exists() {
        assert!(load_theme(DEFAULT_THEME, None).is_some());
    }
}
