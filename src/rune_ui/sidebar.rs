//! The sidebar on Rune: the projects with their threads, what waits on the user, what agents have
//! suggested, and the pages it opens. Counterpart of `src/nsh/sidebar.rs`; it draws a
//! [`Snapshot`] and sends [`Cmd`]s, so it needs nothing of the `Reader`.
//!
//! Not ported yet: renaming a thread from its menu, and the right-click menu.
use rune::{El, TextEdit, Tx, col, row, text};
use rune_kit::shell::RowStatus;
use rune_kit::{Button, ButtonVariant, Kit, Sidebar, SidebarRow, SidebarSection, TextInput};

use crate::model::bus::{Cmd, send};
use crate::model::sidebar::{Mark, Project, Snapshot, Thread};

const CAPTION: rune::tok::TextStyle = rune::tok::TextStyle::new(12.0, 400.0);

/// What the sidebar keeps between frames: the filter field.
pub struct State {
    pub query: TextEdit,
}

impl Default for State {
    fn default() -> Self {
        Self { query: TextEdit::new("") }
    }
}

/// Where the host keeps the sidebar's state: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut State;

fn status(mark: Mark) -> Option<RowStatus> {
    match mark {
        Mark::Idle => None,
        Mark::Unread => Some(RowStatus::Unread),
        Mark::Running => Some(RowStatus::Busy),
        Mark::Asking => Some(RowStatus::Attention),
        Mark::Failed => Some(RowStatus::Other(text("✕"))),
    }
}

fn thread_row<S: 'static>(k: &Kit, t: &Thread, subtitle: Option<&str>) -> El {
    let (pid, tid) = (t.project, t.id);
    let mut row = SidebarRow::new(t.label.clone())
        .selected(t.selected)
        .on_select(move |_: &mut S, _: &mut Tx| send(Cmd::OpenThread(pid, tid)))
        .on_close(move |_: &mut S, _: &mut Tx| send(Cmd::CloseThread(pid, tid)));
    if let Some(s) = status(t.mark) {
        row = row.status(s);
    }
    if let Some(ago) = &t.ago {
        row = row.detail(ago.clone());
    }
    if let Some(project) = subtitle {
        row = row.subtitle(project.to_string());
    }
    if t.closing {
        row = row.badge("Close again to lose its work");
    }
    row.build(k).key(format!("thread-{pid}-{tid}"))
}

fn project_section<S: 'static>(k: &Kit, p: &Project) -> SidebarSection {
    let pid = p.id;
    let new_thread = Button::new("+").variant(ButtonVariant::Ghost).help("New thread").on_press(move |_: &mut S| send(Cmd::StartThread(pid))).build(k);
    let mut section = SidebarSection::new(p.name.clone())
        .key(format!("project-{pid}"))
        .collapsed(p.collapsed)
        .action(new_thread)
        .on_toggle(move |_: &mut S, _: &mut Tx| send(Cmd::ToggleProject(pid)));
    for t in &p.threads {
        section = section.row(thread_row::<S>(k, t, None));
    }
    if let Some((expanded, hidden)) = p.fold {
        let label = if expanded { "Show less".to_string() } else { format!("{hidden} more") };
        section = section.row(SidebarRow::new(label).select_only().on_select(move |_: &mut S, _: &mut Tx| send(Cmd::ToggleMore(pid))).build(k));
    }
    section
}

/// The sidebar for `snapshot`, with its filter in the host's state through `at`.
pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>) -> El {
    let search = TextInput::new(&state.query)
        .label("Search threads")
        .placeholder("Search")
        .width(None)
        .on_edit(move |s: &mut S| &mut at(s).query)
        .on_change(move |s: &mut S, _: &mut Tx| send(Cmd::SideQuery(at(s).query.text.clone())))
        .build(k);

    let issues_detail = match snapshot.issues {
        (0, _) => String::new(),
        (open, 0) => open.to_string(),
        (open, late) => format!("{open} · {late} late"),
    };
    let pages = col()
        .w_full()
        .child(SidebarRow::new("Home").selected(snapshot.home_open).on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleHome)).build(k).key("home"))
        .child(SidebarRow::new("Issues").selected(snapshot.issues_open).detail(issues_detail).on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleIssues)).build(k).key("issues"))
        .child(SidebarRow::new("New thread").on_select(|_: &mut S, _: &mut Tx| send(Cmd::NewThread)).build(k).key("new-thread"));

    let mut bar = Sidebar::new().label("Projects").header(search).header(pages);

    if !snapshot.suggestions.is_empty() {
        let mut list = col().w_full().gap(8.0).py(8.0);
        for s in &snapshot.suggestions {
            let id = s.id;
            list = list.child(
                col()
                    .w_full()
                    .gap(6.0)
                    .child(text(s.text.clone()).font(CAPTION).color(k.c.text_base))
                    .child(
                        row()
                            .gap(6.0)
                            .child(Button::new(s.yes).variant(ButtonVariant::Submit).on_press(move |_: &mut S| send(Cmd::Answer(id, true))).build(k))
                            .child(Button::new("Not now").on_press(move |_: &mut S| send(Cmd::Answer(id, false))).build(k)),
                    ),
            );
        }
        bar = bar.child(list);
    }

    if !snapshot.waiting.is_empty() {
        let mut needs = SidebarSection::new("Needs you").key("needs-you").count(snapshot.waiting.len());
        for w in &snapshot.waiting {
            needs = needs.row(thread_row::<S>(k, &w.thread, Some(&w.project)));
        }
        bar = bar.section(needs);
    }
    for p in &snapshot.projects {
        bar = bar.section(project_section::<S>(k, p));
    }

    let mut footer = vec![];
    if let Some(status) = &snapshot.status {
        footer.push(
            row()
                .w_full()
                .items_center()
                .gap(6.0)
                .child(row().grow(1.0).min_w(0.0).child(text(status.clone()).font(CAPTION).color(k.c.text_muted)))
                .child(Button::new("Dismiss").variant(ButtonVariant::Ghost).on_press(|_: &mut S| send(Cmd::DismissStatus)).build(k)),
        );
    }
    footer.push(SidebarRow::new("Add project").on_select(|_: &mut S, _: &mut Tx| send(Cmd::AddProject)).build(k).key("add-project"));
    footer.push(SidebarRow::new("Settings").on_select(|_: &mut S, _: &mut Tx| send(Cmd::OpenSettings)).build(k).key("settings"));
    for f in footer {
        bar = bar.footer(f);
    }
    bar.build(k)
}
