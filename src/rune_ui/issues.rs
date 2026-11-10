//! The issues page: a list across every open project (grouped, searched, filtered by scope,
//! project, priority, label or lateness), a board by status, the epics with their progress, and
//! one issue's or epic's page with its fields editable. Counterpart of `src/nsh/issues.rs` and
//! `epics.rs`; what it edits is [`crate::issues::shared`]. The add box always targets the repo
//! last shown.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rune::{El, Icon, Key, KeyEvent, ScrollSpec, TextEdit, Tx, col, icon, row, text};
use rune_kit::{Button, ButtonVariant, IconButton, IconButtonVariant, Kit, TextArea, TextInput};

use super::dropdown::{self, Line};
use super::work::{body_style, heading_style, small_style, muted, priority_tint, status_glyph, status_tint};
use crate::date::{Date, due_label, parse_due, today};
use crate::issues::query::{Filter, Group, Scope, parse_quick, sections_across};
use crate::issues::shared::{self, Nav, Overview, Target};
use crate::issues::{EpicStatus, Priority, Status};
use crate::model::bus::{Cmd, send};

/// How long typing pauses before what was typed is written to the issue's file.
const SAVE_AFTER: Duration = Duration::from_millis(300);

#[derive(Clone, PartialEq, Debug, Default)]
pub enum Page {
    #[default]
    List,
    Board,
    Epics,
    /// An issue of the repo with this root.
    Issue(PathBuf, u64),
    /// An epic of the repo with this root, on its page.
    Epic(PathBuf, String),
}

#[derive(Default)]
pub struct State {
    pub page: Page,
    /// The menu the More chip has open.
    pub menu: super::dropdown::State,
    scope: Scope,
    group: Group,
    epic: Option<String>,
    /// The repo each chip narrows to; empty is every open project.
    projects: Vec<PathBuf>,
    priority: Option<Priority>,
    label: Option<String>,
    late: bool,
    search: TextEdit,
    new_title: TextEdit,
    new_due: TextEdit,
    title: TextEdit,
    description: TextEdit,
    due: TextEdit,
    /// The parent: a key typed over empties; held like the other fields, written on the pause.
    parent: TextEdit,
    /// Which epic's fields the editor holds, and what it typed amid its save pause.
    epic_loaded: Option<(PathBuf, String)>,
    epic_dirty: Option<Instant>,
    epic_name: TextEdit,
    epic_description: TextEdit,
    epic_target: TextEdit,
    /// The milestone being added: its name and when it is done.
    milestone_name: TextEdit,
    milestone_due: TextEdit,
    /// The issue the detail fields hold, so they are loaded once and not over what is typed.
    loaded: Option<(PathBuf, u64)>,
    /// When something was last typed in the detail fields and not yet saved.
    dirty: Option<Instant>,
    arm_delete: bool,
    /// The project a new issue was told to go to; else the one the page last showed.
    add_to: Option<PathBuf>,
}

pub type At<S> = fn(&mut S) -> &mut State;

impl State {
    /// Goes where an agent or Home asked the page to go.
    pub fn go_to(&mut self, nav: Nav, overview: &Overview) {
        self.flush(overview);
        if let Some(filter) = nav.filter {
            self.search = TextEdit::new(&filter.text);
            self.scope = filter.scope;
            self.epic = filter.epic;
        }
        if let Some(group) = nav.group {
            self.group = group;
        }
        let here = shared::watch().borrow().root.clone();
        self.page = match (nav.target, here) {
            (Target::List, _) => Page::List,
            (Target::Board, _) => Page::Board,
            (Target::Epics, _) => Page::Epics,
            (Target::Epic(id), _) => {
                self.epic = Some(id.clone());
                // The page knows one epic more closely: opened with the repo that has it.
                match overview.repos.iter().find(|repo| repo.tracker.epic(&id).is_some()) {
                    Some(repo) => Page::Epic(repo.root.clone(), id.clone()),
                    None => Page::Epics,
                }
            }
            (Target::Issue(number), Some(root)) => Page::Issue(root, number),
            (Target::Issue(_), None) => Page::List,
        };
        self.arm_delete = false;
    }

    /// Loads the fields of the issue on screen once, and writes typed ones after a pause.
    pub fn tick(&mut self, overview: &Overview) -> bool {
        let mut changed = false;
        if let Page::Issue(root, number) = self.page.clone() {
            let (root, number) = (&root, &number);
            if self.loaded.as_ref() != Some(&(root.clone(), *number)) {
                self.flush(overview);
                if let Some((repo, issue)) = overview.repos.iter().find(|r| r.root == *root).and_then(|repo| repo.tracker.issue(*number).map(|issue| (repo, issue))) {
                    self.title = TextEdit::new(&issue.title);
                    self.description = TextEdit::new(&issue.description);
                    self.due = TextEdit::new(issue.due.map(Date::iso).unwrap_or_default().as_str());
                    let parent = issue.parent.map(|p| repo.tracker.key(p)).unwrap_or_default();
                    self.parent = TextEdit::new(&parent);
                    self.loaded = Some((root.clone(), *number));
                    changed = true;
                }
            }
        } else {
            self.loaded = None;
        }
        // The epic's own page carries its fields the same way: loaded once, written after a pause.
        if let Page::Epic(root, id) = self.page.clone() {
            if self.epic_loaded.as_ref() != Some(&(root.clone(), id.clone())) {
                self.flush_epic();
                if let Some(epic) = overview.repos.iter().find(|repo| repo.root == root).and_then(|repo| repo.tracker.epic(&id)) {
                    self.epic_name = TextEdit::new(&epic.name);
                    self.epic_description = TextEdit::new(&epic.description);
                    self.epic_target = TextEdit::new(epic.target.map(Date::iso).unwrap_or_default().as_str());
                    self.milestone_name = TextEdit::new("");
                    self.milestone_due = TextEdit::new("");
                    self.epic_loaded = Some((root.clone(), id.clone()));
                    changed = true;
                }
            }
        } else {
            self.epic_loaded = None;
        }
        if self.dirty.is_some_and(|at| at.elapsed() >= SAVE_AFTER) {
            self.flush(overview);
        }
        if self.epic_dirty.is_some_and(|at| at.elapsed() >= SAVE_AFTER) {
            self.flush_epic();
        }
        changed
    }

