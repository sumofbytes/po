//! The issues page: the repo's issues as a list or a board, one issue's details, and the epics
//! they belong to. The data is [`crate::issues::shared`]'s; what is pressed changes it there, and
//! the page follows. [`crate::nsh::epics`] draws the epics.
use nsh_base::text_view;
use nsh_core::prelude::*;

use super::bus::{Cmd, send};
use super::overlay::{self, Height};
use super::{Colors, body, caption, icon, icon_button, note, on_hover, scale, text_sm, text_xs, Sizing, Smooth};
use crate::issues::query::{self, Filter, Group, parse_quick};
use crate::issues::shared::{self, Overview, View as Data};
use crate::issues::{Issue, Link, Priority, Status};
use crate::date::{due_label, parse_due, today};
use crate::ui::icon::IconName;

/// The page fills the space it is given, so the height it measures is not used.
pub static HEIGHT: Height = overlay::height();

const DUE_HINT: &str = "Due: tomorrow, fri, nov 2…";

/// What the add box's due field asks for; the words people say are still read.
const DUE_ISO: &str = "YYYY-MM-DD";

#[derive(Clone, PartialEq, Debug)]
pub enum Page {
    List,
    Board,
    Epics,
    Epic(String),
    Issue(u64),
}

/// The screen's own state: where it is, how it filters, and what its fields hold.
#[derive(Clone, Copy)]
pub struct State {
    pub data: Signal<Data>,
    /// The issues of every open project, which the lists, the board and the epics show.
    pub all: Signal<Overview>,
    /// The projects the page is narrowed to; none means all of them.
    pub projects: Signal<Vec<std::path::PathBuf>>,
    /// Where a new issue goes when the filter doesn't say, and the project that was on screen when
    /// the page opened, which is where it goes until one is picked.
    pub add_to: Signal<Option<std::path::PathBuf>>,
    pub home: Signal<Option<std::path::PathBuf>>,
    /// The popover of the add field's due date is open.
    pub due_open: Signal<bool>,
    pub page: Signal<Page>,
    /// Where Back goes from an issue or an epic.
    pub back_to: Signal<Page>,
    pub filter: Signal<Filter>,
    pub group: Signal<Group>,
    pub search: Signal<String>,
    pub new_title: Signal<String>,
    pub new_due: Signal<String>,
    pub title: Signal<String>,
    pub description: Signal<String>,
    pub due: Signal<String>,
    pub parent: Signal<String>,
    pub new_label: Signal<String>,
    pub new_sub: Signal<String>,
    pub epic_name: Signal<String>,
    pub epic_description: Signal<String>,
    pub epic_target: Signal<String>,
    pub milestone_name: Signal<String>,
    pub milestone_due: Signal<String>,
    pub board_width: Signal<f32>,
    /// The delete button of the page on screen has been pressed once.
    pub arm_delete: Signal<bool>,
    /// How many todos of the old list are still on offer.
    pub old_todos: Signal<usize>,
    /// Fields typed in and not saved yet, and which typing the pending save belongs to.
    pub edited: Signal<Edited>,
    pub pending: Signal<u64>,
    pub add_field: ElementRef,
    /// The title or description being typed in; the others are shown as they read.
    pub editing: Signal<Option<Field>>,
    /// The last press on a field shown, to tell a second one soon after.
    last_press: Stored<Option<std::time::Instant>>,
}

/// A field of the issue and epic pages that is shown as text until it is pressed twice.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Field {
    Title,
    Description,
    EpicName,
    EpicDescription,
}

/// The fields of the page on screen that were typed in since they were last saved.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Edited {
    pub title: bool,
    pub description: bool,
    pub due: bool,
    pub parent: bool,
    pub epic_name: bool,
    pub epic_description: bool,
    pub epic_target: bool,
}

/// How long typing pauses before what was typed is written to the issue's file.
const SAVE_AFTER: std::time::Duration = std::time::Duration::from_millis(300);

impl State {
    fn new() -> Self {
        let s = || signal(String::new());
        Self {
            data: signal(shared::watch().borrow().clone()),
            all: signal(shared::overview().borrow().clone()),
            projects: signal(vec![]),
            add_to: signal(None),
            home: signal(shared::watch().borrow().root.clone()),
            due_open: signal(false),
            page: signal(Page::List),
            back_to: signal(Page::List),
            filter: signal(Filter::default()),
            group: signal(Group::Status),
            search: s(),
            new_title: s(),
            new_due: s(),
            title: s(),
            description: s(),
            due: s(),
            parent: s(),
            new_label: s(),
            new_sub: s(),
            epic_name: s(),
            epic_description: s(),
            epic_target: s(),
            milestone_name: s(),
            milestone_due: s(),
            board_width: signal(0.),
            arm_delete: signal(false),
            old_todos: signal(crate::issues::import::on_offer()),
            edited: signal(Edited::default()),
            pending: signal(0),
            add_field: element_ref(),
            editing: signal(None),
            last_press: stored(None),
        }
    }

    /// Takes up the repo the issue and epic pages show, as it is now. If it is another repo, what
    /// was typed is first saved to the repo it was typed for, and a page of the old repo's issue or
    /// epic is left for the list.
    pub fn follow(self, now: Data) {
        if self.read(|data| data.root != now.root) {
            self.flush();
            if matches!(self.page.get_untracked(), Page::Issue(_) | Page::Epic(_)) {
                self.page.set(Page::List);
                self.back_to.set(Page::List);
            }
            self.arm_delete.set(false);
        }
        self.data.set(now);
    }

    /// Changes the repo this page holds, which may have just stopped being the one [`shared`]
    /// shows, with what was typed for it.
    pub fn save<R>(self, f: impl FnOnce(&mut crate::issues::Tracker, u64) -> R) {
        if let Some(root) = self.read(|data| data.root.clone()) {
            shared::change_repo(&root, f);
        }
    }

    /// What `f` makes of the repos the project filter lets through.
    pub fn visible<R>(self, f: impl FnOnce(&[&shared::RepoView]) -> R) -> R {
        let chosen = self.projects.get();
        self.all.with(|all| {
            let repos: Vec<&shared::RepoView> = all.repos.iter().filter(|repo| chosen.is_empty() || chosen.contains(&repo.root)).collect();
            f(&repos)
        })
    }

    /// The repo a new issue goes to: the one the filter is narrowed to, else the one picked in the
    /// add box, else the one that was on screen, else the first.
    pub fn target(self) -> Option<std::path::PathBuf> {
        self.target_by(false)
    }

    /// [`Self::target`], followed by whatever reads it.
    pub fn target_followed(self) -> Option<std::path::PathBuf> {
        self.target_by(true)
    }

    fn target_by(self, follow: bool) -> Option<std::path::PathBuf> {
        let (chosen, add_to, home) = if follow { (self.projects.get(), self.add_to.get(), self.home.get()) } else { (self.projects.get_untracked(), self.add_to.get_untracked(), self.home.get_untracked()) };
        let with = |f: &mut dyn FnMut(&shared::Overview) -> Option<std::path::PathBuf>| if follow { self.all.with(|all| f(all)) } else { self.all.with_untracked(|all| f(all)) };
        with(&mut |all| {
            let open = |root: &std::path::PathBuf| all.repos.iter().any(|repo| repo.root == *root);
            match chosen.as_slice() {
                [only] if open(only) => Some(only.clone()),
                _ => [add_to.clone(), home.clone()].into_iter().flatten().find(open).or_else(|| all.repos.first().map(|repo| repo.root.clone())),
            }
        })
    }

    /// What `f` makes of the issues of repo `root`, if it is one followed.
    pub fn in_repo<R>(self, root: &std::path::Path, f: impl FnOnce(&crate::issues::Tracker) -> R) -> Option<R> {
        self.all.with_untracked(|all| all.repos.iter().find(|repo| repo.root == root).map(|repo| f(&repo.tracker)))
    }

    /// What `f` makes of the issues as they are now, without following them.
    pub fn read<R>(self, f: impl FnOnce(&Data) -> R) -> R {
        self.data.with_untracked(f)
    }

    /// Adds what the add field holds as an issue, with what is tagged onto it, and leaves the
    /// field empty and focused for the next one. `epic` puts it in an epic whatever the line says.
    pub fn add(self, epic: Option<String>) {
        // A pasted block is one title: its lines run together.
        let line = self.new_title.get_untracked().split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(due) = parse_due(&self.new_due.get_untracked(), today()) else { return };
        // An epic's own page adds to its repo; the lists add where the filter and the chips say.
        let Some(repo) = (if epic.is_some() { self.read(|data| data.root.clone()) } else { self.target() }) else { return };
        let quick = self.in_repo(&repo, |tracker| parse_quick(&line, tracker)).unwrap_or_default();
        if quick.title.is_empty() {
            return;
        }
        shared::change_repo(&repo, |tracker, now| {
            let number = tracker.create(&quick.title, now);
            tracker.update(number, now, |issue| {
                issue.status = Status::Todo;
                issue.priority = quick.priority.unwrap_or(Priority::None);
                issue.labels = quick.labels.clone();
                issue.epic = epic.clone().or(quick.epic.clone());
                issue.due = due;
            });
        });
        // A new issue is one to do, so it shouldn't land out of sight.
        if self.filter.get_untracked().scope == query::Scope::Closed {
            self.filter.update(|filter| filter.scope = query::Scope::Open);
        }
        self.new_title.set(String::new());
        self.new_due.set(String::new());
        self.add_field.focus();
    }

    /// Opens an issue's page, with its fields holding what it has.
    pub fn open_issue(self, number: u64) {
        self.flush();
        let from = self.page.get_untracked();
        if !matches!(from, Page::Issue(_)) {
            self.back_to.set(from);
        }
        let (title, description, due, parent) = self.read(|data| {
            data.tracker.issue(number).map_or_else(Default::default, |issue| {
                (issue.title.clone(), issue.description.clone(), issue.due.map(crate::date::Date::iso).unwrap_or_default(), issue.parent.map(|p| data.tracker.key(p)).unwrap_or_default())
            })
        });
        self.title.set(title);
        self.description.set(description);
        self.due.set(due);
        self.parent.set(parent);
        self.new_label.set(String::new());
        self.new_sub.set(String::new());
        self.arm_delete.set(false);
        self.editing.set(None);
        self.page.set(Page::Issue(number));
    }

