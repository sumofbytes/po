//! The issues page: a list across every open project (grouped, searched, filtered by scope), a
//! board by status, the epics with their progress, and one issue's page with its fields editable.
//! Counterpart of `src/nsh/issues.rs` and `epics.rs`; what it edits is [`crate::issues::shared`].
//! Not carried over yet: labels and parent editing, milestones, epic pages, project filter chips.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rune::{El, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextArea, TextInput};

use super::work::{BODY, HEADING, SMALL, TITLE, muted, priority_tint, status_glyph, status_tint};
use crate::date::{Date, due_label, parse_due, today};
use crate::issues::query::{Filter, Group, Scope, parse_quick, sections_across};
use crate::issues::shared::{self, Nav, Overview, Target};
use crate::issues::{Priority, Status};
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
}

#[derive(Default)]
pub struct State {
    pub page: Page,
    scope: Scope,
    group: Group,
    epic: Option<String>,
    search: TextEdit,
    new_title: TextEdit,
    new_due: TextEdit,
    title: TextEdit,
    description: TextEdit,
    due: TextEdit,
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
                self.epic = Some(id);
                Page::List
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
                if let Some(issue) = overview.repos.iter().find(|r| r.root == *root).and_then(|r| r.tracker.issue(*number)) {
                    self.title = TextEdit::new(&issue.title);
                    self.description = TextEdit::new(&issue.description);
                    self.due = TextEdit::new(&issue.due.map(Date::iso).unwrap_or_default());
                    self.loaded = Some((root.clone(), *number));
                    changed = true;
                }
            }
        } else {
            self.loaded = None;
        }
        if self.dirty.is_some_and(|at| at.elapsed() >= SAVE_AFTER) {
            self.flush(overview);
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
        shared::change_repo(&root, |tracker, now| {
            tracker.update(number, now, |issue| {
                if !title.is_empty() {
                    issue.title = title;
                }
                issue.description = description;
                if let Some(due) = due {
                    issue.due = due;
                }
            });
        });
    }

    fn filter(&self) -> Filter {
        Filter { scope: self.scope, text: self.search.text.clone(), epic: self.epic.clone(), ..Filter::default() }
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

    fn open(&mut self, overview: &Overview, root: PathBuf, number: u64) {
        self.flush(overview);
        self.arm_delete = false;
        self.page = Page::Issue(root, number);
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
    let mut out = col().w_full().gap(8.0).child(bar);
    if let Some(id) = &state.epic {
        let name = overview.repos.iter().find_map(|r| r.tracker.epic(id)).map_or(id.clone(), |e| e.name.clone());
        out = out.child(row().gap(8.0).items_center().child(muted(k, format!("Epic: {name}"))).child(chip::<S>(k, "clear-epic".into(), "Clear".into(), false, move |s| at(s).epic = None)));
    }
    out
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

fn list<S: 'static>(k: &Kit, state: &State, overview: &Overview, at: At<S>) -> El {
    let repos: Vec<(&str, &crate::issues::Tracker)> = overview.repos.iter().map(|r| (r.name.as_str(), &r.tracker)).collect();
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
    let repos: Vec<(&str, &crate::issues::Tracker)> = overview.repos.iter().map(|r| (r.name.as_str(), &r.tracker)).collect();
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
        for epic in repo.tracker.epics() {
            any = true;
            let p = repo.tracker.epic_progress(&epic.id);
            let id = epic.id.clone();
            let share = if p.all == 0 { 0.0 } else { p.done as f32 / p.all as f32 };
            let bar = row().w_full().h(6.0).radius(3.0).fill(k.c.layer_03).child(row().grow(share.max(0.0001)).h(6.0).radius(3.0).fill(k.c.success.fg)).child(row().grow((1.0 - share).max(0.0001)));
            out = out.child(k.interactive(
                col()
                    .key(format!("epic-{}-{}", repo.name, epic.id))
                    .w_full()
                    .gap(6.0)
                    .padding(12.0)
                    .radius(8.0)
                    .border(1.0, k.c.border_base)
                    .child(row().w_full().items_center().gap(8.0).child(row().grow(1.0).child(text(epic.name.clone()).font(HEADING).color(k.c.text_base))).child(muted(k, repo.name.clone())).child(muted(k, epic.status.name())))
                    .child(bar)
                    .child(muted(k, format!("{} of {} done · {} in progress", p.done, p.all, p.started)))
                    .on_press(move |s: &mut S| {
                        let state = at(s);
                        state.epic = Some(id.clone());
                        state.page = Page::List;
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
        .child(property("Due", due));
    if !issue.labels.is_empty() {
        page = page.child(property("Labels", muted(k, issue.labels.iter().map(|l| format!("#{l}")).collect::<Vec<_>>().join(" "))));
    }
    if let Some(epic) = issue.epic.as_ref().and_then(|id| view.tracker.epic(id)) {
        page = page.child(property("Epic", muted(k, epic.name.clone())));
    }
    if let Some(parent) = issue.parent {
        page = page.child(property("Parent", muted(k, format!("{} {}", view.tracker.key(parent), view.tracker.issue(parent).map(|p| p.title.clone()).unwrap_or_default()))));
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