    /// Writes what was typed in the issue's fields.
    pub fn flush(&mut self, _overview: &Overview) {
        if self.dirty.take().is_none() {
            return;
        }
        let Some((root, number)) = self.loaded.clone() else { return };
        let title = self.title.text.trim().to_string();
        let description = self.description.text.clone();
        let due = parse_due(&self.due.text, today());
        let parent = self.parent.text.clone();
        shared::change_repo(&root, |tracker, now| {
            // Emptiness clears the parent; a key of an issue here sets it; anything else waits.
            let parent = if parent.trim().is_empty() {
                None
            } else {
                tracker.parse_key(&parent).filter(|p| tracker.issue(*p).is_some() && !tracker.would_loop(number, *p))
            };
            tracker.update(number, now, |issue| {
                if !title.is_empty() {
                    issue.title = title;
                }
                issue.description = description;
                if let Some(due) = due {
                    issue.due = due;
                }
                issue.parent = parent;
            });
        });
    }

    /// The epics of every open repo, by id, so a detail's epic chips name them all.
    fn epics(&self, overview: &Overview) -> Vec<(String, String)> {
        overview.repos.iter().flat_map(|repo| repo.tracker.epics().iter().map(move |epic| (epic.id.clone(), epic.name.clone()))).collect()
    }

    /// Writes what was typed on the epic's page: its fields, and a milestone being added.
    fn flush_epic(&mut self) {
        if self.epic_dirty.take().is_none() {
            return;
        }
        let Some((root, id)) = self.epic_loaded.clone() else { return };
        let name = self.epic_name.text.trim().to_string();
        let description = self.epic_description.text.clone();
        let target = parse_due(&self.epic_target.text, today());
        let (milestone, due) = (self.milestone_name.text.trim().to_string(), parse_due(&self.milestone_due.text, today()));
        let add = !milestone.is_empty() && due.is_some();
        shared::change_repo(&root, |tracker, now| {
            tracker.update_epic(&id, now, |epic| {
                if !name.is_empty() {
                    epic.name = name;
                }
                epic.description = description;
                if let Some(target) = target {
                    epic.target = target;
                }
                if add && let Some(milestone_due) = due {
                    epic.add_milestone(&milestone, milestone_due);
                }
            });
        });
        if add {
            self.milestone_name = TextEdit::new("");
            self.milestone_due = TextEdit::new("");
            self.epic_dirty = Some(Instant::now());
        }
    }

    /// A project chip's press: the repo joins the narrowed set or leaves it.
    fn toggle_project(&mut self, root: PathBuf) {
        if self.projects.contains(&root) {
            self.projects.retain(|kept| *kept != root);
        } else {
            self.projects.push(root);
        }
    }

    fn filter(&self) -> Filter {
        Filter { scope: self.scope, text: self.search.text.clone(), epic: self.epic.clone(), label: self.label.clone(), priority: self.priority, late: self.late }
    }

    /// The repo a new issue goes to: the one it was told to, else the one the page last showed, else the first.
    fn target(&self, overview: &Overview) -> Option<PathBuf> {
        if let Some(chosen) = self.add_to.clone().filter(|root| overview.repos.iter().any(|r| r.root == *root)) {
            return Some(chosen);
        }
        let here = shared::watch().borrow().root.clone();
        here.filter(|root| overview.repos.iter().any(|r| r.root == *root)).or_else(|| overview.repos.first().map(|r| r.root.clone()))
    }

    fn add(&mut self, overview: &Overview) {
        let line = self.new_title.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(due) = parse_due(&self.new_due.text, today()) else { return };
        let Some(repo) = self.target(overview) else { return };
        let Some(tracker) = overview.repos.iter().find(|r| r.root == repo).map(|r| &r.tracker) else { return };
        let quick = parse_quick(&line, tracker);
        if quick.title.is_empty() {
            return;
        }
        shared::change_repo(&repo, |tracker, now| {
            let number = tracker.create(&quick.title, now);
            tracker.update(number, now, |issue| {
                issue.status = Status::Todo;
                issue.priority = quick.priority.unwrap_or(Priority::None);
                issue.labels = quick.labels.clone();
                issue.epic = quick.epic.clone();
                issue.due = due;
            });
        });
        if self.scope == Scope::Closed {
            self.scope = Scope::Open;
        }
        self.new_title = TextEdit::new("");
        self.new_due = TextEdit::new("");
        self.new_title.focused = true;
    }

    /// An issue for the epic's page: created in the epic's repo, its epic set.
    fn add_epic_issue(&mut self, root: PathBuf, epic: &str) {
        let line = self.new_title.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(due) = parse_due(&self.new_due.text, today()) else { return };
        let epic = epic.to_string();
        let overview = shared::overview().borrow().clone();
        let Some(tracker) = overview.repos.iter().find(|repo| repo.root == root).map(|repo| &repo.tracker) else { return };
        let quick = parse_quick(&line, tracker);
        if quick.title.is_empty() {
            return;
        }
        shared::change_repo(&root, |tracker, now| {
            let number = tracker.create(&quick.title, now);
            tracker.update(number, now, |issue| {
                issue.status = Status::Todo;
                issue.priority = quick.priority.unwrap_or(Priority::None);
                issue.labels = quick.labels.clone();
                issue.due = due;
                issue.epic = Some(epic.clone());
            });
        });
        self.new_title = TextEdit::new("");
        self.new_due = TextEdit::new("");
    }

}

/// The next status along, for a click on the circle: Done wraps to Backlog; Canceled is only picked.
pub fn next_status(status: Status) -> Status {
    match status {
        Status::Backlog => Status::Todo,
        Status::Todo => Status::InProgress,
        Status::InProgress => Status::InReview,
        Status::InReview => Status::Done,
        Status::Done | Status::Canceled => Status::Backlog,
    }
}

fn set_status(root: &std::path::Path, number: u64, status: Status) {
    shared::change_repo(root, |tracker, now| tracker.update(number, now, |issue| issue.status = status));
}

