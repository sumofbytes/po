//! Po's screens drawn with NSH: each runs as an NSH app in an [`AppView`], a GPUI view that sits in
//! Po's window like any other. This module is what they share: the theme and fonts they take from
//! Po's [`Palette`], Po's icons, and the views themselves.
pub mod agent;
pub mod chat;
pub mod bus;
pub mod diff;
pub mod epics;
pub mod explorer;
pub mod finder;
pub mod header;
pub mod issues;
pub mod overlay;
pub mod settings;
pub mod source;
pub mod sidebar;
pub mod strip;
pub mod tabs;

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
pub fn mount(root: Root, window: &mut Window, cx: &mut gpui::App) -> (Entity<AppView>, Subscription) {
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
        ("list-selected", palette.list_selected),
        ("added", palette.added),
        ("info", palette.info),
        ("input-background", palette.input_background),
        ("caret", palette.caret),
    ] {
        theme.set_color(name, color(c));
    }
    theme.set_number("radius", f32::from(palette.radius));
    theme.set_number("rem", f32::from(palette.font_size));
    theme.set_number("mono-size", f32::from(palette.mono_font_size));
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

    pub fn background() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().background
    }

    pub fn popover() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().popover
    }

    pub fn warning() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("warning")
    }

    pub fn selected() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("list-selected")
    }

    pub fn added() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("added")
    }

    pub fn info() -> impl Fn() -> AdaptiveColor + Copy {
        Self::token("info")
    }

    pub fn primary() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().primary
    }

    pub fn primary_foreground() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().primary_foreground
    }

    pub fn sidebar() -> impl Fn() -> AdaptiveColor + Copy {
        || nsh_base::theme().sidebar
    }

    /// `color` faded to Po's dimmed-text strength ([`crate::theme::tokens::DIM`]).
    pub fn dim(color: impl Fn() -> AdaptiveColor + Copy) -> impl Fn() -> AdaptiveColor + Copy {
        move || color().opacity(crate::theme::tokens::DIM)
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

/// `n` rems, a rem being Po's UI font size, as GPUI's `rems` are in the rest of the window. Only
/// [`scale`] calls it: a screen takes its lengths from there, so spacing stays on one scale.
fn rems(n: f32) -> Points {
    Points(n * nsh_base::theme().number("rem").unwrap_or(14.))
}

/// The lengths of the screens: spacing, padding, gaps and sizes in rems. Each step `sN` is N quarter
/// rems (`s1_5` is one and a half), a 2px grid at the default font size; use these, not `rems`, so
/// every screen spaces itself the same way. The named sizes below are one element's own measure.
pub mod scale {
    use super::{Points, rems};

    pub fn s0() -> Points { rems(0.0) }
    pub fn s0_5() -> Points { rems(0.125) }
    pub fn s1() -> Points { rems(0.25) }
    pub fn s1_5() -> Points { rems(0.375) }
    pub fn s2() -> Points { rems(0.5) }
    pub fn s2_5() -> Points { rems(0.625) }
    pub fn s3() -> Points { rems(0.75) }
    pub fn s3_5() -> Points { rems(0.875) }
    pub fn s4() -> Points { rems(1.0) }
    pub fn s5() -> Points { rems(1.25) }
    pub fn s6() -> Points { rems(1.5) }
    pub fn s7() -> Points { rems(1.75) }
    pub fn s8() -> Points { rems(2.0) }
    pub fn s10() -> Points { rems(2.5) }

    /// The label column of a legend row: Status, Target, Progress, and an issue's fields.
    pub fn legend_width() -> Points { rems(5.5) }
    /// The label column of an agent's form, whose labels are the agent's own words and run longer.
    pub fn form_label_width() -> Points { rems(9.0) }
    /// A row's progress bar, in the epics list and in an epic's milestones.
    pub fn bar_width() -> Points { rems(8.0) }
    /// A row's count column, as `3/5`.
    pub fn count_width() -> Points { rems(4.0) }
    /// A row's due-date column.
    pub fn due_width() -> Points { rems(4.5) }
    /// The due-date entry beside a new issue or milestone: its field and the date it reads.
    pub fn due_entry_width() -> Points { rems(13.0) }
    /// From the composer card's edge to its text: the card's padding and the field's, and the
    /// border, which is a point wide whatever the font size.
    pub fn composer_text_inset() -> Points { s2() + s2() + Points(1.) }
    /// `n` lines of small text, whose line is 1.25rem tall: a text area's minimum, a note's maximum.
    pub fn lines(n: f32) -> Points { rems(1.25 * n) }
    /// The least width of a project's name in the sidebar.
    pub fn project_name_min_width() -> Points { rems(6.0) }
    /// The least width of a project's branch in the sidebar.
    pub fn branch_name_min_width() -> Points { rems(4.0) }
}

/// The monospace font's size, in points.
pub fn mono_size() -> f32 {
    nsh_base::theme().number("mono-size").unwrap_or(13.)
}

/// The text of a row's label, GPUI's `text_base`.
pub fn text_base() -> Points {
    scale::s4()
}

/// A field's height.
pub fn field_height() -> Points {
    scale::s8()
}

/// A pane header's height, GPUI's `h_11`.
pub fn header_height() -> Points {
    rems(2.75)
}

/// Small text, GPUI's `text_xs`: dates and counts.
pub fn text_xs() -> Points {
    scale::s3()
}

/// GPUI's `text_sm`: the footer and quiet notes.
pub fn text_sm() -> Points {
    scale::s3_5()
}

/// Quiet small text: dates, counts, secondary labels.
pub fn caption(content: impl IntoReactive<SharedString>) -> Text {
    text(content).text_size(text_xs()).text_color(Colors::muted())
}

/// Small text in the foreground color: short labels and marks.
pub fn small(content: impl IntoReactive<SharedString>) -> Text {
    text(content).text_size(text_xs()).text_color(Colors::foreground())
}

/// Quiet text: notes, empty states, hints.
pub fn note(content: impl IntoReactive<SharedString>) -> Text {
    text(content).text_size(text_sm()).text_color(Colors::muted())
}

/// Text of a row or control, in the foreground color.
pub fn body(content: impl IntoReactive<SharedString>) -> Text {
    text(content).text_size(text_sm()).text_color(Colors::foreground())
}

/// A radius that makes a pill or dot of anything square.
pub fn pill() -> Points {
    Points(999.)
}

/// The corner of a card or panel, which holds controls of the base radius: twice that radius.
pub fn card_radius() -> Points {
    Colors::radius() * 2.
}

/// The corner of the big surfaces of the chat column, the composer, the user's messages and the
/// queue: three times the base radius.
pub fn surface_radius() -> Points {
    Colors::radius() * 3.
}

/// The small radius of a swatch or marker.
pub fn swatch_radius() -> Points {
    Points(2.)
}

/// One of Po's Lucide icons, tinted.
pub fn icon(name: IconName, size: Points, tint: impl Fn() -> AdaptiveColor + 'static) -> nsh_core::element::Image {
    let uri = format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(name.svg().1));
    image(uri).w(size).h(size).tint_color(tint).flex_none()
}