    pub fn go(self, page: Page) {
        self.flush();
        self.editing.set(None);
        self.page.set(page);
    }

    /// Notes that a field was typed in, and saves what was typed once the typing pauses: a write
    /// per letter would rewrite the file, scan the folder and re-draw every other window's list
    /// for each one.
    pub fn touch(self, mark: impl FnOnce(&mut Edited)) {
        self.edited.update(mark);
        let typing = self.pending.get_untracked() + 1;
        self.pending.set(typing);
        spawn(async move {
            timer(SAVE_AFTER).await;
            if self.pending.get_untracked() == typing {
                self.flush();
            }
        });
    }

    /// Saves, now, the fields typed in since they were last saved: on Enter, and before the page
    /// is left.
    pub fn flush(self) {
        let edited = self.edited.get_untracked();
        if edited == Edited::default() {
            return;
        }
        self.edited.set(Edited::default());
        match self.page.get_untracked() {
            Page::Issue(number) => {
                if edited.title {
                    self.save_title(number);
                }
                if edited.description {
                    self.save_description(number);
                }
                if edited.due {
                    self.save_due(number);
                }
                if edited.parent {
                    self.save_parent(number);
                }
            }
            Page::Epic(id) => {
                if edited.epic_name {
                    self.save_epic_name(&id);
                }
                if edited.epic_description {
                    self.save_epic_description(&id);
                }
                if edited.epic_target {
                    self.save_epic_target(&id);
                }
            }
            _ => {}
        }
    }

    pub fn back(self) {
        let to = self.back_to.get_untracked();
        self.back_to.set(Page::List);
        self.go(to);
    }

    // The fields of an issue's page are saved as they change, each one alone, so that closing the
    // page loses nothing, and a field the user has not touched is never written over what an
    // agent or git changed meanwhile. A title, due date or parent that can't be read yet is
    // skipped until it can.

    pub fn save_title(self, number: u64) {
        let title = self.title.get_untracked().trim().to_string();
        if !title.is_empty() {
            self.save(|tracker, now| tracker.update(number, now, |issue| issue.title = title));
        }
    }

    pub fn save_description(self, number: u64) {
        let description = self.description.get_untracked();
        self.save(|tracker, now| tracker.update(number, now, |issue| issue.description = description));
    }

    pub fn save_due(self, number: u64) {
        if let Some(due) = parse_due(&self.due.get_untracked(), today()) {
            self.save(|tracker, now| tracker.update(number, now, |issue| issue.due = due));
        }
    }

    /// Empty clears the parent; a key of an issue there is sets it. Anything else waits.
    pub fn save_parent(self, number: u64) {
        let text = self.parent.get_untracked();
        let parent = self.read(|data| if text.trim().is_empty() { Some(None) } else { data.tracker.parse_key(&text).filter(|p| data.tracker.issue(*p).is_some()).map(Some) });
        if let Some(parent) = parent {
            self.save(|tracker, now| tracker.update(number, now, |issue| issue.parent = parent));
        }
    }

    /// Moves an issue of the repo `repo` to `status`.
    pub fn set_status_in(self, repo: &std::path::Path, number: u64, status: Status) {
        shared::change_repo(repo, |tracker, now| tracker.update(number, now, |issue| issue.status = status));
    }

    pub fn set_status(self, number: u64, status: Status) {
        shared::change(|tracker, now| tracker.update(number, now, |issue| issue.status = status));
    }

    /// The next status along, for a click on the circle: Done wraps to Backlog; Canceled is only picked.
    pub fn advance_in(self, repo: &std::path::Path, number: u64) {
        if let Some(next) = self.in_repo(repo, |tracker| tracker.issue(number).map(|issue| next_status(issue.status))).flatten() {
            self.set_status_in(repo, number, next);
        }
    }

    /// Opens an issue of the repo `repo` on its own page; Back comes back to the list, the board or
    /// the epics it was opened from. The project on screen is not touched.
    pub fn open_in(self, repo: &std::path::Path, number: u64) {
        self.flush();
        shared::set_project(repo);
        self.follow(shared::watch().borrow().clone());
        self.open_issue(number);
    }

    /// Deletes an issue, from its own page, on the second press: the first arms the button.
    pub fn delete(self, number: u64) {
        if !self.arm_delete.get_untracked() {
            return self.arm_delete.set(true);
        }
        self.arm_delete.set(false);
        if self.page.get_untracked() == Page::Issue(number) {
            self.back();
        }
        shared::change(|tracker, now| tracker.remove(number, now));
    }

    /// Goes where an agent asked the page to go, with the filter and grouping it named.
    fn go_to(self, nav: shared::Nav) {
        // The nudge may arrive before the news of the repo it was sent for.
        self.follow(shared::watch().borrow().clone());
        if let Some(filter) = nav.filter {
            self.search.set(filter.text.clone());
            self.filter.set(filter);
        }
        if let Some(group) = nav.group {
            self.group.set(group);
        }
        self.back_to.set(Page::List);
        match nav.target {
            shared::Target::List => self.page.set(Page::List),
            shared::Target::Board => self.page.set(Page::Board),
            shared::Target::Epics => self.page.set(Page::Epics),
            shared::Target::Issue(number) => {
                self.page.set(Page::List);
                self.open_issue(number);
            }
            shared::Target::Epic(id) => {
                self.page.set(Page::Epics);
                self.open_epic(&id);
            }
        }
    }

    /// Esc steps back, else empties the search, else closes the page.
    fn escape(self) {
        if !matches!(self.page.get_untracked(), Page::List | Page::Board | Page::Epics) {
            self.back();
        } else if !self.search.get_untracked().is_empty() {
            self.search.set(String::new());
            self.filter.update(|filter| filter.text.clear());
        } else {
            send(Cmd::CloseIssues);
        }
    }
}

/// The next status along, for a click on the circle: Done wraps to Backlog; Canceled is only picked.
fn next_status(status: Status) -> Status {
    match status {
        Status::Backlog => Status::Todo,
        Status::Todo => Status::InProgress,
        Status::InProgress => Status::InReview,
        Status::InReview => Status::Done,
        Status::Done | Status::Canceled => Status::Backlog,
    }
}

use super::epics::{epic_page, epics_page};

/// The most rows a popup list shows before it scrolls.
const POPUP_ROWS: f32 = 12.;

#[component]
pub fn Issues() -> impl IntoElement {
    let state = State::new();
    spawn(async move {
        let mut changes = shared::watch();
        while changes.changed().await.is_ok() {
            let now = changes.borrow_and_update().clone();
            state.follow(now);
        }
    });
    spawn(async move {
        let mut changes = shared::overview();
        while changes.changed().await.is_ok() {
            let now = changes.borrow_and_update().clone();
            state.all.set(now);
        }
    });
    // An agent may have asked for the page to open somewhere, or may ask while it is open.
    if let Some(nav) = shared::take_nav() {
        state.go_to(nav);
    }
    spawn(async move {
        let mut nudges = shared::nav_watch();
        while nudges.changed().await.is_ok() {
            if let Some(nav) = shared::take_nav() {
                state.go_to(nav);
            }
        }
    });
    on_key_with("escape", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, move |_| state.escape());

    view()
        .flex_1()
        .bg(Colors::popover())
        .child(header(state))
        .child(problems_banner(state))
        .child(dynamic(move || match state.page.get() {
            Page::List => list_page(state).into_element(),
            Page::Board => board_page(state).into_element(),
            Page::Epics => epics_page(state).into_element(),
            Page::Epic(id) => epic_page(state, id).into_element(),
            Page::Issue(number) => issue_page(state, number).into_element(),
        }))
}

// ---------------------------------------------------------------------------------------------
// Pieces shared by the screens.

pub fn status_tint(status: Status) -> AdaptiveColor {
    match status {
        Status::Backlog | Status::Canceled => Colors::muted()(),
        Status::Todo => Colors::foreground()(),
        Status::InProgress => Colors::warning()(),
        Status::InReview => Colors::info()(),
        Status::Done => Colors::added()(),
    }
}

pub fn status_icon(status: Status) -> IconName {
    match status {
        Status::Backlog => IconName::CircleDashed,
        Status::Todo => IconName::Circle,
        Status::InProgress => IconName::CircleHalf,
        Status::InReview => IconName::CircleDot,
        Status::Done => IconName::CircleCheck,
        Status::Canceled => IconName::CircleX,
    }
}

pub fn priority_tint(priority: Priority) -> AdaptiveColor {
    match priority {
        Priority::Urgent => Colors::danger()(),
        Priority::High => Colors::warning()(),
        Priority::Medium => Colors::foreground()(),
        Priority::Low => Colors::muted()(),
        Priority::None => Colors::dim(Colors::muted())(),
    }
}

/// A small pill to pick one of several: filled while it is the pick.
pub fn chip(label: impl Into<String>, selected: impl Fn() -> bool + 'static, on_press: impl Fn() + 'static) -> View {
    let label: String = label.into();
    let selected = std::rc::Rc::new(selected);
    let (on, shown) = (selected.clone(), selected.clone());
    super::smooth(view())
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .px(scale::s3())
        .py(scale::s0_5())
        .rounded(super::pill())
        .border_1()
        .border_color(super::hairline())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .style_when(move || on(), |s| s.bg(Colors::selected()))
        .on_press(on_press)
        .child(text(label).text_size(text_xs()).text_color(move || if shown() { Colors::foreground()() } else { Colors::muted()() }))
}