static LIST_ICON: Icon = Icon { name: "po-list", view: 16.0, body: r#"<path d="M5.5 4H13M5.5 8H13M5.5 12H13" stroke="currentColor" stroke-linecap="round"/><circle cx="3" cy="4" r="0.6" fill="currentColor"/><circle cx="3" cy="8" r="0.6" fill="currentColor"/><circle cx="3" cy="12" r="0.6" fill="currentColor"/>"# };
static BOARD_ICON: Icon = Icon { name: "po-board", view: 16.0, body: r#"<rect x="2.5" y="2.5" width="11" height="11" rx="1.5" stroke="currentColor"/><path d="M6 5.5V10M10 5.5V8" stroke="currentColor" stroke-linecap="round"/>"# };
static EPICS_ICON: Icon = Icon { name: "po-epics", view: 16.0, body: r#"<path d="M8 2.5L13.5 5.5L8 8.5L2.5 5.5Z M2.5 8.5L8 11.5L13.5 8.5 M2.5 11L8 14L13.5 11" stroke="currentColor" stroke-linejoin="round" stroke-linecap="round"/>"# };
static PLUS_ICON: Icon = Icon { name: "po-plus", view: 16.0, body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-linecap="round"/>"# };
static CALENDAR_ICON: Icon = Icon { name: "po-calendar", view: 16.0, body: r#"<rect x="2.5" y="3.5" width="11" height="10" rx="1.5" stroke="currentColor"/><path d="M2.5 6.5H13.5M5.5 2V4.5M10.5 2V4.5" stroke="currentColor" stroke-linecap="round"/>"# };
static FOLDER_ICON: Icon = Icon { name: "po-folder", view: 16.0, body: r#"<path d="M2.5 4.5V12.5H13.5V6H8L6.5 4.5Z" stroke="currentColor" stroke-linejoin="round"/>"# };
static OPEN_ICON: Icon = Icon { name: "po-open", view: 16.0, body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor"/><circle cx="8" cy="8" r="1.5" fill="currentColor"/>"# };
static SEARCH_ICON: Icon = Icon { name: "po-issue-search", view: 16.0, body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor"/><path d="M10.5 10.5L13.5 13.5" stroke="currentColor" stroke-linecap="round"/>"# };

/// A choice of a field on an issue's page: a pill that is filled while it is the one set.
fn chip<S: 'static>(k: &Kit, key: String, label: String, on: bool, f: impl Fn(&mut S) + 'static) -> El {
    let c = if on { k.c.text_base } else { k.c.text_muted };
    let pill = row().key(key).px(10.0).py(4.0).radius(12.0).child(text(label).font(small_style()).color(c)).on_press(f);
    k.interactive(if on { pill.fill(k.c.layer_03) } else { pill })
}

/// Where a chip's menu is kept: in the page's state.
fn menu_at<S: 'static>(at: At<S>) -> dropdown::At<S> {
    std::rc::Rc::new(move |s: &mut S| &mut at(s).menu)
}

/// A field with no box of its own, to sit inside one: the box around it is the frame.
fn bare<S: 'static>(k: &Kit, edit: &TextEdit, label: &'static str, placeholder: &'static str, at: At<S>, field: fn(&mut State) -> &mut TextEdit) -> El {
    wrapping::<S>(k, edit, label, placeholder, at, field).single_line().grow(1.0).shrink()
}

/// A [`bare`] field whose long text wraps instead of scrolling sideways.
fn wrapping<S: 'static>(k: &Kit, edit: &TextEdit, label: &'static str, placeholder: &'static str, at: At<S>, field: fn(&mut State) -> &mut TextEdit) -> El {
    rune::text_input(edit, k.ty.ui_compact)
        .key(label)
        .color(k.c.text_base)
        .placeholder(placeholder, k.c.text_muted)
        .caret(k.c.text_base, k.selection())
        .label(label)
        .on_edit(move |s: &mut S| field(at(s)))
}

/// A bordered control of the filter row: what it sets, with an icon before and a chevron after.
fn menu_face(k: &Kit, lead: Option<&'static Icon>, label: String) -> El {
    row()
        .h(32.0)
        .px(12.0)
        .gap(8.0)
        .items_center()
        .radius(8.0)
        .border(1.0, k.c.border_base)
        .fill(k.c.layer_01)
        .children(lead.map(|i| icon(i, 14.0).color(k.c.icon_muted)))
        .child(text(label).font(body_style()).color(k.c.text_base))
        .child(icon(&dropdown::CHEVRON, 12.0).color(k.c.text_muted))
}

/// A thing the page is narrowed by, with a cross to take it off.
fn set_pill<S: 'static>(k: &Kit, key: String, what: String, remove: impl Fn(&mut S) + 'static) -> El {
    k.interactive(row().key(key).items_center().gap(4.0).pl(10.0).pr(6.0).py(2.0).radius(10.0).fill(k.c.layer_03).child(text(what).font(small_style()).color(k.c.text_base)).child(icon(k.glyph.close, 12.0).color(k.c.text_muted)).on_press(remove))
}

/// One of the page's views, as a pill with its icon.
fn tab<S: 'static>(k: &Kit, label: &'static str, lead: &'static Icon, on: bool, go: impl Fn(&mut S) + 'static) -> El {
    let ink = if on { k.c.text_base } else { k.c.text_muted };
    let pill = row().key(format!("tab-{label}")).items_center().gap(6.0).px(12.0).py(4.0).radius(14.0).child(icon(lead, 14.0).color(k.c.icon_muted)).child(text(label).font(body_style()).color(ink)).on_press(go);
    k.interactive(if on { pill.fill(k.c.layer_03) } else { pill })
}

/// The page's header: its name, the views, and the way out, over a rule.
fn header<S: 'static>(k: &Kit, state: &State, at: At<S>) -> El {
    let (list, board, epics) = match state.page {
        Page::List | Page::Issue(..) => (true, false, false),
        Page::Board => (false, true, false),
        Page::Epics | Page::Epic(..) => (false, false, true),
    };
    let go = move |page: Page| move |s: &mut S| at(s).page = page.clone();
    let bar = row()
        .w_full()
        .items_center()
        .gap(4.0)
        .px(24.0)
        .py(12.0)
        .child(text("Issues").font(crate::fonts::ui(20.0, 600.0)).color(k.c.text_base).mr(16.0))
        .child(tab::<S>(k, "List", &LIST_ICON, list, go(Page::List)))
        .child(tab::<S>(k, "Board", &BOARD_ICON, board, go(Page::Board)))
        .child(tab::<S>(k, "Epics", &EPICS_ICON, epics, go(Page::Epics)))
        .child(row().grow(1.0))
        .child(IconButton::new(k.glyph.close).variant(IconButtonVariant::Ghost).label("Close").help("Close").on_press(|_: &mut S| send(Cmd::CloseIssues)).build(k));
    col().w_full().child(bar).child(rune::el().w_full().h(1.0).fill(k.c.border_muted))
}

/// What the page can be narrowed by beyond the search, in one menu.
fn more_lines<S: 'static>(state: &State, at: At<S>, overview: &Overview) -> (Vec<Line>, Vec<usize>) {
    let mut lines = vec![Line::note("Priority")];
    for (priority, name) in [(Priority::Urgent, "Urgent"), (Priority::High, "High"), (Priority::Medium, "Medium"), (Priority::Low, "Low")] {
        lines.push(Line::call::<S>(name, state.priority == Some(priority), move |s| {
            let state = at(s);
            state.priority = if state.priority == Some(priority) { None } else { Some(priority) };
        }));
    }
    let mut rules = vec![];
    let mut labels: Vec<String> = overview.repos.iter().flat_map(|repo| repo.tracker.config.labels.iter().map(|label| label.name.clone())).collect();
    labels.sort();
    labels.dedup();
    if !labels.is_empty() {
        rules.push(lines.len());
        lines.push(Line::note("Label"));
        for label in labels {
            let name = label.clone();
            lines.push(Line::call::<S>(format!("#{label}"), state.label.as_ref() == Some(&label), move |s| {
                let state = at(s);
                state.label = if state.label.as_ref() == Some(&name) { None } else { Some(name.clone()) };
            }));
        }
    }
    rules.push(lines.len());
    lines.push(Line::call::<S>("Late only", state.late, move |s| {
        let state = at(s);
        state.late = !state.late;
    }));
    (lines, rules)
}

