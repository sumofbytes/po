//! The file tree beside the viewer: every file, or the changed ones in Changes mode, under a
//! header with the mode switch and the PR and worktree strips. [`Snapshot`] is what
//! [`crate::Reader`] publishes; the screen draws it.
use std::sync::LazyLock;

use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::header::{Control, pane_header, toggle_button};
use super::strip::strips_view;
use super::{Colors, body, icon, note, scale, text_xs};
use crate::explorer::{FileKind, Row, Step, file_kind};
use crate::ui::icon::IconName;

/// A row of the tree, with what Po knows of its file.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub row: Row,
    /// `A`dded, `D`eleted, `M`odified…: how the file changed.
    pub mark: Option<char>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// Nothing to list: why.
    Empty(String),
    Rows(Vec<Entry>),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub glass: bool,
    /// Changes mode (true) or the files.
    pub changes: bool,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    pub body: Body,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { glass: false, changes: false, lead: vec![], trail: vec![], body: Body::Rows(vec![]) }
    }
}

static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// Shows `snapshot`; nothing is redrawn when it is what the screen already has.
pub fn publish(snapshot: Snapshot) {
    SNAPSHOT.send_if_modified(|now| {
        let changed = *now != snapshot;
        if changed {
            *now = snapshot;
        }
        changed
    });
}

fn kind_color(kind: FileKind) -> Box<dyn Fn() -> AdaptiveColor> {
    match kind {
        FileKind::Code => Box::new(Colors::primary()),
        FileKind::Data => Box::new(Colors::warning()),
        FileKind::Doc => Box::new(Colors::muted()),
        FileKind::Web => Box::new(Colors::info()),
        FileKind::Image => Box::new(Colors::added()),
        FileKind::Other => Box::new(Colors::dim(Colors::muted())),
    }
}

fn row(entry: Entry) -> View {
    let Entry { row, mark } = entry;
    let lead = if row.folder {
        icon(if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight }, scale::s3(), Colors::muted()).into_element()
    } else {
        view().w(scale::s2()).h(scale::s2()).flex_none().rounded(super::swatch_radius()).bg(kind_color(file_kind(&row.name))).into_element()
    };
    let id = row.id.clone();
    let mark = mark.map(|status| {
        let color: Box<dyn Fn() -> AdaptiveColor> = match status {
            'A' | 'U' => Box::new(Colors::added()),
            'D' => Box::new(Colors::danger()),
            _ => Box::new(Colors::warning()),
        };
        text(status.to_string()).w(scale::s3()).flex_none().text_size(text_xs()).font_weight(FontWeight::SEMIBOLD).text_color(color)
    });
    view()
        .role(AccessibilityRole::Button)
        .label(row.name.clone())
        .flex_row()
        .items_center()
        .gap(if row.folder { scale::s1() } else { scale::s2() })
        .pl(scale::s2() + scale::s4() * row.depth as f32)
        .pr(scale::s2())
        .py(scale::s1())
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .when(row.selected, |v| v.bg(Colors::selected()))
        .on_press(move || send(Cmd::TreeClick(id.clone())))
        .child(lead)
        .child(body(row.name).flex_1().min_w_0().number_of_lines(1))
        .children(mark)
}

#[component]
pub fn Explorer() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        while snapshot.changed().await.is_ok() {
            let now = snapshot.borrow_and_update().clone();
            state.set(now);
        }
    });

    // The arrow keys move the selection while no field has the keyboard.
    let keys = KeyOptions { when: nsh_core::keymap::When::NotEditing, repeat: true };
    for (key, step) in [("up", Step::Up), ("down", Step::Down), ("left", Step::Left), ("right", Step::Right)] {
        on_key_with(key, keys, move |_| send(Cmd::TreeStep(step)));
    }

    let list = element_ref();
    let rows = move || state.with(|s| if let Body::Rows(rows) = &s.body { rows.len() } else { 0 });
    // The selection stays in view as the keys move it.
    effect(move || {
        let at = state.with(|s| match &s.body {
            Body::Rows(rows) => rows.iter().position(|entry| entry.row.selected),
            Body::Empty(_) => None,
        });
        if let Some(at) = at {
            list.scroll_to_index(at as u32, percentage(0.5), px(0.0), false);
        }
    });
    let item = move |at: Position| {
        let entry = state.with(|s| match &s.body {
            Body::Rows(rows) => rows.get(at.index()).cloned(),
            Body::Empty(_) => None,
        });
        match entry {
            Some(entry) => row(entry),
            None => view(),
        }
    };

    let modes = move || {
        let changes = state.with(|s| s.changes);
        view()
            .flex_row()
            .items_center()
            .gap(scale::s1())
            .child(toggle_button(IconName::Folder, "Source (Ctrl+1)", !changes, || send(Cmd::SetMode(false))))
            .child(toggle_button(IconName::Replace, "Changes (Ctrl+2)", changes, || send(Cmd::SetMode(true))))
    };

    view()
        .flex_1()
        .child(dynamic(move || {
            let (changes, lead, trail, glass) = state.with(|s| (s.changes, s.lead.clone(), s.trail.clone(), s.glass));
            pane_header(if changes { "Changes" } else { "Explorer" }, modes(), &lead, &trail, glass)
        }))
        .child(strips_view())
        .child(
            view()
                .flex_1()
                .min_h_0()
                .p(scale::s1())
                .child(dynamic(move || match state.with(|s| s.body.clone()) {
                    Body::Empty(message) => view()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(note(message))
                        .into_element(),
                    Body::Rows(_) => virtual_list(rows, item).element_ref(&list).estimated_item_size(scale::s7()).flex_1().into_element(),
                })),
        )
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    fn entry(id: &str, name: &str, depth: usize, folder: bool, mark: Option<char>) -> Entry {
        Entry { row: Row { id: id.into(), name: name.into(), depth, folder, expanded: folder, selected: false }, mark }
    }

    #[test]
    fn the_explorer_lists_rows_switches_modes_and_reports_presses() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        publish(Snapshot {
            body: Body::Rows(vec![entry("src", "src", 0, true, None), entry("src/main.rs", "main.rs", 1, false, Some('M'))]),
            ..Snapshot::default()
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Explorer>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(300.), Points(400.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        for shown in ["Explorer", "src", "main.rs", "M"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        app.press("label:main.rs").unwrap();
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::TreeClick("src/main.rs".into())));
        app.press("label:Changes (Ctrl+2)").unwrap();
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::SetMode(true)));

        publish(Snapshot { changes: true, body: Body::Empty("No changes".into()), ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("No changes") && app.shows("Changes"), "{:?}", app.screen().texts());
    }
}