/// A chip with an icon before the text.
pub fn icon_chip(name: IconName, tint: AdaptiveColor, label: impl Into<String>, selected: impl Fn() -> bool + 'static, on_press: impl Fn() + 'static) -> View {
    let label: String = label.into();
    let selected = std::rc::Rc::new(selected);
    let (on, shown) = (selected.clone(), selected.clone());
    super::smooth(view())
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .flex_row()
        .items_center()
        .gap(scale::s1())
        .px(scale::s3())
        .py(scale::s0_5())
        .rounded(super::pill())
        .border_1()
        .border_color(super::hairline())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .style_when(move || on(), |s| s.bg(Colors::selected()))
        .on_press(on_press)
        .child(icon(name, scale::s3(), move || tint))
        .child(text(label).text_size(text_xs()).text_color(move || if shown() { Colors::foreground()() } else { Colors::muted()() }))
}

/// A line of text, filling its row, that reports Enter.
pub fn line_input(id: &str, value: Signal<String>, placeholder: &'static str, on_submit: impl Fn() + 'static) -> nsh_base::Input {
    super::text_input(id, value, placeholder, on_submit).min_w(Points(0.))
}

/// A button that deletes on the second press: the first changes its label to ask.
pub fn delete_button(state: State, label: &'static str, on_press: impl Fn() + 'static) -> View {
    super::smooth(view())
        .role(AccessibilityRole::Button)
        .label(label)
        .flex_none()
        .px(scale::s3())
        .py(scale::s1())
        .rounded(super::pill())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(on_press)
        .child(
            text(move || if state.arm_delete.get() { "Press again to delete".to_string() } else { "Delete".to_string() })
                .text_size(text_sm())
                .text_color(move || if state.arm_delete.get() { Colors::danger()() } else { Colors::muted()() }),
        )
}

impl State {
    /// Whether this press follows another closely: a double press.
    fn double_press(self) -> bool {
        let now = std::time::Instant::now();
        let again = self.last_press.get().is_some_and(|at| now.duration_since(at) < std::time::Duration::from_millis(400));
        self.last_press.replace(if again { None } else { Some(now) });
        again
    }
}

/// A field shown as `shown` until it is pressed twice, then as `input`, which takes the keyboard
/// and gives the field back (saving what was typed) when it loses it.
pub fn editable(state: State, field: Field, shown: impl Fn() -> View + 'static, input: impl Fn() -> AnyElement + 'static) -> AnyElement {
    dynamic(move || {
        if state.editing.get() == Some(field) {
            vec![input()]
        } else {
            vec![shown().test_id(&format!("{field:?}-shown")).on_press(move || {
                if state.double_press() {
                    state.editing.set(Some(field));
                }
            }).into_element()]
        }
    })
}

/// Leaves the field being typed in, saving it.
pub fn stop_editing(state: State) {
    if state.editing.get_untracked().is_some() {
        state.editing.set(None);
        state.flush();
    }
}

/// A markdown description, rendered; `empty` is shown in its place when there is none.
pub fn markdown_box(text: Signal<String>, empty: &'static str) -> View {
    let body = view().px(scale::s2_5()).py(scale::s2()).min_h(scale::lines(4.)).rounded(super::card_radius()).border_1().border_color(Colors::input_border());
    let value = text.get();
    if value.trim().is_empty() {
        body.child(super::note(empty))
    } else {
        body.child(
            text_view()
                .markdown(value)
                .on_link_press(|press: &nsh_base::TextViewLinkPressEvent| send(Cmd::OpenUrl(press.url.to_string())))
                .text_size(super::text_base())
                .text_color(Colors::foreground()),
        )
    }
}

/// Several lines of text, growing to a height, where Enter makes a new line. `on_change` hears
/// every edit and is to set `value`.
pub fn text_area(id: &str, value: Signal<String>, placeholder: &'static str, on_change: impl Fn(&str) + 'static) -> View {
    view()
        .px(scale::s2_5())
        .py(scale::s2())
        .min_h(scale::lines(4.))
        .rounded(super::card_radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .child(
            nsh_core::element::input()
                .test_id(id)
                .value(move || value.get())
                .placeholder(placeholder)
                .placeholder_color(Colors::one(Colors::muted()))
                .selection_color(Colors::one(Colors::caret()))
                .text_color(Colors::foreground())
                .auto_correct(false)
                .multiline(true)
                .on_change(move |text: &str| on_change(text))
                .flex_1()
                .min_w(Points(0.))
                .max_h(scale::s8() * POPUP_ROWS),
        )
}

/// A boxed single line with a lead icon, like the add field.
pub fn boxed(lead: IconName, input: nsh_base::Input, hint: Option<View>) -> View {
    view()
        .min_h(scale::s10())
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .px(scale::s3())
        .rounded(super::card_radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .child(icon(lead, scale::s3_5(), Colors::muted()))
        .child(input)
        .child(hint)
}

/// The due field's reading of what was typed, as a small note beside it.
pub fn due_hint(value: Signal<String>) -> View {
    let read = move || match parse_due(&value.get(), today()) {
        Some(Some(date)) => (due_label(date, today()), false),
        Some(None) => (String::new(), false),
        None => ("?".to_string(), true),
    };
    view().flex_none().child(text(move || read().0).text_size(text_xs()).text_color(move || if read().1 { Colors::danger()() } else { Colors::muted()() }))
}

/// A [`caption`] in medium weight, naming a group of controls.
pub fn legend(label: &'static str) -> Text {
    caption(label).font_weight(FontWeight::MEDIUM)
}

fn header(state: State) -> View {
    let tab = move |label: &'static str, name: IconName, page: Page, also: fn(&Page) -> bool| {
        let selected = std::rc::Rc::new(move |now: &Page| *now == page || also(now));
        let (on, shown) = (selected.clone(), selected.clone());
        let target = match label {
            "Board" => Page::Board,
            "Epics" => Page::Epics,
            _ => Page::List,
        };
        view()
            .role(AccessibilityRole::Button)
            .label(label)
            .flex_row()
            .items_center()
            .gap(scale::s1_5())
            .px(scale::s3())
            .py(scale::s1())
            .rounded(super::pill())
            .cursor_pointer()
            .smooth()
            .hover(|s| s.bg(Colors::hover()))
            .style_when(move || state.page.with(|now| on(now)), |s| s.bg(Colors::selected()))
            .on_press(move || state.go(target.clone()))
            .child(icon(name, scale::s3_5(), Colors::muted()))
            .child(text(label).text_size(text_sm()).text_color(move || if state.page.with(|now| shown(now)) { Colors::foreground()() } else { Colors::muted()() }))
    };
    view()
        .flex_row()
        .items_center()
        .gap(scale::s1())
        .px(scale::s6())
        .py(scale::s3())
        .border_b_1()
        .border_color(super::hairline())
        .child(text("Issues").number_of_lines(1).text_size(scale::s4() + scale::s1()).font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground()).mr(scale::s5()))
        .child(tab("List", IconName::List, Page::List, |p| matches!(p, Page::Issue(_))))
        .child(tab("Board", IconName::SquareKanban, Page::Board, |_| false))
        .child(tab("Epics", IconName::Layers, Page::Epics, |p| matches!(p, Page::Epic(_))))
        .child(view().flex_1())
        .child(icon_button(IconName::Close, scale::s3_5(), "Close (Esc)", || send(Cmd::CloseIssues)))
}

/// What was wrong with the files in `.po` when they were read: one that can't be read is left
/// as it is and not shown, so the page says so rather than letting an issue quietly go missing.
fn problems_banner(state: State) -> View {
    // Each with the project it is in, for the projects the filter lets through.
    let found = move || state.visible(|repos| repos.iter().flat_map(|repo| repo.problems.iter().map(|problem| format!("{}: {problem}", repo.name))).collect::<Vec<_>>());
    let summary = move || {
        let found = found();
        let first = found.first().cloned().unwrap_or_default();
        match found.len() {
            1 => format!("A problem with the files in .po: {first}"),
            n => format!("{n} problems with the files in .po; the first: {first}"),
        }
    };
    view().child(when(move || !found().is_empty(), move || {
        view()
            .px(scale::s4())
            .py(scale::s1_5())
            .bg(Colors::input_background())
            .border_b_1()
            .border_color(super::hairline())
            .child(text(summary).number_of_lines(2).text_size(text_xs()).text_color(Colors::warning()))
    }).otherwise(|| {}))
}

fn no_project() -> View {
    view().py(scale::s6()).items_center().child(note("Open a project to see its issues."))
}

// ---------------------------------------------------------------------------------------------
// The list.

fn list_page(state: State) -> View {
    let has_project = move || state.all.with(|all| !all.repos.is_empty());
    view()
        .flex_1()
        .child(when(has_project, move || {
            view()
                .flex_1()
                .child(
                    view().px(scale::s6()).pt(scale::s4()).pb(scale::s3()).child(add_box(
                        line_input("issue-new-title", state.new_title, "New issue — try: Fix login !high #bug @epic", move || state.add(None)).multiline(true).max_h(px(160.)).auto_focus(true).element_ref(&state.add_field),
                        move || {
                            let mut pills = project_pill(state);
                            pills.push(due_pill(state));
                            pills
                        },
                    )),
                )
                .child(todo_offer(state))
                .child(toolbar(state, Tools::ALL))
                .child(scroll().flex_shrink(1.).child(dynamic(move || rows(state))))
        })
        .otherwise(no_project))
}

/// A small control in the footer of the add box: what it sets, with a dot or an icon before it.
fn pill_button(tip: String, lead: impl IntoElement, label: impl Fn() -> String + 'static, danger: impl Fn() -> bool + 'static) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(tip.clone())
        .flex_none()
        .flex_row()
        .items_center()
        .gap(scale::s1_5())
        .px(scale::s2_5())
        .py(scale::s0_5())
        .rounded(super::pill())
        .border_1()
        .border_color(super::hairline())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(super::tooltip(tip))
        .child(lead)
        .child(text(label).text_size(text_xs()).text_color(move || if danger() { Colors::danger()() } else { Colors::foreground()() }))
}