/// What the page is narrowed or grouped by, over List, Board and Epics alike: a search, the
/// projects, how far along, the less used in one menu and the grouping; and under them, only while
/// something is set, what it is, each to take off.
fn toolbar<S: 'static>(k: &Kit, state: &State, at: At<S>, overview: &Overview) -> El {
    let menu = menu_at::<S>(at);
    let search = row()
        .grow(1.0)
        .min_w(160.0)
        .h(32.0)
        .px(12.0)
        .gap(8.0)
        .items_center()
        .radius(8.0)
        .border(1.0, k.c.border_base)
        .fill(k.c.layer_01)
        .child(icon(&SEARCH_ICON, 14.0).color(k.c.icon_muted))
        .child(bare::<S>(k, &state.search, "Search issues", "Filter", at, |s| &mut s.search));
    let mut bar = row().w_full().flex_wrap().items_center().gap(8.0).child(search);

    // With more than one project open, the page narrows to any of them.
    if overview.repos.len() > 1 {
        let mut lines = vec![];
        for repo in &overview.repos {
            let root = repo.root.clone();
            lines.push(Line::call::<S>(repo.name.clone(), state.projects.contains(&repo.root), move |s| at(s).toggle_project(root.clone())));
        }
        lines.push(Line::call::<S>("All projects", state.projects.is_empty(), move |s| at(s).projects = Vec::new()));
        let label = match state.projects.as_slice() {
            [] => "All projects".to_string(),
            [only] => overview.repos.iter().find(|r| r.root == *only).map(|r| r.name.clone()).unwrap_or_default(),
            some => format!("{} projects", some.len()),
        };
        let rule = lines.len() - 1;
        bar = bar.child(dropdown::opens::<S>(k, "issues-projects", menu_face(k, Some(&FOLDER_ICON), label), lines, vec![rule], menu.clone()));
    }
    let mut scopes = vec![];
    for (scope, name) in [(Scope::Open, "Open"), (Scope::All, "All"), (Scope::Closed, "Closed")] {
        scopes.push(Line::call::<S>(name, state.scope == scope, move |s| at(s).scope = scope));
    }
    let scope_name = match state.scope {
        Scope::Open => "Open",
        Scope::All => "All",
        Scope::Closed => "Closed",
    };
    bar = bar.child(dropdown::opens::<S>(k, "issues-scope", menu_face(k, Some(&OPEN_ICON), scope_name.to_string()), scopes, vec![], menu.clone()));
    let (more, rules) = more_lines::<S>(state, at, overview);
    bar = bar.child(dropdown::opens::<S>(k, "issues-more", menu_face(k, None, "More".to_string()), more, rules, menu.clone()));
    if matches!(state.page, Page::List) {
        let groups = Group::ALL.iter().map(|group| {
            let group = *group;
            Line::call::<S>(group.name(), state.group == group, move |s| at(s).group = group)
        });
        bar = bar.child(dropdown::opens::<S>(k, "issues-group", menu_face(k, Some(&EPICS_ICON), format!("Group: {}", state.group.name())), groups.collect(), vec![], menu));
    }

    let mut out = col().w_full().gap(8.0).child(bar);
    // Each thing the page is narrowed by, to take off.
    let mut set = row().w_full().flex_wrap().items_center().gap(4.0);
    let mut any = false;
    for root in &state.projects {
        let name = overview.repos.iter().find(|r| r.root == *root).map(|r| r.name.clone()).unwrap_or_default();
        let root = root.clone();
        set = set.child(set_pill::<S>(k, format!("set-project-{name}"), name, move |s| at(s).toggle_project(root.clone())));
        any = true;
    }
    if let Some(priority) = state.priority {
        let name = priority.name();
        set = set.child(set_pill::<S>(k, format!("set-{}", name.to_lowercase()), format!("!{}", name.to_lowercase()), move |s| at(s).priority = None));
        any = true;
    }
    if let Some(label) = state.label.clone() {
        set = set.child(set_pill::<S>(k, format!("set-#{label}"), format!("#{label}"), move |s| at(s).label = None));
        any = true;
    }
    if state.late {
        set = set.child(set_pill::<S>(k, "set-late".into(), "Late".into(), move |s| at(s).late = false));
        any = true;
    }
    if let Some(id) = &state.epic {
        let name = overview.repos.iter().find_map(|r| r.tracker.epic(id)).map_or(id.clone(), |e| e.name.clone());
        set = set.child(set_pill::<S>(k, "clear-epic".into(), format!("Epic: {name}"), move |s| at(s).epic = None));
        any = true;
    }
    if any {
        out = out.child(set);
    }
    out
}

