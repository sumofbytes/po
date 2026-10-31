//! A pane's header: its title and what is said or offered on the right, with the window's own
//! controls in the panes at its edges (each button is one pane and never changes what it means).
use nsh_core::prelude::*;

use super::bus::{Cmd, send};
use super::{Colors, body, caption, header_height, icon, scale, tooltip};
use crate::ui::icon::IconName;

/// One of the window's controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    /// Shows or hides the sidebar; selected while it shows.
    Sidebar(bool),
    /// Shows or hides the viewer.
    Viewer(bool),
    /// Gives the viewer the room by hiding the conversation (or gives it back).
    Focus(bool),
    /// Back to the explorer, in a pane too narrow for it beside the file.
    Explorer,
}

impl Control {
    fn parts(self) -> (IconName, &'static str, bool, Cmd) {
        match self {
            Control::Sidebar(on) => (IconName::PanelLeft, "Sidebar (Ctrl+B)", on, Cmd::ToggleLeft),
            Control::Viewer(on) => (IconName::FileText, "Viewer (Ctrl+Shift+B)", on, Cmd::ToggleRight),
            Control::Focus(true) => (IconName::Minimize, "Show the conversation (Ctrl+Shift+F)", false, Cmd::ToggleFocus),
            Control::Focus(false) => (IconName::Maximize, "Focus the viewer (Ctrl+Shift+F)", false, Cmd::ToggleFocus),
            Control::Explorer => (IconName::FolderTree, "Back to the explorer", false, Cmd::Browse(true)),
        }
    }
}

/// A ghost icon button that can be selected, as the header's controls are.
pub fn toggle_button(name: IconName, tip: &'static str, selected: bool, on_press: impl Fn() + 'static) -> View {
    let b = super::smooth(view())
        .role(AccessibilityRole::Button)
        .label(tip)
        .flex_none()
        .p(scale::s1_5())
        .rounded(super::pill())
        .cursor_pointer()
        .tooltip(tooltip(tip))
        .on_press(on_press)
        .child(icon(name, scale::s3_5(), move || if selected { Colors::foreground()() } else { Colors::muted()() }));
    if selected { b.bg(Colors::selected()) } else { b.hover(|s| s.bg(Colors::hover())) }
}

fn controls(list: &[Control]) -> impl Iterator<Item = View> + '_ {
    list.iter().map(|control| {
        let (name, tip, selected, cmd) = control.parts();
        toggle_button(name, tip, selected, move || send(cmd.clone()))
    })
}

/// `title` with `lead` before it and `right` then `trail` after, on a bar with a line under it
/// (not on glass, where the headers' titles set the sections apart).
pub fn pane_header(title: impl IntoReactive<SharedString>, right: impl IntoElement, lead: &[Control], trail: &[Control], glass: bool) -> View {
    let bar = view()
        .h(header_height())
        .flex_none()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(scale::s3())
        .px(scale::s3())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s1())
                .flex_shrink(1.)
                .child(controls(lead).collect::<Vec<_>>())
                .child(body(title).number_of_lines(1).flex_shrink(1.).font_weight(FontWeight::MEDIUM)),
        )
        .child(view().flex_row().items_center().gap(scale::s2()).child(right).child(controls(trail).collect::<Vec<_>>()));
    if glass { bar } else { bar.border_b_1().border_color(super::hairline()) }
}

/// A quiet note in a header, such as the language of the file.
pub fn status(text_: impl IntoReactive<SharedString>) -> Text {
    caption(text_)
}

/// A small text button in a header.
pub fn header_button(label: &'static str, tip: &'static str, on_press: impl Fn() + 'static) -> View {
    super::smooth(view())
        .role(AccessibilityRole::Button)
        .label(tip)
        .flex_none()
        .px(scale::s3())
        .py(scale::s1())
        .rounded(super::pill())
        .cursor_pointer()
        .tooltip(tooltip(tip))
        .hover(|s| s.bg(Colors::hover()))
        .on_press(on_press)
        .child(body(label))
}