/// A field for adding something, like [`boxed`], with a row of pills under the line that say where
/// it goes and what it carries. The pills are made as the box is drawn, so they follow what they set.
pub fn add_box(input: nsh_base::Input, pills: impl Fn() -> Vec<AnyElement> + 'static) -> View {
    // The first line sits where a single line is centered in the box and stays there: what is typed
    // past the end of a line goes under it, and the box grows towards the bottom.
    let line = super::leading(super::text_base());
    let above = (scale::s10() - line) / 2.;
    view()
        .flex_col()
        .rounded(super::card_radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(scale::s2())
                .px(scale::s3())
                .pt(above)
                .pb(above)
                .child(view().h(line).flex_none().flex_row().items_center().child(icon(IconName::Plus, scale::s3_5(), Colors::muted())))
                .child(input.sized(super::text_base())),
        )
        .child(dynamic(move || {
            let pills = pills();
            (!pills.is_empty()).then(|| view().flex_row().flex_wrap().items_center().gap(scale::s1_5()).pl(scale::s3() + scale::s3_5() + scale::s2()).pr(scale::s3()).pb(scale::s2()).child(pills))
        }))
}

/// The project a new issue goes to: a pill that opens the list of them. Nothing when there is only
/// one to go to; with the filter narrowed to one, that one, which cannot be changed from here. What
/// it says follows the choice without being made again, so the open list stays.
pub fn project_pill(state: State) -> Vec<AnyElement> {
    let repos: Vec<(std::path::PathBuf, String)> = state.all.with(|all| all.repos.iter().map(|repo| (repo.root.clone(), repo.name.clone())).collect());
    if repos.len() < 2 {
        return vec![];
    }
    let name_of = {
        let repos = repos.clone();
        move || state.target_followed().and_then(|target| repos.iter().find(|(root, _)| *root == target).map(|(_, name)| name.clone())).unwrap_or_default()
    };
    let dot = {
        let name_of = name_of.clone();
        move || view().w(scale::s2()).h(scale::s2()).flex_none().rounded(super::pill()).bg(move || super::sidebar::accent(&name_of()))
    };
    if state.projects.with(|chosen| chosen.len() == 1) {
        let tip = format!("Filtered to {}", name_of());
        return vec![pill_button(tip, dot(), name_of, || false).opacity(crate::theme::tokens::DIM).into_element()];
    }
    let trigger = pill_button("Which project the issue goes to".into(), dot(), name_of, || false).child(icon(IconName::ChevronDown, scale::s3(), Colors::dim(Colors::muted())));
    let mut menu = nsh_base::menu().popup_class(super::chat::MENU).trigger(trigger);
    for (root, name) in repos {
        let (pick, on) = (root.clone(), root);
        let row = view()
            .flex_row()
            .items_center()
            .gap(scale::s3())
            .w_full()
            .child(body(name).flex_1().text_color(Colors::foreground()))
            .child(view().flex_none().w(scale::s3_5()).child(dynamic(move || (state.target_followed().as_ref() == Some(&on)).then(|| icon(IconName::Check, scale::s3_5(), Colors::foreground())))));
        menu = menu.child(
            nsh_base::menu_item()
                .cursor_pointer()
                .px(scale::s3())
                .py(scale::s1_5())
                .hover(|s| s.bg(Colors::hover()))
                .on_select(move || {
                    state.add_to.set(Some(pick.clone()));
                    state.add_field.focus();
                })
                .child(row),
        );
    }
    vec![menu.into_element()]
}

/// The due date of a new issue: a pill that opens a field for a date as `YYYY-MM-DD` (what people
/// say is read too), and Today and Tomorrow to press. Once set it shows the date, with a cross to take it off. It reads
/// the field as it is typed, without being made again, so the field keeps the keys.
pub fn due_pill(state: State) -> AnyElement {
    let read = move || parse_due(&state.new_due.get(), today());
    let label = move || match read() {
        Some(Some(date)) => due_label(date, today()),
        Some(None) => "Due".to_string(),
        None => "Due ?".to_string(),
    };
    let pick = move |said: &'static str| {
        move || {
            state.new_due.set(said.to_string());
            state.due_open.set(false);
            state.add_field.focus();
        }
    };
    let content = view()
        .flex_col()
        .gap(scale::s2())
        .p(scale::s2())
        .child(boxed(IconName::Calendar, line_input("issue-new-due", state.new_due, DUE_ISO, move || {
            state.due_open.set(false);
            state.add_field.focus();
        }).auto_focus(true), Some(due_hint(state.new_due))))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(scale::s1())
                .child(chip("Today", || false, pick("today")))
                .child(chip("Tomorrow", || false, pick("tomorrow"))),
        );
    let trigger = pill_button("When the issue is due".into(), icon(IconName::Calendar, scale::s3(), Colors::muted()), label, move || read().is_none());
    let popover = nsh_base::popover()
        .open(move || Some(state.due_open.get()))
        .on_open_change(move |event: &nsh_base::PopoverEvent| {
            if let nsh_base::PopoverEvent::OpenChange(open, _) = event {
                state.due_open.set(*open);
            }
        })
        .align(Align::Start)
        .popup_class(super::chat::MENU)
        .trigger(trigger)
        .child(content);
    view()
        .flex_row()
        .items_center()
        .gap(scale::s0_5())
        .child(popover)
        .child(dynamic(move || (!state.new_due.get().trim().is_empty()).then(|| icon_button(IconName::Close, scale::s3(), "Clear the due date", move || state.new_due.set(String::new())))))
        .into_element()
}

/// The old todo list, offered once, for this project to take: Po's todo page is gone, and the
/// list it kept is still in `todos.json`. Taking it or dismissing it ends the offer everywhere.
fn todo_offer(state: State) -> View {
    let offered = move || state.old_todos.get() > 0;
    view().child(when(offered, move || {
        view()
            .flex_row()
            .items_center()
            .gap(scale::s3())
            .mx(scale::s4())
            .mb(scale::s2())
            .px(scale::s3())
            .py(scale::s2())
            .rounded(super::card_radius())
            .border_1()
            .border_color(super::hairline())
            .child(body(move || format!("Your old todo list has {} items. Bring them in as issues here?", state.old_todos.get())).flex_1())
            .child(super::button("Import here", true, move || {
                if let Some(repo) = state.target() {
                    shared::change_repo(&repo, |tracker, now| crate::issues::import::from_items(&crate::issues::import::read(&crate::issues::import::file()), tracker, now));
                }
                crate::issues::import::handled("imported");
                state.old_todos.set(0);
            }))
            .child(super::button("Dismiss", false, move || {
                crate::issues::import::handled("dismissed");
                state.old_todos.set(0);
            }))
    }).otherwise(|| {}))
}

/// Which controls of [`toolbar`] a page has: the board has no use for a grouping or a scope, the
/// epics only for a project.
#[derive(Clone, Copy)]
pub struct Tools {
    pub search: bool,
    pub status: bool,
    pub more: bool,
    pub group: bool,
}

impl Tools {
    pub const ALL: Tools = Tools { search: true, status: true, more: true, group: true };
    pub const BOARD: Tools = Tools { search: true, status: false, more: true, group: false };
    pub const PROJECT: Tools = Tools { search: false, status: false, more: false, group: false };
}

/// A row of a menu with its choice shown: a tick at the end for one of several, a box at the start
/// for one that is on beside the others. What is on is read as it changes, so a menu that stays
/// open (`close`: false) shows the choice made in it.
fn menu_row(what: impl Into<String>, boxed_tick: bool, close: bool, on: impl Fn() -> bool + Clone + 'static, press: impl Fn() + 'static) -> AnyElement {
    let (tick, shown) = (on.clone(), on);
    let label = body(what.into()).flex_1().text_color(Colors::foreground());
    let row = if boxed_tick {
        let check = view()
            .flex_none()
            .w(scale::s3_5())
            .h(scale::s3_5())
            .flex_row()
            .items_center()
            .justify_center()
            .rounded(Points(3.))
            .border_1()
            .border_color(Colors::input_border())
            .child(dynamic(move || shown().then(|| icon(IconName::Check, scale::s3(), Colors::foreground()))));
        view().flex_row().items_center().gap(scale::s3()).w_full().child(check).child(label)
    } else {
        let check = view().flex_none().w(scale::s3_5()).child(dynamic(move || tick().then(|| icon(IconName::Check, scale::s3_5(), Colors::foreground()))));
        view().flex_row().items_center().gap(scale::s3()).w_full().child(label).child(check)
    };
    nsh_base::menu_item().close_on_select(close).cursor_pointer().px(scale::s3()).py(scale::s1_5()).hover(|s| s.bg(Colors::hover())).on_select(press).child(row).into_element()
}

/// A title over a run of a menu's rows.
fn menu_title(title: &str) -> AnyElement {
    view().focusable(false).px(scale::s3()).py(scale::s1_5()).child(note(title.to_string())).into_element()
}