/// An epic on its page: its fields, how far along, its milestones and the issues it holds.
fn epic_detail<S: 'static>(k: &Kit, state: &State, overview: &Overview, root: &PathBuf, id: &str, at: At<S>) -> El {
    let back = Button::new("\u{2190} Back").variant(ButtonVariant::Ghost).on_press(move |s: &mut S| {
        let state = at(s);
        state.flush_epic();
        state.arm_delete = false;
        state.epic = None;
        state.page = Page::Epics;
    });
    let Some(view) = overview.repos.iter().find(|repo| repo.root == *root) else {
        return col().w_full().gap(12.0).child(back.build(k)).child(muted(k, "This project isn't open any more."));
    };
    let Some(epic) = view.tracker.epic(id) else {
        return col().w_full().gap(12.0).child(back.build(k)).child(muted(k, "This epic is gone."));
    };
    let dirty = move |s: &mut S, _: &mut Tx| at(s).epic_dirty = Some(Instant::now());
    let name = TextInput::new(&state.epic_name).label("Name").width(None).on_edit(move |s: &mut S| &mut at(s).epic_name).on_change(dirty).build(k);
    let description = TextArea::new(&state.epic_description).label("Description").placeholder("Description (markdown)").auto_grow(4, 16).width(None).on_edit(move |s: &mut S| &mut at(s).epic_description).on_change(dirty).build(k);
    let target = TextInput::new(&state.epic_target).label("Target").placeholder("Due: fri, nov 2, 2026-12-01...").width(Some(240.0)).on_edit(move |s: &mut S| &mut at(s).epic_target).on_change(dirty).build(k);
    let r = root.clone();
    let epic_id = id.to_string();
    let delete = Button::new(if state.arm_delete { "Press again to delete" } else { "Delete" }).variant(ButtonVariant::Ghost).on_press(move |s: &mut S| {
        let state = at(s);
        if !state.arm_delete {
            state.arm_delete = true;
            return;
        }
        state.arm_delete = false;
        state.epic_dirty = None;
        state.epic_loaded = None;
        let epic_id = epic_id.clone();
        shared::change_repo(&r, |tracker, now| tracker.remove_epic(&epic_id, now));
        state.page = Page::Epics;
    }).build(k);

    let mut statuses = row().gap(4.0).flex_wrap();
    for status in EpicStatus::ALL {
        let r = root.clone();
        let id = id.to_string();
        statuses = statuses.child(chip::<S>(k, format!("epic-status-{}", status.slug()), status.name().into(), epic.status == status, move |_| {
            shared::change_repo(&r, |tracker, now| tracker.update_epic(&id, now, |epic| epic.status = status));
        }));
    }

    let progress = |p: crate::issues::Progress| {
        let share = if p.all == 0 { 0.0 } else { p.done as f32 / p.all as f32 };
        row().w(160.0).h(6.0).radius(3.0).fill(k.c.layer_03).child(row().grow(share.max(0.0001)).h(6.0).radius(3.0).fill(k.c.success.fg)).child(row().grow((1.0 - share).max(0.0001)))
    };

    let mut milestones = col().w_full().gap(4.0);
    for milestone in &epic.milestones {
        let p = view.tracker.milestone_progress(id, &milestone.id);
        let key = format!("milestone-{id}-{}", milestone.id);
        let (r2, id2, mid) = (root.clone(), id.to_string(), milestone.id.clone());
        let remove = k.interactive(row().key(key).px(6.0).radius(6.0).child(text("×").font(small_style()).color(k.c.text_muted)).on_press(move |_: &mut S| {
            shared::change_repo(&r2, |tracker, now| tracker.update_epic(&id2, now, |epic| epic.milestones.retain(|m| m.id != *mid.as_str())));
        }));
        milestones = milestones.child(row().w_full().items_center().gap(8.0).child(text(milestone.name.clone()).font(body_style()).color(k.c.text_base).grow(1.0)).child(progress(p)).child(muted(k, format!("{}/{}", p.done, p.all))).child(muted(k, milestone.target.map(|t| due_label(t, today())).unwrap_or_default())).child(remove));
    }
    let milestone_name = TextInput::new(&state.milestone_name).label("New milestone").placeholder("New milestone").width(None).on_edit(move |s: &mut S| &mut at(s).milestone_name).on_change(dirty).build(k);
    let milestone_due = TextInput::new(&state.milestone_due).label("Done by").placeholder("Due: fri, nov 2...").width(Some(220.0)).on_edit(move |s: &mut S| &mut at(s).milestone_due).on_change(dirty).build(k);

    let mut issues = col().w_full().gap(4.0);
    let mut any_issues = false;
    let filter = Filter { scope: Scope::All, epic: Some(id.to_string()), ..Filter::default() };
    let epic_repos = [(view.name.as_str(), &view.tracker)];
    for section in sections_across(&epic_repos, &filter, Group::None, today()) {
        for (repo, number) in section.issues {
            if let Some(line) = issue_line::<S>(k, overview, repo, number, at, false) {
                issues = issues.child(line);
                any_issues = true;
            }
        }
    }
    if !any_issues {
        issues = issues.child(muted(k, "No issues in this epic yet. Add one below."));
    }
    let root_for_add = root.clone();
    let id_for_add = id.to_string();
    let new_title = TextInput::new(&state.new_title).label("New issue").placeholder("New issue - title !high #label").width(None).on_edit(move |s: &mut S| &mut at(s).new_title).on_submit(move |s: &mut S, _: &mut Tx| at(s).add_epic_issue(root_for_add.clone(), &id_for_add)).build(k);
    let new_due = TextInput::new(&state.new_due).label("Due").placeholder("Due: fri, nov 2...").width(Some(220.0)).on_edit(move |s: &mut S| &mut at(s).new_due).build(k);

    let overall = view.tracker.epic_progress(id);
    let property = |name: &str, value: El| row().w_full().items_center().gap(12.0).child(row().w(80.0).child(muted(k, name.to_string()))).child(value);
    col()
        .w_full()
        .gap(14.0)
        .child(row().w_full().items_center().child(row().grow(1.0).child(back.build(k))).child(delete))
        .child(muted(k, view.name.clone()))
        .child(name)
        .child(property("Status", statuses))
        .child(property("Target", target))
        .child(property("Description", description))
        .child(property("Progress", col().w_full().gap(4.0).child(progress(overall)).child(muted(k, format!("{}/{} done", overall.done, overall.all)))))
        .child(property("Milestones", col().w_full().gap(4.0).child(row().gap(8.0).child(milestone_name).child(milestone_due)).child(milestones)))
        .child(property("Issues", col().w_full().gap(4.0).child(row().gap(8.0).child(new_title).child(new_due)).child(issues)))
}

