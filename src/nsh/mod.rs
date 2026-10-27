//! Po's screens drawn with NSH: each runs as an NSH app in an [`AppView`], a GPUI view that sits in
//! Po's window like any other. This module is what they share: the theme and fonts they take from
//! Po's [`Palette`], Po's icons, and the views themselves.
pub mod todos;

use base64::Engine as _;
use gpui::{AppContext as _, Entity, Hsla, Subscription, Window};
use nsh_core::prelude::*;
use nsh_platform_gpui::{AdaptiveColor, App as NshApp, AppOptions, AppView, Appearance, Root};

use crate::theme::Palette;
use crate::ui::icon::IconName;

/// Once, before the first view: the UI sets the screens draw with.
pub fn init() {
    nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry()]);
}

/// A view running `root`, in Po's theme, and what keeps it in the theme as that changes: hold it
/// as long as the view.
pub fn view(root: Root, window: &mut Window, cx: &mut gpui::App) -> (Entity<AppView>, Subscription) {
    let view = cx.new(|cx| AppView::new(AppOptions::default(), window, cx));
    apply_theme(&view, cx);
    view.update(cx, |view, _| {
        view.on_error(|message| log::error!("nsh: {message}"));
        view.load(NshApp::Rust(root));
    });
    let follow = view.downgrade();
    let theme = cx.observe_global::<Palette>(move |cx| {
        if let Some(view) = follow.upgrade() {
            apply_theme(&view, cx);
        }
    });
    (view, theme)
}

fn apply_theme(view: &Entity<AppView>, cx: &mut gpui::App) {
    let palette = cx.global::<Palette>();
    let (theme, appearance) = (theme(palette), if palette.background.l < 0.5 { Appearance::Dark } else { Appearance::Light });
    view.update(cx, |view, cx| {
        view.set_theme(Some(theme), cx);
        view.set_appearance(appearance, cx);
    });
}

fn color(c: Hsla) -> AdaptiveColor {
    AdaptiveColor::from(nsh_platform_gpui::Hsla { h: c.h, s: c.s, l: c.l, a: c.a })
}

/// Po's palette as NSH's theme: shadcn/ui's tokens, each the palette color of its role, and the
/// roles shadcn has no token for under Po's own names ([`Colors`]).
fn theme(palette: &Palette) -> nsh_platform_gpui::Theme {
    let mut theme = nsh_platform_gpui::Theme::default();
    for (name, c) in [
        ("background", palette.background),
        ("foreground", palette.foreground),
        ("card", palette.popover),
        ("card-foreground", palette.popover_foreground),
        ("popover", palette.popover),
        ("popover-foreground", palette.popover_foreground),
        ("primary", palette.primary),
        ("secondary", palette.secondary),
        ("muted-foreground", palette.muted_foreground),
        ("accent", palette.accent),
        ("accent-foreground", palette.accent_foreground),
        ("destructive", palette.danger),
        ("border", palette.border),
        ("input", palette.input_border),
        ("ring", palette.primary),
        ("sidebar", palette.sidebar),
        ("warning", palette.warning),
        ("list-hover", palette.list_hover),
        ("input-background", palette.input_background),
        ("caret", palette.caret),
    ] {
        theme.set_color(name, color(c));
    }
    theme.set_number("radius", f32::from(palette.radius));
    theme.set_number("rem", f32::from(palette.font_size));
    theme.set_font(nsh_core::element::FONT_SANS, palette.font_family.to_string());
    theme.set_font(nsh_core::element::FONT_MONO, palette.mono_font_family.to_string());
    theme
}

/// Po's colors and sizes, as a screen reads them from the theme while it draws.
pub struct Colors;

impl Colors {
    fn token(name: &'static str) -> impl Fn() -> AdaptiveColor + Copy {
        move || nsh_base::theme().color(name).unwrap_or(AdaptiveColor::TRANSPARENT)
    }

    pub fn foreground() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().foreground
    }

    pub fn muted() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().muted_foreground
    }

    pub fn border() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().border
    }

    pub fn danger() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().destructive
    }

    pub fn popover() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().popover
    }

    pub fn warning() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("warning")
    }

    pub fn hover() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("list-hover")
    }

    pub fn input_background() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("input-background")
    }

    pub fn input_border() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().input
    }

    pub fn caret() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("caret")
    }

    /// `color` as one color: Po's theme gives each token the same in both appearances.
    pub fn one(color: impl Fn() -> AdaptiveColor + Copy) -> impl Fn() -> nsh_core::Hsla + Copy {
        move || color().light
    }

    pub fn radius() -> Points {
        nsh_base::theme().radius
    }
}

/// `n` rems, a rem being Po's UI font size, as GPUI's `rems` are in the rest of the window.
pub fn rems(n: f32) -> Points {
    Points(n * nsh_base::theme().number("rem").unwrap_or(14.))
}

/// Small text, GPUI's `text_xs`: dates and counts.
pub fn text_xs() -> Points {
    rems(0.75)
}

/// GPUI's `text_sm`: the footer and quiet notes.
pub fn text_sm() -> Points {
    rems(0.875)
}

/// One of Po's Lucide icons, tinted.
pub fn icon(name: IconName, size: Points, tint: impl Fn() -> AdaptiveColor + 'static) -> nsh_core::element::Image {
    let uri = format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(name.svg().1));
    image(uri).w(size).h(size).tint_color(tint).flex_none()
}

/// A tooltip in Po's look: a small popover with a line of text.
pub fn tooltip(label: &'static str) -> Text {
    text(label)
        .text_size(text_xs())
        .text_color(Colors::foreground())
        .bg(Colors::popover())
        .border_1()
        .border_color(Colors::border())
        .rounded(Colors::radius())
        .px(Points(8.))
        .py(Points(4.))
}
