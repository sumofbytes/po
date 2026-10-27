//! Buttons as gpui-kit drew them: the default, outline, ghost and primary variants at its
//! extra-small and small sizes, with an icon, a label, or both, an optional dropdown caret and
//! a tooltip.
use std::rc::Rc;

use gpui::{
    AnyView, App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Render, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, relative,
};

use super::icon::{Icon, IconName, IconSize};
use crate::theme::{ButtonStyle, Palette, tokens::RADIUS_FULL};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Default,
    Outline,
    Ghost,
    Primary,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Size {
    XSmall,
    Small,
}

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    variant: Variant,
    size: Size,
    selected: bool,
    disabled: bool,
    caret: bool,
    rounded: Option<Pixels>,
    tooltip: Option<SharedString>,
    on_click: Option<OnClick>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            variant: Variant::Default,
            size: Size::Small,
            selected: false,
            disabled: false,
            caret: false,
            rounded: None,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    pub fn outline(mut self) -> Self {
        self.variant = Variant::Outline;
        self
    }

    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    pub fn small(mut self) -> Self {
        self.size = Size::Small;
        self
    }

    pub fn xsmall(mut self) -> Self {
        self.size = Size::XSmall;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A chevron after the label, for a button that opens a menu.
    pub fn dropdown_caret(mut self, caret: bool) -> Self {
        self.caret = caret;
        self
    }

    /// Fully round, in place of the theme's corner radius.
    pub fn pill(mut self) -> Self {
        self.rounded = Some(RADIUS_FULL);
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

fn style(palette: &Palette, variant: Variant) -> ButtonStyle {
    match variant {
        Variant::Default => palette.buttons.default,
        Variant::Outline => palette.buttons.outline,
        Variant::Ghost => palette.buttons.ghost,
        Variant::Primary => palette.buttons.primary,
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        let style = style(palette, self.variant);
        let colors = if self.disabled {
            style.disabled
        } else if self.selected {
            style.selected
        } else {
            style.normal
        };
        let radius = self.rounded.unwrap_or(palette.radius);
        let icon_only = self.label.is_none();
        let bordered = matches!(self.variant, Variant::Default | Variant::Outline);
        let icon_size = match self.size {
            Size::XSmall => IconSize::XSmall,
            Size::Small => IconSize::Small,
        };
        let (hover, active) = (style.hover, style.active);
        let still = self.selected || self.disabled;
        let has_content = self.icon.is_some() || self.label.is_some();

        let content = div()
            .flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .items_center()
            .justify_center()
            .gap_1()
            .map(|this| match self.size {
                Size::XSmall => this.text_xs(),
                Size::Small => this.text_sm(),
            })
            .when_some(self.icon, |this, icon| this.child(Icon::new(icon).size(icon_size)))
            .when_some(self.label, |this, label| this.child(div().min_w_0().whitespace_nowrap().text_ellipsis().child(label)))
            .when(self.caret, |this| {
                this.when(has_content, |this| this.justify_between())
                    .child(Icon::new(IconName::ChevronDown).size(icon_size).color(colors.fg.opacity(0.75)))
            });

        div()
            .id(self.id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .cursor_default()
            .when(true, |this| match (icon_only, self.size) {
                (true, Size::XSmall) => this.size_5(),
                (true, Size::Small) => this.size_6(),
                (false, Size::XSmall) => this.h_5().px_1(),
                (false, Size::Small) => this.h_6().px_2(),
            })
            .rounded(radius)
            .when(bordered, |this| this.border_1())
            .border_color(colors.border)
            .bg(colors.bg)
            .text_color(colors.fg)
            .when(!still, |this| {
                this.hover(move |this| this.bg(hover.bg).border_color(hover.border).text_color(hover.fg))
                    .active(move |this| this.bg(active.bg).border_color(active.border).text_color(active.fg))
            })
            .line_height(relative(1.25))
            .child(content)
            // Clicking a button doesn't take focus from where it is.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.on_click.filter(|_| !self.disabled), |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
            .when_some(self.tooltip, |this, text| this.tooltip(move |_, cx| tooltip(text.clone(), cx)))
    }
}

/// A tooltip styled like the kit's: a small popover with the text.
pub fn tooltip(text: SharedString, cx: &mut App) -> AnyView {
    cx.new(|_| Tooltip(platform_keys(text))).into()
}

/// Shortcuts are written as Ctrl+Shift+X; on macOS the "secondary" modifier is Command, so show it so.
fn platform_keys(text: SharedString) -> SharedString {
    if cfg!(target_os = "macos") { text.replace("Ctrl+", "⌘").replace("Shift+", "⇧").into() } else { text }
}

struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        div().child(
            div()
                .flex()
                .font_family(palette.font_family.clone())
                .m_3()
                .bg(palette.popover)
                .text_color(palette.popover_foreground)
                .border_1()
                .border_color(palette.border)
                .shadow_md()
                .rounded(palette.radius)
                .py_0p5()
                .px_2()
                .text_sm()
                .child(self.0.clone()),
        )
    }
}