/// The box a new issue is written in, with what it sets in a footer: the project it goes to when
/// there are several, and when it is due.
fn add_box<S: 'static>(k: &Kit, state: &State, at: At<S>, overview: &Overview) -> El {
    let (for_enter, for_button) = (overview.clone(), overview.clone());
    // A title is one line however it is wrapped, so Enter adds it even where a multi-line field would break the line.
    let input = wrapping::<S>(k, &state.new_title, "New issue", "New issue — try: Fix login !high #bug @epic", at, |s| &mut s.new_title).w_full().on_input_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| {
        if event.key == Key::Enter {
            at(s).add(&for_enter);
            return true;
        }
        false
    });
    // One row at rest, as tall as before; wrapped text grows the box downward, the plus staying by the first line.
    let line = k.ty.ui_compact.line_px();
    let pad = ((40.0 - line) / 2.0).max(0.0);
    let plus = row().h(line).items_center().child(icon(&PLUS_ICON, 14.0).color(k.c.icon_muted));
    let top = row().w_full().items_start().gap(8.0).px(12.0).py(pad).child(plus).child(col().grow(1.0).shrink().min_w(0.0).child(rune::auto_grow(input, 1, 8)));

    let mut foot = row().w_full().flex_wrap().items_center().gap(6.0).pl(34.0).pr(12.0).pb(8.0);
    if overview.repos.len() > 1 {
        let target = state.target(overview);
        let name = overview.repos.iter().find(|r| Some(&r.root) == target.as_ref()).map(|r| r.name.clone()).unwrap_or_default();
        let lines = overview
            .repos
            .iter()
            .map(|repo| {
                let root = repo.root.clone();
                Line::call::<S>(repo.name.clone(), Some(&repo.root) == target.as_ref(), move |s| at(s).add_to = Some(root.clone()))
            })
            .collect();
        let face = row().items_center().gap(6.0).px(8.0).py(2.0).radius(10.0).fill(k.c.layer_02).child(row().w(8.0).h(8.0).radius(4.0).fill(k.c.icon_accent)).child(text(name).font(small_style()).color(k.c.text_base)).child(icon(&dropdown::CHEVRON, 12.0).color(k.c.text_muted));
        foot = foot.child(dropdown::opens::<S>(k, "issues-target", face, lines, vec![], menu_at::<S>(at)));
    }
    let said = parse_due(&state.new_due.text, today());
    let hint = match said {
        None => Some(("?".to_string(), k.c.danger.fg)),
        Some(Some(date)) => Some((due_label(date, today()), k.c.text_muted)),
        Some(None) => None,
    };
    let due = row()
        .items_center()
        .gap(6.0)
        .px(8.0)
        .py(2.0)
        .radius(10.0)
        .fill(k.c.layer_02)
        .child(icon(&CALENDAR_ICON, 12.0).color(k.c.icon_muted))
        .child(row().w(150.0).child(bare::<S>(k, &state.new_due, "Due", "Due: tomorrow, fri, nov 2", at, |s| &mut s.new_due).on_submit(move |s: &mut S, _: &mut Tx| at(s).add(&for_button))))
        .children(hint.map(|(note, tint)| text(note).font(small_style()).color(tint)));
    foot = foot.child(due);
    col().w_full().radius(10.0).border(1.0, k.c.border_base).fill(k.c.layer_01).child(top).child(foot)
}

fn issue_line<S: 'static>(k: &Kit, overview: &Overview, repo: usize, number: u64, at: At<S>, show_repo: bool) -> Option<El> {
    let view = overview.repos.get(repo)?;
    let issue = view.tracker.issue(number)?;
    let key = view.tracker.key(number);
    let (root, open) = (view.root.clone(), view.root.clone());
    let status = issue.status;
    let closed = !issue.status.is_open();
    let due_tint = match issue.due {
        Some(due) if !closed && due < today() => k.c.danger.fg,
        Some(due) if !closed && due == today() => k.c.warning.fg,
        _ => k.c.text_muted,
    };
    let mut line = row()
        .key(format!("is-{repo}-{number}"))
        .w_full()
        .items_center()
        .gap(12.0)
        .px(16.0)
        .h(40.0)
        .child(k.interactive(row().key(format!("st-{repo}-{number}")).w(20.0).child(text(status_glyph(status)).font(body_style()).color(status_tint(k, status))).on_press(move |_: &mut S| set_status(&root, number, next_status(status)))))
        .child(row().w(3.0).h(14.0).radius(2.0).fill(priority_tint(k, issue.priority)))
        .child(row().w(56.0).child(muted(k, key)))
        .child(row().grow(1.0).min_w(0.0).child(text(issue.title.clone()).font(body_style()).color(if closed { k.c.text_muted } else { k.c.text_base }).truncate()));
    for label in &issue.labels {
        line = line.child(muted(k, format!("#{label}")));
    }
    if let Some(due) = issue.due {
        line = line.child(text(due_label(due, today())).font(small_style()).color(due_tint));
    }
    if show_repo {
        line = line.child(muted(k, view.name.clone()));
    }
    Some(k.interactive(line.on_press(move |s: &mut S| {
        let _ = at(s);
        send(Cmd::OpenIssue(open.clone(), number));
    })))
}

/// The repos the page shows, narrowed to the projects the chips chose.
fn shown<'a>(overview: &'a Overview, state: &State) -> Vec<(&'a str, &'a crate::issues::Tracker)> {
    if state.projects.is_empty() {
        overview.repos.iter().map(|repo| (repo.name.as_str(), &repo.tracker)).collect()
    } else {
        overview.repos.iter().filter(|repo| state.projects.contains(&repo.root)).map(|repo| (repo.name.as_str(), &repo.tracker)).collect()
    }
}