/// The button of a menu in the toolbar: what it sets, which follows the choice. As tall and as
/// boxed as the search field beside it, so the row reads as one.
fn menu_button(tip: &str, lead: Option<IconName>, label: impl Fn() -> String + 'static) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(tip.to_string())
        .flex_none()
        .h(scale::s10())
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .px(scale::s3())
        .rounded(super::card_radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(super::tooltip(tip.to_string()))
        .child(lead.map(|name| icon(name, scale::s3_5(), Colors::muted())))
        .child(body(label).text_color(Colors::foreground()))
        .child(icon(IconName::ChevronDown, scale::s3(), Colors::dim(Colors::muted())))
}

/// What the page is narrowed to or grouped by, over List, Board and Epics alike: a search, the
/// projects, how far along, the less used (priority, labels, lateness) in one menu, and the
/// grouping; and under them, only while something is set, what it is, each to take off.
pub fn toolbar(state: State, tools: Tools) -> View {
    let names = move |chosen: &[std::path::PathBuf]| -> Vec<String> { state.all.with(|all| chosen.iter().filter_map(|root| all.repos.iter().find(|repo| repo.root == *root).map(|repo| repo.name.clone())).collect()) };

    let project = move || {
        let repos: Vec<(std::path::PathBuf, String)> = state.all.with(|all| all.repos.iter().map(|repo| (repo.root.clone(), repo.name.clone())).collect());
        if repos.len() < 2 {
            return None;
        }
        let label = move || match state.projects.get().as_slice() {
            [] => "All projects".to_string(),
            [only] => names(std::slice::from_ref(only)).pop().unwrap_or_default(),
            some => format!("{} projects", some.len()),
        };
        let mut menu = nsh_base::menu().popup_class(super::chat::MENU).trigger(menu_button("Filter by project", Some(IconName::FolderTree), label));
        for (root, name) in repos {
            let on = {
                let root = root.clone();
                move || state.projects.with(|chosen| chosen.contains(&root))
            };
            menu = menu.child(menu_row(name, true, false, on, move || {
                state.projects.update(|chosen| if chosen.contains(&root) { chosen.retain(|r| *r != root) } else { chosen.push(root.clone()) })
            }));
        }
        menu = menu.child(nsh_base::menu_separator()).child(menu_row("All projects", false, true, move || state.projects.with(|chosen| chosen.is_empty()), move || state.projects.set(vec![])));
        Some(menu.into_element())
    };

    let status = move || {
        let label = move || match state.filter.with(|f| f.scope) {
            query::Scope::Open => "Open",
            query::Scope::All => "All",
            query::Scope::Closed => "Closed",
        }
        .to_string();
        let mut menu = nsh_base::menu().popup_class(super::chat::MENU).trigger(menu_button("Which issues: open, closed or all", Some(IconName::CircleDot), label));
        for (name, scope) in [("Open", query::Scope::Open), ("All", query::Scope::All), ("Closed", query::Scope::Closed)] {
            menu = menu.child(menu_row(name, false, true, move || state.filter.with(|f| f.scope == scope), move || state.filter.update(|f| f.scope = scope)));
        }
        menu.into_element()
    };

    let more = move || {
        let labels = move || {
            let mut labels: Vec<String> = state.visible(|repos| repos.iter().flat_map(|repo| repo.tracker.config.labels.iter().map(|l| l.name.clone())).collect());
            labels.sort();
            labels.dedup();
            labels
        };
        let mut menu = nsh_base::menu().popup_class(super::chat::MENU).trigger(menu_button("More filters: priority, labels, late", None, || "More".to_string()));
        menu = menu.child(menu_title("Priority"));
        for p in [Priority::Urgent, Priority::High, Priority::Medium, Priority::Low] {
            menu = menu.child(menu_row(p.name(), false, false, move || state.filter.with(|f| f.priority == Some(p)), move || state.filter.update(|f| f.priority = if f.priority == Some(p) { None } else { Some(p) })));
        }
        let labels = labels();
        if !labels.is_empty() {
            menu = menu.child(nsh_base::menu_separator()).child(menu_title("Label"));
            for name in labels {
                let (on, set) = (name.clone(), name.clone());
                menu = menu.child(menu_row(format!("#{name}"), false, false, move || state.filter.with(|f| f.label.as_ref() == Some(&on)), move || state.filter.update(|f| f.label = if f.label.as_ref() == Some(&set) { None } else { Some(set.clone()) })));
            }
        }
        menu = menu.child(nsh_base::menu_separator()).child(menu_row("Late only", true, false, move || state.filter.with(|f| f.late), move || state.filter.update(|f| f.late = !f.late)));
        menu.into_element()
    };

    let group = move || {
        let label = move || format!("Group: {}", state.group.get().name());
        let mut menu = nsh_base::menu().popup_class(super::chat::MENU).trigger(menu_button("How the list is grouped", Some(IconName::Layers), label));
        for group in Group::ALL {
            menu = menu.child(menu_row(group.name(), false, true, move || state.group.get() == group, move || state.group.set(group)));
        }
        menu.into_element()
    };

    // Each thing the page is narrowed by, to take off.
    let set = move || {
        let mut pills: Vec<View> = vec![];
        for root in state.projects.get() {
            let name = names(std::slice::from_ref(&root)).pop().unwrap_or_default();
            pills.push(set_pill(name.clone(), format!("Remove project {name}"), move || state.projects.update(|chosen| chosen.retain(|r| *r != root))));
        }
        let filter = state.filter.get();
        if let Some(label) = filter.label {
            pills.push(set_pill(format!("#{label}"), format!("Remove label {label}"), move || state.filter.update(|f| f.label = None)));
        }
        if let Some(priority) = filter.priority {
            pills.push(set_pill(priority.name().to_string(), format!("Remove priority {}", priority.name()), move || state.filter.update(|f| f.priority = None)));
        }
        if filter.late {
            pills.push(set_pill("Late".into(), "Remove late".into(), move || state.filter.update(|f| f.late = false)));
        }
        (!pills.is_empty()).then(|| view().flex_row().flex_wrap().items_center().gap(scale::s1()).child(pills))
    };

    view()
        .px(scale::s6())
        .pb(scale::s3())
        .gap(scale::s2())
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(scale::s2())
                .child(tools.search.then(|| {
                    view().flex_1().min_w(Points(160.)).child(boxed(IconName::Search, line_input("issue-search", state.search, "Filter", || {}).on_change(move |text: &str| {
                        state.search.set(text.to_string());
                        state.filter.update(|f| f.text = text.to_string());
                    }), None))
                }))
                .child(dynamic(project))
                .child(tools.status.then(|| dynamic(status)))
                .child(tools.more.then(|| dynamic(more)))
                .child(tools.group.then(|| dynamic(group))),
        )
        .child(dynamic(set))
}

/// A thing the page is narrowed by, with a cross to take it off.
fn set_pill(what: String, tip: String, remove: impl Fn() + 'static) -> View {
    view()
        .flex_row()
        .items_center()
        .gap(scale::s0_5())
        .pl(scale::s2_5())
        .pr(scale::s1())
        .py(scale::s0_5())
        .rounded(super::pill())
        .bg(Colors::selected())
        .child(super::small(what))
        .child(view().role(AccessibilityRole::Button).label(tip).flex_none().cursor_pointer().on_press(remove).child(icon(IconName::Close, scale::s3(), Colors::muted())))
}

fn rows(state: State) -> View {
    let tag = state.all.with(|all| all.repos.len() > 1);
    let (out, empty) = state.visible(|repos| {
        let filter = state.filter.get();
        let trackers: Vec<(&str, &crate::issues::Tracker)> = repos.iter().map(|repo| (repo.name.as_str(), &repo.tracker)).collect();
        let mut out = vec![];
        for section in query::sections_across(&trackers, &filter, state.group.get(), today()) {
            if !section.title.is_empty() {
                out.push(
                    view()
                        .flex_row()
                        .items_baseline()
                        .gap(scale::s2())
                        .mt(scale::s5())
                        .px(scale::s2())
                        .child(body(section.title.clone()).font_weight(FontWeight::SEMIBOLD))
                        .child(caption(section.issues.len().to_string())),
                );
            }
            // One card for the section, its issues divided by hairlines.
            let mut card = view().flex_col().rounded(super::card_radius()).border_1().border_color(super::hairline()).bg(Colors::sidebar()).overflow_hidden();
            for (at, (of, number)) in section.issues.iter().enumerate() {
                if at > 0 {
                    card = card.child(view().h(Points(1.)).flex_none().bg(super::hairline()));
                }
                card = card.child(issue_line(state, repos[*of], *number, tag));
            }
            out.push(card);
        }
        (out, repos.iter().all(|repo| repo.tracker.issues().next().is_none()))
    });
    let mut out = out;
    if out.is_empty() {
        let message = if empty { "No issues yet. Add one above." } else { "Nothing matches." };
        out.push(view().py(scale::s6()).items_center().child(note(message)));
    }
    view().gap(scale::s2()).px(scale::s6()).pb(scale::s6()).child(out)
}

/// The line of an issue of the repo the issue and epic pages show.
pub fn issue_row(state: State, number: u64) -> View {
    let Some(repo) = state.read(|data| data.root.clone().map(|root| shared::RepoView { root, name: data.name.clone(), tracker: data.tracker.clone(), problems: vec![] })) else {
        return view();
    };
    issue_line(state, &repo, number, false)
}

/// A line of the list: the state of the issue to press, its priority, key, title and the rest, and
/// the project it is in when there are several to tell apart.
fn issue_line(state: State, repo: &shared::RepoView, number: u64, tag: bool) -> View {
    let tracker = &repo.tracker;
    let Some(issue) = tracker.issue(number) else {
        return view();
    };
    let (key, progress, parent) = (tracker.key(number), tracker.progress(number), issue.parent.map(|p| tracker.key(p)));
    let closed = !issue.status.is_open();
    let (status, priority) = (issue.status, issue.priority);
    let due = issue.due.map(|due| (due, today()));
    let due_tint = match due {
        Some((due, today)) if !closed && due < today => Colors::danger()(),
        Some((due, today)) if !closed && due == today => Colors::warning()(),
        _ => Colors::muted()(),
    };
    let (open_repo, advance_repo) = (repo.root.clone(), repo.root.clone());
    view()
        .group()
        .test_id(&format!("issue-{}-{number}", repo.name))
        .h(scale::s10())
        .flex_row()
        .items_center()
        .gap(scale::s3())
        .px(scale::s4())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || state.open_in(&open_repo, number))
        .child(view().role(AccessibilityRole::Button).label(format!("Status: {}", status.name())).flex_none().on_press(move || state.advance_in(&advance_repo, number)).child(icon(status_icon(status), scale::s3_5(), move || status_tint(status))))
        .child(icon(IconName::Signal, scale::s3(), move || priority_tint(priority)))
        .child(caption(key).flex_none())
        .child(text(issue.title.clone()).flex_1().number_of_lines(1).style_when(closed, |s| s.line_through()).text_color(if closed { Colors::muted()() } else { Colors::foreground()() }))
        .child(parent.map(|parent| caption(format!("↳ {parent}")).flex_none()))
        .child((progress.all > 0).then(|| caption(format!("{}/{}", progress.done, progress.all)).flex_none()))
        .child(issue.labels.iter().map(|label| caption(format!("#{label}")).flex_none()).collect::<Vec<_>>())
        .child(text(due.map(|(due, today)| due_label(due, today)).unwrap_or_default()).flex_none().text_size(text_xs()).text_color(move || due_tint))
        .child(tag.then(|| caption(repo.name.clone()).flex_none()))
}

