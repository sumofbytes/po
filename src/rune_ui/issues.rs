//! The issues page: a list across every open project (grouped, searched, filtered by scope,
//! project, priority, label or lateness), a board by status, the epics with their progress, and
//! one issue's or epic's page with its fields editable. Counterpart of `src/nsh/issues.rs` and
//! `epics.rs`; what it edits is [`crate::issues::shared`]. The add box always targets the repo
//! last shown.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rune::{El, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextArea, TextInput};

use super::work::{BODY, HEADING, SMALL, TITLE, muted, priority_tint, status_glyph, status_tint};
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

    /// The repo a new issue goes to: the one the page last showed, else the first.
    fn target(&self, overview: &Overview) -> Option<PathBuf> {
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

fn chip<S: 'static>(k: &Kit, key: String, label: String, on: bool, f: impl Fn(&mut S) + 'static) -> El {
    let c = if on { k.c.text_base } else { k.c.text_muted };
    let pill = row().key(key).px(10.0).py(4.0).radius(12.0).child(text(label).font(SMALL).color(c)).on_press(f);
    k.interactive(if on { pill.fill(k.c.layer_03) } else { pill })
}

fn tabs<S: 'static>(k: &Kit, state: &State, at: At<S>) -> El {
    let page = match state.page {
        Page::List => Some(0),
        Page::Board => Some(1),
        Page::Epics => Some(2),
        Page::Issue(..) => None,
        Page::Epic(..) => Some(2),
    };
    SegmentedControl::new(&["List", "Board", "Epics"], page)
        .label("Issues view")
        .on_pick(move |s: &mut S, i: usize, _: &mut Tx| {
            let state = at(s);
            state.page = [Page::List, Page::Board, Page::Epics][i].clone();
        })
        .build(k)
}

fn toolbar<S: 'static>(k: &Kit, state: &State, at: At<S>, overview: &Overview) -> El {
    let search = TextInput::new(&state.search)
        .label("Search issues")
        .placeholder("Search issues")
        .width(None)
        .on_edit(move |s: &mut S| &mut at(s).search)
        .build(k);
    let mut scopes = row().gap(4.0);
    for (scope, name) in [(Scope::Open, "Open"), (Scope::All, "All"), (Scope::Closed, "Closed")] {
        scopes = scopes.child(chip::<S>(k, format!("scope-{name}"), name.into(), state.scope == scope, move |s| at(s).scope = scope));
    }
    let mut groups = row().gap(4.0).items_center().child(muted(k, "Group"));
    for group in Group::ALL {
        groups = groups.child(chip::<S>(k, format!("group-{}", group.name()), group.name().into(), state.group == group, move |s| at(s).group = group));
    }
    let mut bar = row().w_full().items_center().gap(12.0).child(tabs::<S>(k, state, at)).child(row().grow(1.0).min_w(0.0).child(search)).child(scopes);
    if matches!(state.page, Page::List) {
        bar = bar.child(groups);
    }
    let mut out = col().w_full().gap(8.0);
    // With more than one project open, the page narrows to any of them.
    if overview.repos.len() > 1 {
        let mut projects = row().w_full().flex_wrap().gap(4.0).items_center().child(muted(k, "Projects"));
        projects = projects.child(chip::<S>(k, "projects-all".into(), "All projects".into(), state.projects.is_empty(), move |s| at(s).projects = Vec::new()));
        for repo in &overview.repos {
            let (root, shown) = (repo.root.clone(), state.projects.contains(&repo.root));
            let name = repo.name.clone();
            let press = move |s: &mut S| at(s).toggle_project(root.clone());
            projects = projects.child(chip::<S>(k, format!("project-{name}"), name, shown, press));
        }
        out = out.child(projects);
    }
    out = out.child(bar);
    if let Some(more) = more_filters::<S>(k, state, at, overview) {
        out = out.child(more);
    }
    if let Some(id) = &state.epic {
        let name = overview.repos.iter().find_map(|r| r.tracker.epic(id)).map_or(id.clone(), |e| e.name.clone());
        out = out.child(row().gap(8.0).items_center().child(muted(k, format!("Epic: {name}"))).child(chip::<S>(k, "clear-epic".into(), "Clear".into(), false, move |s| at(s).epic = None)));
    }
    out
}

