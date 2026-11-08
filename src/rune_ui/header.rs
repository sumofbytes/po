//! The window's own controls, which sit in the headers of the panes at its edges. Counterpart of
//! the control half of `src/nsh/header.rs`.
use rune::{El, Icon, row};
use rune_kit::{IconButton, IconButtonVariant, Kit};

use crate::model::bus::{Cmd, send};
use crate::model::header::Control;

/// The height of the window's top band, as the old pane headers had it (2.75 rem).
pub const BAND: f32 = 44.0;

/// What the kit's own band, the toolbar's and the sidebar's, is tall. The shell hangs the kit's
/// layout this much above the window, so that the part of it that shows is [`BAND`] tall.
pub const KIT_BAND: f32 = rune_kit::shell::TOOLBAR_HEIGHT;

static PANEL_LEFT: Icon = Icon {
    name: "po-panel-left",
    view: 16.0,
    body: r#"<rect x="2" y="3" width="12" height="10" rx="1.5" stroke="currentColor"/><path d="M6 3V13" stroke="currentColor"/>"#,
};
static PANEL_RIGHT: Icon = Icon {
    name: "po-panel-right",
    view: 16.0,
    body: r#"<rect x="2" y="3" width="12" height="10" rx="1.5" stroke="currentColor"/><path d="M10 3V13" stroke="currentColor"/>"#,
};
static FOLDER_TREE: Icon = Icon {
    name: "po-folder-tree",
    view: 16.0,
    body: r#"<path d="M3 3V12.5H13M3 7H8M3 12.5H8" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
static DIFF: Icon = Icon {
    name: "po-diff",
    view: 16.0,
    body: r#"<path d="M5.5 2.5V8.5M2.5 5.5H8.5M10 10.5H14" stroke="currentColor" stroke-linecap="round"/><path d="M4 13.5L12 2.5" stroke="currentColor" stroke-linecap="round" opacity="0.5"/>"#,
};
static TERMINAL: Icon = Icon {
    name: "po-terminal",
    view: 16.0,
    body: r#"<rect x="2" y="3" width="12" height="10" rx="1.5" stroke="currentColor"/><path d="M5 6.5L7 8.5L5 10.5M8.5 10.5H11" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
static MAXIMIZE: Icon = Icon {
    name: "po-maximize",
    view: 16.0,
    body: r#"<path d="M3 6V3H6M10 3H13V6M13 10V13H10M6 13H3V10" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
static MINIMIZE: Icon = Icon {
    name: "po-minimize",
    view: 16.0,
    body: r#"<path d="M6 3V6H3M13 6H10V3M10 13V10H13M3 10H6V13" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

/// What a control looks like, says, and does: the icon, its tooltip, whether it is on, and the
/// command it sends.
fn of(control: Control) -> (&'static Icon, &'static str, bool, Cmd) {
    match control {
        Control::Sidebar(on) => (&PANEL_LEFT, "Sidebar (Ctrl+B)", on, Cmd::ToggleLeft),
        Control::Viewer(on) => (&PANEL_RIGHT, "Viewer (Ctrl+Shift+B)", on, Cmd::ToggleRight),
        Control::ViewerLeft(on) => (&PANEL_LEFT, "Viewer (Ctrl+Shift+B)", on, Cmd::ToggleRight),
        Control::Files(on) => (&FOLDER_TREE, "Files (Ctrl+1)", on, Cmd::Show(false)),
        Control::Changes(on) => (&DIFF, "Changes (Ctrl+2)", on, Cmd::Show(true)),
        Control::Terminal(on) => (&TERMINAL, "Terminal (Ctrl+`)", on, Cmd::Terminal),
        Control::Focus(true) => (&MINIMIZE, "Show the conversation (Ctrl+Shift+F)", false, Cmd::ToggleFocus),
        Control::Focus(false) => (&MAXIMIZE, "Focus the viewer (Ctrl+Shift+F)", false, Cmd::ToggleFocus),
    }
}

/// A pane's header bar with its bottom border drawn inside its height, so that every header
/// (and the window's band, which has the same height) is ruled the same.
pub fn ruled(k: &Kit, bar: El) -> El {
    bar.child(rune::el().absolute(0.0, BAND - 1.0).w_full().h(1.0).fill(k.c.border_base))
}

/// A row of the controls given, in the order given but for the places (the files, the terminal,
/// the changes), which go first in two pills with a hairline between: where the user works, then
/// what the thread made. Empty when there are none.
pub fn controls<S: 'static>(k: &Kit, which: &[Control]) -> El {
    let button = |i: usize, control: Control, small: bool| {
        let (icon, help, on, cmd) = of(control);
        let variant = if on { IconButtonVariant::Neutral } else { IconButtonVariant::Ghost };
        let b = IconButton::new(icon).variant(variant).label(help).help(help).on_press(move |_: &mut S| send(cmd.clone()));
        let b = if small { b.control_size(rune_kit::ControlSize::Small) } else { b };
        b.build(k).key(format!("control-{i}-{help}"))
    };
    let place = |c: &Control| matches!(c, Control::Files(_) | Control::Terminal(_) | Control::Changes(_));
    let pill = |members: Vec<El>| {
        let mut pill = row().items_center().gap(2.0).padding(2.0).radius(8.0).border(1.0, k.c.border_base);
        for member in members {
            pill = pill.child(member);
        }
        pill
    };
    let mut bar = row().items_center().gap(2.0);
    let work: Vec<_> = which.iter().enumerate().filter(|(_, c)| matches!(c, Control::Files(_) | Control::Terminal(_))).map(|(i, c)| button(i, *c, true)).collect();
    let record: Vec<_> = which.iter().enumerate().filter(|(_, c)| matches!(c, Control::Changes(_))).map(|(i, c)| button(i, *c, true)).collect();
    let both = !work.is_empty() && !record.is_empty();
    if !work.is_empty() {
        bar = bar.child(pill(work));
    }
    if both {
        bar = bar.child(row().w(1.0).h(16.0).fill(k.c.border_base));
    }
    if !record.is_empty() {
        bar = bar.child(pill(record));
    }
    for (i, control) in which.iter().enumerate().filter(|(_, c)| !place(c)) {
        bar = bar.child(button(i, *control, false));
    }
    bar
}