fn list<S: 'static>(k: &Kit, state: &State, overview: &Overview, at: At<S>) -> El {
    let repos = shown(overview, state);
    let sections = sections_across(&repos, &state.filter(), state.group, today());
    let mut out = col().w_full().gap(8.0);
    if sections.is_empty() {
        let none = overview.repos.iter().all(|repo| repo.tracker.issues().next().is_none());
        out = out.child(row().w_full().justify_center().py(24.0).child(muted(k, if none { "No issues yet. Add one above." } else { "Nothing matches." })));
    }
    for section in sections {
        if !section.title.is_empty() {
            out = out.child(row().items_center().gap(8.0).mt(12.0).px(8.0).child(text(section.title.clone()).font(heading_style()).color(k.c.text_base)).child(muted(k, section.issues.len().to_string())));
        }
        // One card for the section, its issues divided by rules.
        let mut block = col().w_full().radius(10.0).border(1.0, k.c.border_muted).fill(k.c.layer_01);
        let mut first = true;
        for (repo, number) in section.issues {
            if let Some(line) = issue_line::<S>(k, overview, repo, number, at, overview.repos.len() > 1) {
                if !first {
                    block = block.child(rune::el().w_full().h(1.0).fill(k.c.border_muted));
                }
                first = false;
                block = block.child(line);
            }
        }
        out = out.child(block);
    }
    out
}

fn board<S: 'static>(k: &Kit, state: &State, overview: &Overview, at: At<S>) -> El {
    let repos = shown(overview, state);
    let filter = Filter { scope: Scope::All, ..state.filter() };
    let sections = sections_across(&repos, &filter, Group::Status, today());
    let mut columns = row().w_full().gap(12.0).items_start();
    for status in [Status::Backlog, Status::Todo, Status::InProgress, Status::InReview, Status::Done] {
        let issues = sections.iter().find(|s| s.key.as_deref() == Some(status.slug())).map(|s| s.issues.clone()).unwrap_or_default();
        // Alike wide, whatever is in them, and the titles wrap: a column that sizes to its longest
        // title pushes the last one past the page.
        let mut column = col()
            .w(0.0)
            .grow(1.0)
            .min_w(0.0)
            .gap(6.0)
            .padding(8.0)
            .radius(8.0)
            .fill(k.c.layer_02)
            .child(text(format!("{}  {}", status.name(), issues.len())).font(heading_style()).color(status_tint(k, status)));
        for (repo, number) in issues {
            let Some(view) = overview.repos.get(repo) else { continue };
            let Some(issue) = view.tracker.issue(number) else { continue };
            let (root, open) = (view.root.clone(), view.root.clone());
            let next = next_status(status);
            let card = col()
                .w_full()
                .gap(4.0)
                .padding(8.0)
                .radius(6.0)
                .border(1.0, k.c.border_base)
                .child(
                    k.interactive(
                        col()
                            .key(format!("card-{repo}-{number}"))
                            .w_full()
                            .child(muted(k, view.tracker.key(number)))
                            .child(text(issue.title.clone()).font(body_style()).color(k.c.text_base).wrap())
                            .on_press(move |s: &mut S| {
                                let _ = at(s);
                                send(Cmd::OpenIssue(open.clone(), number));
                            }),
                    ),
                )
                .child(
                    k.interactive(row().key(format!("adv-{repo}-{number}")).child(muted(k, format!("Move to {}", next.name()))).on_press(move |_: &mut S| set_status(&root, number, next))),
                );
            column = column.child(card);
        }
        columns = columns.child(column);
    }
    columns
}

fn epics<S: 'static>(k: &Kit, overview: &Overview, at: At<S>) -> El {
    let mut out = col().w_full().gap(8.0);
    let mut any = false;
    for repo in &overview.repos {
        let root = repo.root.clone();
        let name = repo.name.clone();
        for epic in repo.tracker.epics() {
            any = true;
            let p = repo.tracker.epic_progress(&epic.id);
            let id = epic.id.clone();
            let share = if p.all == 0 { 0.0 } else { p.done as f32 / p.all as f32 };
            let bar = row().w_full().h(6.0).radius(3.0).fill(k.c.layer_03).child(row().grow(share.max(0.0001)).h(6.0).radius(3.0).fill(k.c.success.fg)).child(row().grow((1.0 - share).max(0.0001)));
            let (open_root, open_id) = (root.clone(), id.clone());
            out = out.child(k.interactive(
                col()
                    .key(format!("epic-{name}-{}", epic.id))
                    .w_full()
                    .gap(6.0)
                    .padding(12.0)
                    .radius(8.0)
                    .border(1.0, k.c.border_base)
                    .child(row().w_full().items_center().gap(8.0).child(row().grow(1.0).child(text(epic.name.clone()).font(heading_style()).color(k.c.text_base))).child(muted(k, name.clone())).child(muted(k, epic.status.name())))
                    .child(bar)
                    .child(muted(k, format!("{} of {} done · {} in progress", p.done, p.all, p.started)))
                    .on_press(move |s: &mut S| {
                        let state = at(s);
                        state.epic = Some(open_id.clone());
                        // The epic opens on its page, like an issue opens on its own.
                        state.epic_loaded = None;
                        state.page = Page::Epic(open_root.clone(), open_id.clone());
                    }),
            ));
        }
    }
    if !any {
        out = out.child(muted(k, "No epics yet. Agents and the API create them."));
    }
    out
}