/// The less used filters (priority, label, late) in one popup from the page's More chip; what
/// of them is set shows beside it as chips that take it off.
fn more_filters<S: 'static>(k: &Kit, state: &State, at: At<S>, overview: &Overview) -> Option<El> {
    let mut menu = rune::PopupMenu::new();
    for (priority, name) in [(Priority::Urgent, "Urgent"), (Priority::High, "High"), (Priority::Medium, "Medium"), (Priority::Low, "Low")] {
        menu = menu.item(rune::PopupItem::new(name.to_lowercase(), name).checked(state.priority == Some(priority)));
    }
    let mut labels: Vec<String> = overview.repos.iter().flat_map(|repo| repo.tracker.config.labels.iter().map(|label| label.name.clone())).collect();
    labels.sort();
    labels.dedup();
    if !labels.is_empty() {
        menu = menu.separator();
        for label in labels {
            menu = menu.item(rune::PopupItem::new(label.clone(), format!("#{label}")).checked(state.label.as_ref() == Some(&label)));
        }
    }
    menu = menu.separator().item(rune::PopupItem::new("late", "Late only").checked(state.late));
    let mut bar = row().w_full().flex_wrap().gap(4.0).items_center();
    if let Some(priority) = state.priority {
        let name = priority.name();
        bar = bar.child(chip::<S>(k, format!("set-{}", name.to_lowercase()), format!("!{}", name.to_lowercase()), true, move |s| at(s).priority = None));
    }
    if let Some(label) = state.label.clone() {
        bar = bar.child(chip::<S>(k, format!("set-#{label}"), format!("#{label}"), true, move |s| at(s).label = None));
    }
    if state.late {
        bar = bar.child(chip::<S>(k, "set-late".into(), "Late".into(), true, move |s| at(s).late = false));
    }
    // The chip that opens the menu shows the count of what is set, so a page dressed by it stays
    // one press from being cleared.
    let n = state.priority.is_some() as usize + state.label.is_some() as usize + state.late as usize;
    let label = if n == 0 { "More".to_string() } else { format!("More ({n})") };
    let chip = k.interactive(row().key("more-filters").px(10.0).py(4.0).radius(12.0).fill(k.c.layer_02).child(text(label).font(SMALL).color(k.c.text_muted)).on_press_tx(move |_: &mut S, tx: &mut Tx| {
        let menu = menu.clone();
        tx.popup_menu(menu, "more-filters", move |s: &mut S, picked: Option<&str>, _: &mut Tx| {
            let state = at(s);
            match picked {
                Some("late") => state.late = !state.late,
                Some(hit) if hit.starts_with('#') => {
                    let picked = hit.trim_start_matches('#').to_string();
                    state.label = if state.label.as_ref() == Some(&picked) { None } else { Some(picked) };
                }
                Some(hit) => {
                    let picked = priority_of(hit);
                    state.priority = if state.priority == picked { None } else { picked };
                }
                _ => {}
            }
        });
    }));
    bar = bar.child(chip);
    Some(bar)
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
        let remove = k.interactive(row().key(key).px(6.0).radius(6.0).child(text("×").font(SMALL).color(k.c.text_muted)).on_press(move |_: &mut S| {
            shared::change_repo(&r2, |tracker, now| tracker.update_epic(&id2, now, |epic| epic.milestones.retain(|m| m.id != *mid.as_str())));
        }));
        milestones = milestones.child(row().w_full().items_center().gap(8.0).child(text(milestone.name.clone()).font(BODY).color(k.c.text_base).grow(1.0)).child(progress(p)).child(muted(k, format!("{}/{}", p.done, p.all))).child(muted(k, milestone.target.map(|t| due_label(t, today())).unwrap_or_default())).child(remove));
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

/// The priority a filter item's id names.
fn priority_of(hit: &str) -> Option<Priority> {
    match hit.to_lowercase().as_str() {
        "urgent" => Some(Priority::Urgent),
        "high" => Some(Priority::High),
        "medium" => Some(Priority::Medium),
        "low" => Some(Priority::Low),
        _ => None,
    }
}

fn add_box<S: 'static>(k: &Kit, state: &State, at: At<S>, overview: &Overview) -> El {
    let overview_now = overview.clone();
    let title = TextInput::new(&state.new_title)
        .label("New issue")
        .placeholder("New issue — title !high #label @epic")
        .width(None)
        .on_edit(move |s: &mut S| &mut at(s).new_title)
        .on_submit({
            let overview = overview_now.clone();
            move |s: &mut S, _: &mut Tx| at(s).add(&overview)
        })
        .build(k);
    let due = TextInput::new(&state.new_due).label("Due").placeholder("Due: tomorrow, fri, nov 2…").width(Some(190.0)).on_edit(move |s: &mut S| &mut at(s).new_due).build(k);
    let add = Button::new("Add").variant(ButtonVariant::Submit).on_press(move |s: &mut S| at(s).add(&overview_now)).build(k);
    row().w_full().items_center().gap(8.0).child(row().grow(1.0).min_w(0.0).child(title)).child(due).child(add)
}