// ---------------------------------------------------------------------------------------------
// The board.

/// The columns, left to right. Canceled issues are not on the board.
const COLUMNS: [Status; 5] = [Status::Backlog, Status::Todo, Status::InProgress, Status::InReview, Status::Done];

/// The status a card dragged `dx` points lands in, from the column it started in: one column per
/// `width / 5`, kept on the board.
pub fn column_after(from: Status, dx: f32, board_width: f32) -> Status {
    let at = COLUMNS.iter().position(|s| *s == from).unwrap_or(0) as f32;
    if board_width <= 0. {
        return from;
    }
    let moved = (at + dx / (board_width / COLUMNS.len() as f32)).round().clamp(0., COLUMNS.len() as f32 - 1.);
    COLUMNS[moved as usize]
}

fn board_page(state: State) -> View {
    view().flex_1().child(view().pt(scale::s3()).child(toolbar(state, Tools::BOARD))).child(
        scroll().flex_shrink(1.).child(
            view()
                .flex_row()
                .gap(scale::s2())
                .px(scale::s3())
                .py(scale::s3())
                .on_layout(move |bounds: &Bounds<Points>| state.board_width.set(bounds.size.width.0))
                .child(dynamic(move || {
                    let filter = Filter { scope: query::Scope::All, ..state.filter.get() };
                    let tag = state.all.with(|all| all.repos.len() > 1);
                    let columns: Vec<View> = state.visible(|repos| {
                        let today = today();
                        COLUMNS
                            .iter()
                            .map(|status| {
                                let mut cards: Vec<(usize, &Issue)> = repos
                                    .iter()
                                    .enumerate()
                                    .flat_map(|(at, repo)| repo.tracker.issues().filter(|i| i.status == *status && filter.matches(i, &repo.tracker.key(i.number), today)).map(move |i| (at, i)))
                                    .collect();
                                cards.sort_by_key(|(at, i)| (i.priority.rank(), *at, std::cmp::Reverse(i.number)));
                                let count = cards.len();
                                let cards: Vec<View> = cards.into_iter().map(|(at, issue)| card(state, repos[at], issue, tag)).collect();
                                column(*status, count, cards)
                            })
                            .collect()
                    });
                    view().flex_1().flex_row().gap(scale::s2()).child(columns)
                })),
        ),
    )
}

fn column(status: Status, count: usize, cards: Vec<View>) -> View {
    view()
        .test_id(&format!("column-{}", status.slug()))
        .flex_1()
        .min_w(Points(0.))
        .gap(scale::s1_5())
        .p(scale::s1_5())
        .rounded(super::card_radius())
        .border_1()
        .border_color(super::hairline())
        .bg(Colors::input_background())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s1_5())
                .px(scale::s1())
                .child(icon(status_icon(status), scale::s3(), move || status_tint(status)))
                .child(text(status.name()).text_size(text_xs()).font_weight(FontWeight::SEMIBOLD).number_of_lines(1).text_color(Colors::foreground()))
                .child(caption(count.to_string())),
        )
        .child(cards)
}

fn card(state: State, repo: &shared::RepoView, issue: &Issue, tag: bool) -> View {
    let (number, status, priority) = (issue.number, issue.status, issue.priority);
    let key = repo.tracker.key(number);
    let (root, open_root, drag_root, left_root, right_root) = (repo.root.clone(), repo.root.clone(), repo.root.clone(), repo.root.clone(), repo.root.clone());
    let _ = root;
    let left = COLUMNS.iter().position(|s| *s == status).and_then(|at| at.checked_sub(1)).map(|at| COLUMNS[at]);
    let right = COLUMNS.iter().position(|s| *s == status).and_then(|at| COLUMNS.get(at + 1)).copied();
    view()
        .group()
        .test_id(&format!("card-{}-{number}", repo.name))
        .gap(scale::s1())
        .p(scale::s3())
        .rounded(super::card_radius())
        .border_1()
        .border_color(super::hairline())
        .bg(Colors::popover())
        .shadow_sm()
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || state.open_in(&open_root, number))
        // Dragging a card across the columns moves it to the one it ends over.
        .on_pan_end(move |event: &PanEvent| {
            if event.cancelled {
                return;
            }
            let to = column_after(status, event.translation.x.0, state.board_width.get_untracked());
            if to != status {
                state.set_status_in(&drag_root, number, to);
            }
        })
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s1_5())
                .child(icon(IconName::Signal, scale::s2_5(), move || priority_tint(priority)))
                .child(caption(key).flex_1())
                .child(left.map(|to| on_hover(icon_button(IconName::ChevronLeft, scale::s2_5(), "Move left", move || state.set_status_in(&left_root, number, to)))))
                .child(right.map(|to| on_hover(icon_button(IconName::ChevronRight, scale::s2_5(), "Move right", move || state.set_status_in(&right_root, number, to))))),
        )
        .child(body(issue.title.clone()))
        .child(tag.then(|| caption(repo.name.clone())))
        .child(
            issue.due.map(|due| {
                let late = issue.status.is_open() && due < today();
                text(due_label(due, today())).text_size(text_xs()).text_color(if late { Colors::danger()() } else { Colors::muted()() })
            }),
        )
}

// ---------------------------------------------------------------------------------------------
// One issue.

fn issue_page(state: State, number: u64) -> View {
    let exists = move || state.data.with(|data| data.tracker.issue(number).is_some());
    view().flex_1().child(when(exists, move || issue_body(state, number)).otherwise(move || {
        view().p(scale::s6()).gap(scale::s3()).items_center().child(text("This issue is gone.").text_color(Colors::muted())).child(super::button("Back", false, move || state.back()))
    }))
}

/// A labelled row of properties.
fn property(label: &'static str, body: Vec<View>) -> View {
    view().flex_row().items_start().gap(scale::s3()).child(view().w(scale::legend_width()).flex_none().pt(scale::s1()).child(legend(label))).child(view().flex_1().flex_row().flex_wrap().items_center().gap(scale::s1()).child(body))
}