fn detail<S: 'static>(k: &Kit, state: &State, overview: &Overview, root: &PathBuf, number: u64, at: At<S>) -> El {
    let back = Button::new("← Back").variant(ButtonVariant::Ghost).on_press(move |s: &mut S| {
        let state = at(s);
        let overview = shared::overview().borrow().clone();
        state.flush(&overview);
        state.arm_delete = false;
        state.page = Page::List;
    });
    let Some(view) = overview.repos.iter().find(|r| r.root == *root) else {
        return col().w_full().gap(12.0).child(back.build(k)).child(muted(k, "This project isn't open any more."));
    };
    let Some(issue) = view.tracker.issue(number) else {
        return col().w_full().gap(12.0).child(back.build(k)).child(muted(k, "This issue is gone."));
    };
    let dirty = move |s: &mut S, _: &mut Tx| at(s).dirty = Some(Instant::now());
    let title = TextInput::new(&state.title).label("Title").width(None).on_edit(move |s: &mut S| &mut at(s).title).on_change(dirty).build(k);
    let description = TextArea::new(&state.description)
        .label("Description")
        .placeholder("Description (markdown)")
        .auto_grow(6, 20)
        .width(None)
        .on_edit(move |s: &mut S| &mut at(s).description)
        .on_change(move |s: &mut S, _: &mut Tx| at(s).dirty = Some(Instant::now()))
        .build(k);
    let due = TextInput::new(&state.due).label("Due").placeholder("YYYY-MM-DD").width(Some(200.0)).on_edit(move |s: &mut S| &mut at(s).due).on_change(move |s: &mut S, _: &mut Tx| at(s).dirty = Some(Instant::now())).build(k);
    let parent = TextInput::new(&state.parent).label("Parent").placeholder("Parent: PO-4; empty clears it").width(Some(220.0)).on_edit(move |s: &mut S| &mut at(s).parent).on_change(move |s: &mut S, _: &mut Tx| at(s).dirty = Some(Instant::now())).build(k);

    let mut statuses = row().gap(4.0).flex_wrap();
    for status in Status::ALL {
        let r = root.clone();
        statuses = statuses.child(chip::<S>(k, format!("status-{}", status.slug()), status.name().into(), issue.status == status, move |_| set_status(&r, number, status)));
    }
    let mut priorities = row().gap(4.0).flex_wrap();
    for priority in Priority::ALL {
        let r = root.clone();
        priorities = priorities.child(chip::<S>(k, format!("prio-{}", priority.slug()), priority.name().into(), issue.priority == priority, move |_| {
            shared::change_repo(&r, |tracker, now| tracker.update(number, now, |issue| issue.priority = priority));
        }));
    }
    let mut label_chips = row().gap(4.0).flex_wrap();
    for label in view.tracker.config.labels.iter().map(|l| l.name.clone()) {
        let r = root.clone();
        let on = issue.labels.contains(&label);
        label_chips = label_chips.child(chip::<S>(k, format!("label-{}--{}", view.name, label), format!("#{label}"), on, move |_| {
            shared::change_repo(&r, |tracker, now| tracker.update(number, now, |issue| {
                if issue.labels.contains(&label) {
                    issue.labels.retain(|kept| *kept != label);
                } else {
                    issue.labels.push(label.clone());
                }
            }));
        }));
    }
    let property = |name: &str, value: El| row().w_full().items_center().gap(12.0).child(row().w(80.0).child(muted(k, name.to_string()))).child(value);
    let r = root.clone();
    let delete = Button::new(if state.arm_delete { "Press again to delete" } else { "Delete" })
        .variant(ButtonVariant::Ghost)
        .on_press(move |s: &mut S| {
            let state = at(s);
            if !state.arm_delete {
                state.arm_delete = true;
                return;
            }
            state.arm_delete = false;
            state.dirty = None;
            state.loaded = None;
            state.page = Page::List;
            shared::change_repo(&r, |tracker, now| tracker.remove(number, now));
        })
        .build(k);
    let mut page = col()
        .w_full()
        .gap(14.0)
        .child(row().w_full().items_center().child(row().grow(1.0).child(back.build(k))).child(delete))
        .child(muted(k, format!("{} · {}", view.tracker.key(number), view.name)))
        .child(title)
        .child(property("Status", statuses))
        .child(property("Priority", priorities))
        .child(property("Due", due))
        .child(property("Parent", parent));
    if !view.tracker.config.labels.is_empty() {
        page = page.child(property("Labels", label_chips));
    }
    // The epic it belongs to, from a chip per epic; None is one of them.
    if !state.epics(overview).is_empty() {
        let mut epic_chips = row().gap(4.0).flex_wrap();
        for (epic_id, epic_name) in state.epics(overview) {
            let r = root.clone();
            let id = epic_id.clone();
            epic_chips = epic_chips.child(chip::<S>(k, format!("epic-{}-{epic_id}", view.name), epic_name, issue.epic.as_ref() == Some(&epic_id), move |_| {
                shared::change_repo(&r, |tracker, now| tracker.update(number, now, |issue| issue.epic = if issue.epic.as_ref() == Some(&id) { None } else { Some(id.clone()) }));
            }));
        }
        page = page.child(property("Epic", epic_chips));
    }

    let children: Vec<_> = view.tracker.children(number).collect();
    if !children.is_empty() {
        let mut subs = col().gap(4.0);
        for c in children {
            subs = subs.child(row().gap(8.0).child(text(status_glyph(c.status)).font(small_style()).color(status_tint(k, c.status))).child(muted(k, view.tracker.key(c.number))).child(text(c.title.clone()).font(body_style()).color(k.c.text_base)));
        }
        page = page.child(property("Sub-issues", subs));
    }
    page.child(description)
}

pub fn view<S: 'static>(overview: &Overview, state: &State, k: &Kit, at: At<S>) -> El {
    let body = match &state.page {
        Page::Issue(root, number) => col().w_full().padding(28.0).child(detail::<S>(k, state, overview, root, *number, at)),
        Page::Epic(root, id) => col().w_full().padding(28.0).child(epic_detail::<S>(k, state, overview, root, &id.clone(), at)),
        page => {
            let content = match page {
                Page::Board => board::<S>(k, state, overview, at),
                Page::Epics => epics::<S>(k, overview, at),
                _ => list::<S>(k, state, overview, at),
            };
            let mut out = col().w_full().gap(12.0).px(24.0).pt(16.0).pb(24.0);
            if matches!(page, Page::List | Page::Board) {
                out = out.child(add_box::<S>(k, state, at, overview));
            }
            out = out.child(toolbar::<S>(k, state, at, overview));
            for repo in &overview.repos {
                for problem in &repo.problems {
                    out = out.child(text(format!("{}: {problem}", repo.name)).font(small_style()).color(k.c.warning.fg));
                }
            }
            if overview.repos.is_empty() {
                out = out.child(muted(k, "Open a project to see its issues."));
            }
            col().w_full().child(header::<S>(k, state, at)).child(out.child(content))
        }
    };
    let page = col().w_full().max_w(1100.0).child(body);
    col().w_full().h_full().min_h(0.0).child(k.scroll_bar(col().w_full().grow(1.0).min_h(0.0).items_center().scroll(ScrollSpec { stick_bottom: false }).child(page)))
}