fn issue_line<S: 'static>(k: &Kit, overview: &Overview, repo: usize, number: u64, at: At<S>, show_repo: bool) -> Option<El> {
    let view = overview.repos.get(repo)?;
    let issue = view.tracker.issue(number)?;
    let key = view.tracker.key(number);
    let (root, open) = (view.root.clone(), view.root.clone());
    let status = issue.status;
    let late = issue.status.is_open() && issue.due.is_some_and(|d| d < today());
    let mut line = row()
        .w_full()
        .items_center()
        .gap(10.0)
        .px(8.0)
        .h(34.0)
        .radius(6.0)
        .child(k.interactive(row().key(format!("st-{repo}-{number}")).w(20.0).child(text(status_glyph(status)).font(BODY).color(status_tint(k, status))).on_press(move |_: &mut S| set_status(&root, number, next_status(status)))))
        .child(row().w(3.0).h(16.0).radius(2.0).fill(priority_tint(k, issue.priority)))
        .child(row().w(64.0).child(muted(k, key)))
        .child(
            k.interactive(
                row()
                    .key(format!("is-{repo}-{number}"))
                    .grow(1.0)
                    .min_w(0.0)
                    .child(text(issue.title.clone()).font(BODY).color(if issue.status.is_open() { k.c.text_base } else { k.c.text_muted }))
                    .on_press(move |s: &mut S| {
                        let _ = at(s);
                        send(Cmd::OpenIssue(open.clone(), number));
                    }),
            ),
        );
    for label in &issue.labels {
        line = line.child(muted(k, format!("#{label}")));
    }
    if show_repo {
        line = line.child(muted(k, view.name.clone()));
    }
    if let Some(due) = issue.due {
        line = line.child(row().w(64.0).justify_end().child(text(due_label(due, today())).font(SMALL).color(if late { k.c.danger.fg } else { k.c.text_muted })));
    }
    Some(line)
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
    let mut out = col().w_full().gap(12.0);
    if sections.is_empty() {
        out = out.child(muted(k, "No issues here."));
    }
    for section in sections {
        if !section.title.is_empty() {
            out = out.child(text(format!("{}  {}", section.title, section.issues.len())).font(HEADING).color(k.c.text_base));
        }
        let mut block = col().w_full();
        for (repo, number) in section.issues {
            if let Some(line) = issue_line::<S>(k, overview, repo, number, at, overview.repos.len() > 1) {
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
        let mut column = col()
            .grow(1.0)
            .min_w(0.0)
            .gap(6.0)
            .padding(8.0)
            .radius(8.0)
            .fill(k.c.layer_02)
            .child(text(format!("{}  {}", status.name(), issues.len())).font(HEADING).color(status_tint(k, status)));
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
                            .child(text(issue.title.clone()).font(BODY).color(k.c.text_base))
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
                    .child(row().w_full().items_center().gap(8.0).child(row().grow(1.0).child(text(epic.name.clone()).font(HEADING).color(k.c.text_base))).child(muted(k, name.clone())).child(muted(k, epic.status.name())))
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
            subs = subs.child(row().gap(8.0).child(text(status_glyph(c.status)).font(SMALL).color(status_tint(k, c.status))).child(muted(k, view.tracker.key(c.number))).child(text(c.title.clone()).font(BODY).color(k.c.text_base)));
        }
        page = page.child(property("Sub-issues", subs));
    }
    page.child(description)
}

pub fn view<S: 'static>(overview: &Overview, state: &State, k: &Kit, at: At<S>) -> El {
    let body = match &state.page {
        Page::Issue(root, number) => detail::<S>(k, state, overview, root, *number, at),
        Page::Epic(root, id) => epic_detail::<S>(k, state, overview, root, &id.clone(), at),
        page => {
            let content = match page {
                Page::Board => board::<S>(k, state, overview, at),
                Page::Epics => epics::<S>(k, overview, at),
                _ => list::<S>(k, state, overview, at),
            };
            let mut out = col().w_full().gap(14.0).child(row().w_full().items_center().child(row().grow(1.0).child(text("Issues").font(TITLE).color(k.c.text_base))).child(Button::new("Close").variant(ButtonVariant::Ghost).on_press(|_: &mut S| send(Cmd::CloseIssues)).build(k))).child(toolbar::<S>(k, state, at, overview));
            if matches!(page, Page::List | Page::Board) {
                out = out.child(add_box::<S>(k, state, at, overview));
            }
            for repo in &overview.repos {
                for problem in &repo.problems {
                    out = out.child(text(format!("{}: {problem}", repo.name)).font(SMALL).color(k.c.warning.fg));
                }
            }
            if overview.repos.is_empty() {
                out = out.child(muted(k, "Open a project to see its issues."));
            }
            out.child(content)
        }
    };
    let page = col().w_full().max_w(1100.0).padding(28.0).child(body);
    col().w_full().h_full().min_h(0.0).child(k.scroll_bar(col().w_full().grow(1.0).min_h(0.0).items_center().scroll(ScrollSpec { stick_bottom: false }).child(page)))
}