/// A tooltip in Po's look: a small popover with a line of text.
pub fn tooltip(label: &'static str) -> Text {
    small(label)
        .bg(Colors::popover())
        .border_1()
        .border_color(Colors::border())
        .rounded(Colors::radius())
        .px(Points(8.))
        .py(Points(4.))
}

/// A row's control that only shows while the pointer is on the row.
pub fn on_hover(control: View) -> View {
    control.opacity(0.).group_hover(|s| s.opacity(1.))
}

/// A small ghost button with an icon.
pub fn icon_button(name: IconName, size: Points, label: &'static str, on_press: impl Fn() + 'static) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(label)
        .flex_none()
        .p(scale::s1())
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(tooltip(label))
        .on_press(on_press)
        .child(icon(name, size, Colors::muted()))
}

/// A text button: filled with the primary color, or an outline.
pub fn button(label: &'static str, primary: bool, on_press: impl Fn() + 'static) -> View {
    let b = view()
        .role(AccessibilityRole::Button)
        .label(label)
        .flex_none()
        .px(scale::s3())
        .py(scale::s1_5())
        .rounded(Colors::radius())
        .cursor_pointer()
        .on_press(on_press)
        .child(text(label).text_size(text_sm()).text_color(move || if primary { Colors::primary_foreground()() } else { Colors::foreground()() }));
    if primary { b.bg(Colors::primary()) } else { b.border_1().border_color(Colors::input_border()).hover(|s| s.bg(Colors::hover())) }
}

/// A single line of text for `value`, filling its row; Enter calls `on_submit`. `id` names it for tests.
pub fn text_input(id: &str, value: Signal<String>, placeholder: &'static str, on_submit: impl Fn() + 'static) -> nsh_base::Input {
    nsh_core::element::input()
        .test_id(id)
        .value(move || value.get())
        .placeholder(placeholder)
        .placeholder_color(Colors::one(Colors::muted()))
        .selection_color(Colors::one(Colors::caret()))
        .text_color(Colors::foreground())
        .auto_correct(false)
        .submit_behavior(nsh_core::element::SubmitBehavior::Submit)
        .on_change(move |text: &str| value.set(text.to_string()))
        .on_submit(move |_: &str| on_submit())
        .flex_1()
}

/// A line of text in a bordered box, as the settings page has them. `id` names it for tests.
pub fn field(id: &str, value: Signal<String>, placeholder: &'static str, on_submit: impl Fn() + 'static) -> View {
    field_with(id, value, placeholder, on_submit, |input| input)
}

/// [`field`], with `more` done to its input (a ref, focus).
pub fn field_with(id: &str, value: Signal<String>, placeholder: &'static str, on_submit: impl Fn() + 'static, more: impl FnOnce(nsh_base::Input) -> nsh_base::Input) -> View {
    let input = more(text_input(id, value, placeholder, on_submit));
    view()
        .h(field_height())
        .flex_row()
        .items_center()
        .px(scale::s2_5())
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .child(input)
}
