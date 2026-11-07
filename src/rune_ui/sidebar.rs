//! The sidebar on Rune: the projects with their threads, what waits on the user, what agents have
//! suggested, and the pages it opens. Counterpart of `src/nsh/sidebar.rs`; it draws a
//! [`Snapshot`] and sends [`Cmd`]s, so it needs nothing of the `Reader`.
//!
//! Not ported yet: renaming a thread from its menu, and the right-click menu.
use rune::{El, Icon, TextEdit, Tx, col, el, row, text};
use rune_kit::shell::RowStatus;
use rune_kit::{Button, ButtonVariant, Kit, Sidebar, SidebarRow, SidebarSection, TextInput};

use crate::model::bus::{Cmd, send};
use crate::model::sidebar::{Mark, Project, Snapshot, Thread};

static HOME: Icon = Icon { name: "po-home", view: 16.0, body: r#"<path d="M2.5 7L8 2.5L13.5 7V13H9.5V9.5H6.5V13H2.5Z" stroke="currentColor" stroke-linejoin="round"/>"# };
static NEW: Icon = Icon { name: "po-new", view: 16.0, body: r#"<path d="M8 3V13M3 8H13" stroke="currentColor" stroke-linecap="round"/>"# };
static ISSUES: Icon = Icon { name: "po-issues", view: 16.0, body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor"/><circle cx="8" cy="8" r="1.5" fill="currentColor"/>"# };
static SEARCH: Icon = Icon { name: "po-search", view: 16.0, body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor"/><path d="M10.5 10.5L13.5 13.5" stroke="currentColor" stroke-linecap="round"/>"# };
static SETTINGS: Icon = Icon { name: "po-settings", view: 16.0, body: r#"<circle cx="8" cy="8" r="2" stroke="currentColor"/><path d="M8 2V3.5M8 12.5V14M2 8H3.5M12.5 8H14M3.8 3.8L4.9 4.9M11.1 11.1L12.2 12.2M12.2 3.8L11.1 4.9M4.9 11.1L3.8 12.2" stroke="currentColor" stroke-linecap="round"/>"# };

const CAPTION: rune::tok::TextStyle = rune::tok::TextStyle::new(12.0, 400.0);

/// What the sidebar keeps between frames: the filter field.
pub struct State {
    pub query: TextEdit,
    /// The filter field is out: the Search row was pressed, or there is something in it.
    pub open: bool,
}

impl Default for State {
    fn default() -> Self {
        Self { query: TextEdit::new(""), open: false }
    }
}

/// Where the host keeps the sidebar's state: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut State;

fn status(mark: Mark) -> Option<RowStatus> {
    match mark {
        Mark::Idle => Some(RowStatus::Other(el().size(10.0, 10.0).radius(5.0).border(1.0, rune::color::hex("#8a8a96").unwrap_or(rune::Color::WHITE)))),
        Mark::Unread => Some(RowStatus::Unread),
        Mark::Running => Some(RowStatus::Busy),
        Mark::Asking => Some(RowStatus::Attention),
        Mark::Failed => Some(RowStatus::Other(text("×"))),
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
pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>, settings_open: bool) -> El {
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
    let searching = state.open || !state.query.text.is_empty();
    let search_row = SidebarRow::new("Search").icon(&SEARCH).selected(searching).on_select(move |s: &mut S, _: &mut Tx| {
        let state = at(s);
        if state.open || !state.query.text.is_empty() {
            state.open = false;
            state.query = TextEdit::new("");
            send(Cmd::SideQuery(String::new()));
        } else {
            state.open = true;
            state.query.focused = true;
        }
    });
    let pages = col()
        .w_full()
        .child(SidebarRow::new("Home").icon(&HOME).selected(snapshot.home_open && !settings_open).on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleHome)).build(k).key("home"))
        .child(SidebarRow::new("New").icon(&NEW).on_select(|_: &mut S, _: &mut Tx| send(Cmd::NewThread)).build(k).key("new-thread"))
        .child(search_row.build(k).key("search"))
        .children(searching.then(|| col().w_full().px(8.0).py(4.0).child(search)))
        .child(SidebarRow::new("Issues").icon(&ISSUES).selected(snapshot.issues_open && !settings_open).detail(issues_detail).on_select(|_: &mut S, _: &mut Tx| send(Cmd::ToggleIssues)).build(k).key("issues"));

    let mut bar = Sidebar::new().label("Projects").header(text("Po").font(rune::tok::TextStyle::new(15.0, 600.0)).color(k.c.text_base).px(12.0)).header(pages);

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
    let add = Button::new("+").variant(ButtonVariant::Ghost).help("Add project").on_press(|_: &mut S| send(Cmd::AddProject)).build(k);
    bar = bar.child(row().w_full().items_center().px(12.0).pt(8.0).child(row().grow(1.0).child(text("Projects").font(CAPTION).color(k.c.text_muted))).child(add));
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
    footer.push(SidebarRow::new("Settings").icon(&SETTINGS).on_select(|_: &mut S, _: &mut Tx| send(Cmd::OpenSettings)).build(k).key("settings"));
    for f in footer {
        bar = bar.footer(f);
    }
    bar.build(k)
}
