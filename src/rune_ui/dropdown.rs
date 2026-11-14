//! A menu drawn in the window, for the chips that open one. Rune's native pop-up menus exist only
//! on macOS and Windows; on Linux they answer "nothing picked" at once, so a chip would do
//! nothing. The chip opens this instead: what is open lives in the host's state, and
//! [`overlay`] mounts it over the window.
use rune::{El, Icon, icon, row, text, tok::TextStyle};
use rune_kit::{Kit, Menu, MenuItem};

use crate::model::bus::{Cmd, send};

/// One line of a menu: what it says, whether it is the current choice, and what picking it does
/// (a line without a command is a note, greyed).
#[derive(Clone)]
pub struct Line {
    pub label: String,
    pub checked: bool,
    act: Option<Act>,
}

/// A change to the host's own state, run when a line is picked.
type Call = std::rc::Rc<dyn Fn(&mut dyn std::any::Any)>;

/// What picking a line does: tell the Reader, or change the host's own state.
#[derive(Clone)]
enum Act {
    Send(Cmd),
    Call(Call),
}

impl Line {
    pub fn choice(label: impl Into<String>, checked: bool, cmd: Cmd) -> Self {
        Self {
            label: label.into(),
            checked,
            act: Some(Act::Send(cmd)),
        }
    }
    /// A line that changes the host's state `S` when picked.
    pub fn call<S: 'static>(
        label: impl Into<String>,
        checked: bool,
        f: impl Fn(&mut S) + 'static,
    ) -> Self {
        let f = std::rc::Rc::new(move |host: &mut dyn std::any::Any| {
            if let Some(host) = host.downcast_mut::<S>() {
                f(host);
            }
        });
        Self {
            label: label.into(),
            checked,
            act: Some(Act::Call(f)),
        }
    }
    pub fn note(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            checked: false,
            act: None,
        }
    }
}

/// The menu that is open: the key of the chip it hangs from, and its lines.
#[derive(Clone)]
pub struct Open {
    key: String,
    lines: Vec<Line>,
    /// A rule above the line at this index.
    rules: Vec<usize>,
}

#[derive(Default)]
pub struct State {
    open: Option<Open>,
}

/// Where the host keeps the state.
pub type At<S> = std::rc::Rc<dyn for<'a> Fn(&'a mut S) -> &'a mut State>;

pub static CHEVRON: Icon = Icon {
    name: "po-chevron-down",
    view: 16.0,
    body: r#"<path d="M4.5 6.5L8 10L11.5 6.5" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

fn small_style() -> TextStyle {
    crate::fonts::ui(12.0, 400.0)
}

/// A chip that opens `lines` below it (above, when there is no room). `key` names it; it must be
/// unique in the window.
pub fn chip<S: 'static>(
    k: &Kit,
    key: &str,
    label: String,
    lines: Vec<Line>,
    rules: Vec<usize>,
    at: At<S>,
) -> El {
    chip_inked::<S>(k, key, label, k.c.text_base, lines, rules, at)
}

/// [`chip`] with its label in `ink`: a chip that should stand out, such as one naming a risky setting.
pub fn chip_inked<S: 'static>(
    k: &Kit,
    key: &str,
    label: String,
    ink: rune::Color,
    lines: Vec<Line>,
    rules: Vec<usize>,
    at: At<S>,
) -> El {
    let face = row()
        .px(8.0)
        .py(2.0)
        .radius(6.0)
        .gap(2.0)
        .items_center()
        .child(text(label).font(small_style()).color(ink))
        .child(icon(&CHEVRON, 12.0).color(k.c.text_muted));
    opens(k, key, face, lines, rules, at)
}

/// Makes `face` open `lines` when pressed. `key` names it; it must be unique in the window.
pub fn opens<S: 'static>(
    k: &Kit,
    key: &str,
    face: El,
    lines: Vec<Line>,
    rules: Vec<usize>,
    at: At<S>,
) -> El {
    let open = Open {
        key: key.to_string(),
        lines,
        rules,
    };
    k.interactive(
        face.key(format!("{key}-trigger"))
            .on_press(move |s: &mut S| at(s).open = Some(open.clone())),
    )
}

/// The open menu, for the window's overlay layer.
pub fn overlay<S: 'static>(k: &Kit, state: &State, at: &At<S>, layer: &mut Vec<El>) {
    let Some(open) = &state.open else { return };
    let close = at.clone();
    let mut menu = Menu::new(open.key.clone())
        .open(true)
        .min_width(200.0)
        .placement(rune_kit::Placement::TopStart)
        .on_close(move |s: &mut S| close(s).open = None);
    for (i, line) in open.lines.iter().enumerate() {
        if open.rules.contains(&i) {
            menu = menu.separator();
        }
        let mut item = MenuItem::new(line.label.clone())
            .checked(line.checked)
            .disabled(line.act.is_none());
        if let Some(act) = line.act.clone() {
            let at = at.clone();
            item = item.on_select(move |s: &mut S| {
                at(s).open = None;
                match &act {
                    Act::Send(cmd) => send(cmd.clone()),
                    Act::Call(f) => f(s),
                }
            });
        }
        menu = menu.item(item);
    }
    menu.build(k, layer);
}