fn issue_body(state: State, number: u64) -> AnyElement {
    let key = state.read(|data| format!("{} · {}", data.name, data.tracker.key(number)));
    let sections = move || {
        let Some(issue) = state.data.with(|data| data.tracker.issue(number).cloned()) else { return view() };
        let status = issue.status;
        let priority = issue.priority;
        let statuses: Vec<View> = Status::ALL.iter().map(|s| { let s = *s; icon_chip(status_icon(s), status_tint(s), s.name(), move || status == s, move || state.set_status(number, s)) }).collect();
        let priorities: Vec<View> = Priority::ALL
            .iter()
            .map(|p| {
                let p = *p;
                icon_chip(IconName::Signal, priority_tint(p), p.name(), move || priority == p, move || { shared::change(|t, now| t.update(number, now, |i| i.priority = p)); })
            })
            .collect();
        let (labels, epics, milestones, links, subs) = state.data.with(|data| {
            let labels: Vec<View> = data
                .tracker
                .config
                .labels
                .iter()
                .map(|l| {
                    let (name, on) = (l.name.clone(), issue.labels.contains(&l.name));
                    chip(format!("#{name}"), move || on, move || {
                        shared::change(|t, now| t.update(number, now, |i| if on { i.labels.retain(|x| *x != name) } else { i.labels.push(name.clone()) }));
                    })
                })
                .collect();
            let mut epics: Vec<View> = vec![{
                let on = issue.epic.is_none();
                chip("No epic", move || on, move || { shared::change(|t, now| t.update(number, now, |i| i.epic = None)); })
            }];
            epics.extend(data.tracker.epics().iter().map(|e| {
                let (id, on) = (e.id.clone(), issue.epic.as_ref() == Some(&e.id));
                chip(e.name.clone(), move || on, move || { shared::change(|t, now| t.update(number, now, |i| i.epic = Some(id.clone()))); })
            }));
            let milestones: Vec<View> = issue
                .epic
                .as_ref()
                .and_then(|id| data.tracker.epic(id))
                .map(|epic| {
                    let mut out = vec![{
                        let on = issue.milestone.is_none();
                        chip("None", move || on, move || { shared::change(|t, now| t.update(number, now, |i| i.milestone = None)); })
                    }];
                    out.extend(epic.milestones.iter().map(|m| {
                        let (id, on) = (m.id.clone(), issue.milestone.as_ref() == Some(&m.id));
                        chip(m.name.clone(), move || on, move || { shared::change(|t, now| t.update(number, now, |i| i.milestone = Some(id.clone()))); })
                    }));
                    out
                })
                .unwrap_or_default();
            let children: Vec<u64> = data.tracker.children(number).map(|c| c.number).collect();
            let subs: Vec<View> = children
                .iter()
                .filter_map(|c| data.tracker.issue(*c))
                .map(|c| {
                    let (n, s) = (c.number, c.status);
                    view()
                        .test_id(&format!("sub-{n}"))
                        .h(scale::s7())
                        .flex_row()
                        .items_center()
                        .gap(scale::s2())
                        .px(scale::s2())
                        .rounded(Colors::radius())
                        .cursor_pointer()
                        .smooth()
                        .hover(|st| st.bg(Colors::hover()))
                        .on_press(move || state.open_issue(n))
                        .child(icon(status_icon(s), scale::s3(), move || status_tint(s)))
                        .child(caption(data.tracker.key(n)).flex_none())
                        .child(text(c.title.clone()).flex_1().number_of_lines(1).text_color(Colors::foreground()))
                })
                .collect();
            let links: Vec<View> = issue
                .links
                .iter()
                .map(|link| {
                    let (name, label) = match link {
                        Link::Thread(session) => (IconName::FileText, format!("Thread {}", &session[..session.len().min(8)])),
                        Link::Branch(branch) => (IconName::GitBranch, branch.clone()),
                        Link::Pr(pr) => (IconName::GitBranch, format!("PR #{pr}")),
                    };
                    view().flex_row().items_center().gap(scale::s1_5()).child(icon(name, scale::s3(), Colors::muted())).child(caption(label))
                })
                .collect();
            (labels, epics, milestones, links, subs)
        });
        view()
            .gap(scale::s3())
            .child(property("Status", statuses))
            .child(property("Priority", priorities))
            .child(property("Labels", labels))
            .child(property("Epic", epics))
            .child((!milestones.is_empty()).then(|| property("Milestone", milestones)))
            .child((!links.is_empty()).then(|| property("Links", links)))
            .child(property("Sub-issues", vec![view().w_full().gap(scale::s0_5()).child(subs)]))
    };
    scroll().flex_shrink(1.).child(
        view()
            .gap(scale::s3())
            .px(scale::s4())
            .py(scale::s3())
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(scale::s2())
                    .child(icon_button(IconName::ChevronLeft, scale::s3_5(), "Back (Esc)", move || state.back()))
                    .child(note(key).flex_1())
                    .child(super::button("Work on this", true, move || send(Cmd::WorkOnIssue(number))))
                    .child(delete_button(state, "Delete issue", move || state.delete(number))),
            )
            .child(editable(
                state,
                Field::Title,
                move || {
                    let title = state.title.get();
                    let empty = title.trim().is_empty();
                    view().min_h(scale::s10()).justify_center().child(
                        text(if empty { "Untitled (double-click to edit)".to_string() } else { title })
                            .sized(super::font(1.25))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(move || if empty { Colors::muted()() } else { Colors::foreground()() }),
                    )
                },
                move || {
                    boxed(IconName::Pencil, line_input("issue-title", state.title, "Title", move || stop_editing(state)).auto_focus(true).on_blur(move || stop_editing(state)).on_change(move |text: &str| {
                        state.title.set(text.to_string());
                        state.touch(|edited| edited.title = true);
                    }), None)
                    .into_element()
                },
            ))
            .child(editable(
                state,
                Field::Description,
                move || markdown_box(state.description, "No description. Double-click to write one (markdown)."),
                move || {
                    text_area("issue-description", state.description, "Description (markdown)", move |text| {
                        state.description.set(text.to_string());
                        state.touch(|edited| edited.description = true);
                    })
                    .on_blur(move || stop_editing(state))
                    .into_element()
                },
            ))
            .child(dynamic(sections))
            .child(property("Due", vec![view().flex_1().child(boxed(IconName::Calendar, line_input("issue-due", state.due, DUE_HINT, move || state.flush()).on_change(move |text: &str| {
                state.due.set(text.to_string());
                state.touch(|edited| edited.due = true);
            }), Some(due_hint(state.due))))]))
            .child(property("Sub-issue of", vec![boxed(IconName::CornerDownRight, line_input("issue-parent", state.parent, "Key, e.g. PO-3 (empty for none)", move || state.flush()).on_change(move |text: &str| {
                state.parent.set(text.to_string());
                state.touch(|edited| edited.parent = true);
            }), None)]))
            .child(property("Add label", vec![boxed(IconName::Tag, line_input("issue-label", state.new_label, "New label", move || {
                let name = state.new_label.get_untracked().trim().to_string();
                if !name.is_empty() {
                    shared::change(|t, now| {
                        t.define_label(&name, "");
                        t.update(number, now, |i| i.labels.push(name.clone()));
                    });
                    state.new_label.set(String::new());
                }
            }), None)]))
            .child(property("Add sub-issue", vec![boxed(IconName::Plus, line_input("issue-sub", state.new_sub, "Title", move || {
                let title = state.new_sub.get_untracked().trim().to_string();
                if !title.is_empty() {
                    shared::change(|t, now| {
                        let child = t.create(&title, now);
                        t.update(child, now, |i| { i.parent = Some(number); i.status = Status::Todo; });
                    });
                    state.new_sub.set(String::new());
                }
            }), None)])),
    )
    .into_element()
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;
    use crate::issues::shared::testing::{Repo, turn};

    fn start() -> Headless {
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Issues>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(1000.), Points(800.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app
    }

    /// Shows `repo` on the page and holds it among the projects followed.
    fn show(repo: &Repo) {
        shared::set_project(&repo.0);
        shared::track(vec![repo.0.clone()]);
        shared::poll();
    }

    fn issue(number: u64) -> Issue {
        shared::watch().borrow().tracker.issue(number).cloned().expect("the issue")
    }

    #[test]
    fn a_card_dragged_across_the_board_lands_in_the_column_it_ends_over() {
        // Five columns over 500 points: 100 each.
        assert_eq!(column_after(Status::Todo, 0., 500.), Status::Todo);
        assert_eq!(column_after(Status::Todo, 120., 500.), Status::InProgress);
        assert_eq!(column_after(Status::Todo, 40., 500.), Status::Todo);
        assert_eq!(column_after(Status::Todo, 60., 500.), Status::InProgress);
        assert_eq!(column_after(Status::Todo, -400., 500.), Status::Backlog);
        assert_eq!(column_after(Status::InReview, 900., 500.), Status::Done);
        assert_eq!(column_after(Status::Todo, 300., 0.), Status::Todo, "an unmeasured board moves nothing");
    }

    /// The one test of the screens: the issues are the shared store's, so its steps run in order.
    #[test]
    fn the_page_adds_moves_edits_and_groups_issues_and_epics() {
        let _turn = turn();
        let mut commands = super::super::bus::lock();
        let commands = commands.as_mut().expect("the channel");
        let repo = Repo::new("pg");
        show(&repo);
        let mut app = start();
        assert!(app.shows("No issues yet. Add one above."));

        // Quick add: tags in the line set the priority; the due date is read as people say it.
        app.type_text("id:issue-new-title", "Fix login !high").unwrap();
        // The due date is a pill that opens its field; Enter in it closes it.
        assert!(app.shows("Due"), "{:?}", app.screen().texts());
        app.press("role:button[When the issue is due]").unwrap();
        app.settle();
        app.type_text("id:issue-new-due", "tomorrow").unwrap();
        app.submit("id:issue-new-due").unwrap();
        app.settle();
        assert!(app.shows("Tomorrow"), "the pill says what was read: {:?}", app.screen().texts());
        app.submit("id:issue-new-title").unwrap();
        app.settle();
        let first = issue(1);
        assert_eq!((first.title.as_str(), first.priority, first.status, first.due), ("Fix login", Priority::High, Status::Todo, Some(today().plus(1))));
        assert!(app.shows("Fix login") && app.shows("PG-1"));

        // The circle moves it along.
        app.press("role:button[Status: Todo]").unwrap();
        app.settle();
        assert_eq!(issue(1).status, Status::InProgress);

        // The detail page: pick a status, edit the title and description, go back.
        app.press("id:issue-pg-1").unwrap();
        app.settle();
        assert!(app.shows("Add sub-issue"));
        app.press("role:button[In Review]").unwrap();
        app.settle();
        assert_eq!(issue(1).status, Status::InReview);
        // Title and description read as text until pressed twice.
        app.press("id:Title-shown").unwrap();
        app.press("id:Title-shown").unwrap();
        app.clear("id:issue-title").unwrap();
        app.type_text("id:issue-title", "Fix the login").unwrap();
        app.press("id:Description-shown").unwrap();
        app.press("id:Description-shown").unwrap();
        app.type_text("id:issue-description", "Users can't sign in.").unwrap();
        app.type_text("id:issue-label", "bug").unwrap();
        app.submit("id:issue-label").unwrap();
        app.type_text("id:issue-sub", "Add a test").unwrap();
        app.submit("id:issue-sub").unwrap();
        app.settle();
        assert_eq!(issue(2).parent, Some(1));
        // Fields are saved once typing pauses: nothing waits for Back.
        app.wait(std::time::Duration::from_millis(400)).unwrap();
        app.settle();
        assert_eq!((issue(1).title.as_str(), issue(1).description.as_str()), ("Fix the login", "Users can't sign in."));
        // An agent adds a note while the page is open; going back with the fields untouched
        // since keeps it.
        shared::change(|tracker, now| tracker.update(1, now, |issue| issue.description.push_str(" Agent: found the cause.")));
        app.press("role:button[Back (Esc)]").unwrap();
        app.settle();
        let first = issue(1);
        assert_eq!((first.title.as_str(), first.description.as_str(), first.labels.as_slice()), ("Fix the login", "Users can't sign in. Agent: found the cause.", ["bug".to_string()].as_slice()));
        assert!(app.shows("PG-1") && app.shows("0/1"), "the parent counts its sub-issue");

        // Deleting takes two presses, and goes back to the list.
        app.press("id:issue-pg-2").unwrap();
        app.settle();
        app.press("role:button[Delete issue]").unwrap();
        app.settle();
        assert!(shared::watch().borrow().tracker.issue(2).is_some());
        assert!(app.shows("Press again to delete"));
        app.press("role:button[Delete issue]").unwrap();
        app.settle();
        assert!(shared::watch().borrow().tracker.issue(2).is_none());
        assert!(app.shows("Fix the login") && !app.shows("Press again to delete"), "back at the list");

        // Filtering by words narrows the list.
        app.type_text("id:issue-search", "nothing like it").unwrap();
        app.settle();
        assert!(app.shows("Nothing matches."));
        app.press_key("escape").unwrap();
        app.settle();
        assert!(app.shows("Fix the login"));

        // The board has a column for each status; the arrows move a card.
        app.press("role:button[Board]").unwrap();
        app.settle();
        assert_eq!(app.screen().count("id:column-in-review").unwrap(), 1);
        app.hover("id:card-pg-1").unwrap();
        app.press("id:card-pg-1 >> label:Move right").unwrap();
        app.settle();
        assert_eq!(issue(1).status, Status::Done);
        // Dragging it back a column, with the pointer, moves it too.
        app.drag("id:card-pg-1", Point::new(Points(-190.), Points(0.))).unwrap();
        app.settle();
        assert_eq!(issue(1).status, Status::InReview);

        // Epics: make one, give it a milestone, put an issue in it.
        app.press("role:button[Epics]").unwrap();
        app.settle();
        app.type_text("id:epic-new-name", "Launch").unwrap();
        app.submit("id:epic-new-name").unwrap();
        app.settle();
        assert_eq!(shared::watch().borrow().tracker.epics().len(), 1);
        app.type_text("id:milestone-name", "Beta").unwrap();
        app.type_text("id:milestone-due", "2026-12-01").unwrap();
        app.submit("id:milestone-name").unwrap();
        app.settle();
        assert_eq!(shared::watch().borrow().tracker.epic("launch").unwrap().milestones.len(), 1);
        app.type_text("id:epic-issue-title", "Write the notes").unwrap();
        app.submit("id:epic-issue-title").unwrap();
        app.settle();
        assert_eq!(issue(3).epic.as_deref(), Some("launch"));
        assert!(app.shows("0/1"), "the epic counts its issue");

        // An agent can put the page somewhere: a view, an issue, an epic, and a filter.
        let nav = |target, filter, group| shared::navigate(shared::Nav { target, filter, group });
        nav(shared::Target::Board, None, None);
        app.settle();
        assert_eq!(app.screen().count("id:column-todo").unwrap(), 1);
        nav(shared::Target::Issue(1), None, None);
        app.settle();
        assert!(app.shows("Add sub-issue"));
        nav(shared::Target::Epic("launch".into()), None, None);
        app.settle();
        assert!(app.shows("Milestones"));
        nav(shared::Target::List, Some(Filter { text: "zzz".into(), ..Default::default() }), Some(Group::Priority));
        app.settle();
        assert!(app.shows("Nothing matches."));
        nav(shared::Target::List, Some(Filter::default()), None);
        app.settle();
        assert!(app.shows("Fix the login"));

        // Typing and leaving at once does not lose what was typed.
        app.press("id:issue-pg-1").unwrap();
        app.settle();
        app.press("id:Title-shown").unwrap();
        app.press("id:Title-shown").unwrap();
        app.type_text("id:issue-title", " now").unwrap();
        app.press("role:button[Back (Esc)]").unwrap();
        app.settle();
        assert_eq!(issue(1).title, "Fix the login now");

        // With nothing open, Esc asks the frame to close.
        app.press_key("escape").unwrap();
        app.settle();
        app.press_key("escape").unwrap();
        app.settle();
        assert_eq!(commands.try_recv().ok(), Some(Cmd::CloseIssues));
    }

    #[test]
    fn typing_goes_to_the_repo_it_was_typed_for_when_the_page_moves_to_another() {
        let _turn = turn();
        let _commands = super::super::bus::lock();
        let (one, two) = (Repo::new("pg-one"), Repo::new("pg-two"));
        for (repo, title) in [(&one, "Of the first"), (&two, "Of the second")] {
            shared::change_repo(&repo.0, |t, now| t.create(title, now));
        }
        let nav = |number| shared::navigate(shared::Nav { target: shared::Target::Issue(number), filter: None, group: None });
        shared::set_project(&one.0);
        nav(1);
        let mut app = start();
        app.settle();
        assert!(app.screen().texts().iter().any(|t| t.to_string().contains("pg-one")), "{:?}", app.screen().texts());

        // Issue 1 of the other repo, pressed in the sidebar before the typing was saved.
        app.press("id:Title-shown").unwrap();
        app.press("id:Title-shown").unwrap();
        app.type_text("id:issue-title", " typed").unwrap();
        shared::set_project(&two.0);
        nav(1);
        app.settle();
        assert!(app.screen().texts().iter().any(|t| t.to_string().contains("pg-two")), "{:?}", app.screen().texts());
        assert_eq!(issue(1).title, "Of the second", "untouched");
        assert_eq!(shared::change_repo(&one.0, |t, _| t.issue(1).unwrap().title.clone()), "Of the first typed");
    }

    #[test]
    fn the_page_is_one_list_board_and_epics_for_every_open_project_that_a_filter_narrows() {
        let _turn = turn();
        let _commands = super::super::bus::lock();
        let (one, two) = (Repo::new("all-one"), Repo::new("all-two"));
        shared::change_repo(&one.0, |t, now| t.create("Mend the door", now));
        shared::change_repo(&two.0, |t, now| {
            t.create("Paint the wall", now);
            t.create_epic("Launch", now);
        });
        // The second was never shown, only followed.
        show(&one);
        shared::track(vec![one.0.clone(), two.0.clone()]);
        shared::poll();
        let mut app = start();
        app.settle();
        for shown in ["Mend the door", "Paint the wall", "all-one", "all-two"] {
            assert!(app.shows(shown), "{shown}: {:?}", app.screen().texts());
        }

        // With no project chosen, the pill in the add box says where a new issue goes.
        app.press("role:button[Which project the issue goes to]").unwrap();
        app.settle();
        app.press("role:menuitem[all-two]").unwrap();
        app.settle();
        // A quick pick sets the due date, which the pill says; its cross takes it off again.
        app.press("role:button[When the issue is due]").unwrap();
        app.settle();
        app.press("role:button[Today]").unwrap();
        app.settle();
        assert!(app.shows("Today"), "{:?}", app.screen().texts());
        app.press("role:button[Clear the due date]").unwrap();
        app.settle();
        assert!(!app.shows("Today") && app.shows("Due"), "{:?}", app.screen().texts());
        app.type_text("id:issue-new-title", "Fix the hinge").unwrap();
        app.submit("id:issue-new-title").unwrap();
        app.settle();
        assert_eq!(shared::change_repo(&two.0, |t, _| t.issues().count()), 2);

        // The words narrow it; so does a project.
        app.type_text("id:issue-search", "paint").unwrap();
        app.settle();
        assert!(app.shows("Paint the wall") && !app.shows("Mend the door"), "{:?}", app.screen().texts());
        app.clear("id:issue-search").unwrap();
        app.settle();
        app.press("role:button[Filter by project]").unwrap();
        app.settle();
        app.press("role:menuitem[all-one]").unwrap();
        app.settle();
        app.press_key("escape").unwrap();
        app.settle();
        assert!(app.shows("Mend the door") && !app.shows("Paint the wall"), "{:?}", app.screen().texts());
        // With one project chosen a new issue goes to it, and its key is that repo's.
        app.type_text("id:issue-new-title", "Hang a picture").unwrap();
        app.submit("id:issue-new-title").unwrap();
        app.settle();
        assert_eq!(shared::change_repo(&one.0, |t, _| t.issues().count()), 2);
        assert_eq!(shared::change_repo(&two.0, |t, _| t.issues().count()), 2);
        assert!(app.shows("all-one"), "the pill shows the one it is filtered to");
        app.press("role:button[Remove project all-one]").unwrap();
        app.settle();

        // The other menus of the toolbar: how far along, what is less used, and the grouping.
        app.press("role:button[Which issues: open, closed or all]").unwrap();
        app.settle();
        app.press("role:menuitem[Closed]").unwrap();
        app.settle();
        assert!(app.shows("Nothing matches."), "{:?}", app.screen().texts());
        app.press("role:button[Which issues: open, closed or all]").unwrap();
        app.settle();
        app.press("role:menuitem[Open]").unwrap();
        app.settle();
        app.press("role:button[More filters: priority, labels, late]").unwrap();
        app.settle();
        app.press("role:menuitem[Late only]").unwrap();
        app.settle();
        app.press_key("escape").unwrap();
        app.settle();
        assert!(app.shows("Nothing matches.") && app.shows("Late"), "{:?}", app.screen().texts());
        app.press("role:button[Remove late]").unwrap();
        app.settle();
        app.press("role:button[How the list is grouped]").unwrap();
        app.settle();
        app.press("role:menuitem[Priority]").unwrap();
        app.settle();
        assert!(app.shows("No priority") && app.shows("Mend the door"), "{:?}", app.screen().texts());

        // The board is every project's, a card in the column of its status, and has its own toolbar.
        app.press("role:button[Board]").unwrap();
        app.settle();
        assert!(app.shows("More") && app.shows("All projects"), "{:?}", app.screen().texts());
        assert!(app.shows("Paint the wall") && app.shows("Mend the door"), "{:?}", app.screen().texts());
        app.hover("id:card-all-two-1").unwrap();
        app.press("id:card-all-two-1 >> label:Move right").unwrap();
        app.settle();
        assert_eq!(shared::change_repo(&two.0, |t, _| t.issue(1).unwrap().status), Status::Todo);

        // So are the epics, under the name of their project.
        app.press("role:button[Epics]").unwrap();
        app.settle();
        assert!(app.shows("Launch") && app.shows("all-two"), "{:?}", app.screen().texts());
        app.press("id:epic-all-two-launch").unwrap();
        app.settle();
        assert!(app.shows("Milestones"), "{:?}", app.screen().texts());
        assert_eq!(shared::watch().borrow().name, "all-two");
        app.press("role:button[Back (Esc)]").unwrap();
        app.settle();

        // An issue opens in its repo; Back is the list it came from.
        app.press("role:button[List]").unwrap();
        app.settle();
        app.press("id:issue-all-two-1").unwrap();
        app.settle();
        assert!(app.shows("Add sub-issue"), "{:?}", app.screen().texts());
        assert_eq!(shared::watch().borrow().name, "all-two");
        app.press("role:button[Back (Esc)]").unwrap();
        app.settle();
        assert!(app.shows("Mend the door") && !app.shows("Add sub-issue"), "{:?}", app.screen().texts());
        shared::track(vec![]);
        shared::poll();
    }
}
