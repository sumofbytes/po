//! The issues page: the repo's issues as a list or a board, one issue's details, and the epics
//! they belong to. The data is [`crate::issues::shared`]'s; what is pressed changes it there, and
//! the page follows. [`crate::nsh::epics`] draws the epics.
use nsh_core::prelude::*;

use super::bus::{Cmd, send};
use super::overlay::{self, Height};
use super::{Colors, icon, icon_button, on_hover, rems, text_sm, text_xs};
use crate::issues::query::{self, Filter, Group, parse_quick};
use crate::issues::shared::{self, View as Data};
use crate::issues::{Issue, Link, Priority, Status};
use crate::date::{due_label, parse_due, today};
use crate::ui::icon::IconName;

/// The page fills the space it is given, so the height it measures is not used.
pub static HEIGHT: Height = overlay::height();

const DUE_HINT: &str = "Due: tomorrow, fri, nov 2…";

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
        }
    }

    /// What `f` makes of the issues as they are now, without following them.
    pub fn read<R>(self, f: impl FnOnce(&Data) -> R) -> R {
        self.data.with_untracked(f)
    }

    /// Adds what the add field holds as an issue, with what is tagged onto it, and leaves the
    /// field empty and focused for the next one. `epic` puts it in an epic whatever the line says.
    pub fn add(self, epic: Option<String>) {
        let line = self.new_title.get_untracked();
        let Some(due) = parse_due(&self.new_due.get_untracked(), today()) else { return };
        let quick = self.read(|data| parse_quick(&line, &data.tracker));
        if quick.title.is_empty() {
            return;
        }
        shared::change(|tracker, now| {
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
        self.page.set(Page::Issue(number));
    }

    pub fn go(self, page: Page) {
        self.flush();
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
            shared::change(|tracker, now| tracker.update(number, now, |issue| issue.title = title));
        }
    }

    pub fn save_description(self, number: u64) {
        let description = self.description.get_untracked();
        shared::change(|tracker, now| tracker.update(number, now, |issue| issue.description = description));
    }

    pub fn save_due(self, number: u64) {
        if let Some(due) = parse_due(&self.due.get_untracked(), today()) {
            shared::change(|tracker, now| tracker.update(number, now, |issue| issue.due = due));
        }
    }

    /// Empty clears the parent; a key of an issue there is sets it. Anything else waits.
    pub fn save_parent(self, number: u64) {
        let text = self.parent.get_untracked();
        let parent = self.read(|data| if text.trim().is_empty() { Some(None) } else { data.tracker.parse_key(&text).filter(|p| data.tracker.issue(*p).is_some()).map(Some) });
        if let Some(parent) = parent {
            shared::change(|tracker, now| tracker.update(number, now, |issue| issue.parent = parent));
        }
    }

    pub fn set_status(self, number: u64, status: Status) {
        shared::change(|tracker, now| tracker.update(number, now, |issue| issue.status = status));
    }

    /// The next status along, for a click on the circle: Done wraps to Backlog; Canceled is only picked.
    pub fn advance(self, number: u64) {
        let next = self.read(|data| data.tracker.issue(number).map(|issue| match issue.status {
            Status::Backlog => Status::Todo,
            Status::Todo => Status::InProgress,
            Status::InProgress => Status::InReview,
            Status::InReview => Status::Done,
            Status::Done | Status::Canceled => Status::Backlog,
        }));
        if let Some(next) = next {
            self.set_status(number, next);
        }
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

use super::epics::{epic_page, epics_page};

#[component]
pub fn Issues() -> impl IntoElement {
    let state = State::new();
    spawn(async move {
        let mut changes = shared::watch();
        while changes.changed().await.is_ok() {
            let now = changes.borrow_and_update().clone();
            state.data.set(now);
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
    view()
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .px(rems(0.5))
        .py(rems(0.125))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::border())
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
    view()
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_none()
        .flex_row()
        .items_center()
        .gap(rems(0.25))
        .px(rems(0.5))
        .py(rems(0.125))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::border())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .style_when(move || on(), |s| s.bg(Colors::selected()))
        .on_press(on_press)
        .child(icon(name, rems(0.75), move || tint))
        .child(text(label).text_size(text_xs()).text_color(move || if shown() { Colors::foreground()() } else { Colors::muted()() }))
}

/// A line of text, filling its row, that reports Enter.
pub fn line_input(id: &str, value: Signal<String>, placeholder: &'static str, on_submit: impl Fn() + 'static) -> nsh_base::Input {
    super::text_input(id, value, placeholder, on_submit).min_w(Points(0.))
}

/// A button that deletes on the second press: the first changes its label to ask.
pub fn delete_button(state: State, label: &'static str, on_press: impl Fn() + 'static) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(label)
        .flex_none()
        .px(rems(0.5))
        .py(rems(0.25))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(on_press)
        .child(
            text(move || if state.arm_delete.get() { "Press again to delete".to_string() } else { "Delete".to_string() })
                .text_size(text_sm())
                .text_color(move || if state.arm_delete.get() { Colors::danger()() } else { Colors::muted()() }),
        )
}

/// Several lines of text, growing to a height, where Enter makes a new line. `on_change` hears
/// every edit and is to set `value`.
pub fn text_area(id: &str, value: Signal<String>, placeholder: &'static str, on_change: impl Fn(&str) + 'static) -> View {
    view()
        .px(rems(0.625))
        .py(rems(0.5))
        .min_h(rems(5.))
        .rounded(Colors::radius())
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
                .max_h(rems(24.)),
        )
}

/// A boxed single line with a lead icon, like the add field.
pub fn boxed(lead: IconName, input: nsh_base::Input, hint: Option<View>) -> View {
    view()
        .min_h(rems(2.5))
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .px(rems(0.625))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .child(icon(lead, rems(0.875), Colors::muted()))
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

/// A small muted caption for a group of controls.
pub fn caption(label: &'static str) -> Text {
    text(label).text_size(text_xs()).font_weight(FontWeight::MEDIUM).text_color(Colors::muted())
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
            .gap(rems(0.375))
            .px(rems(0.5))
            .py(rems(0.25))
            .rounded(Colors::radius())
            .cursor_pointer()
            .hover(|s| s.bg(Colors::hover()))
            .style_when(move || state.page.with(|now| on(now)), |s| s.bg(Colors::selected()))
            .on_press(move || state.go(target.clone()))
            .child(icon(name, rems(0.875), Colors::muted()))
            .child(text(label).text_size(text_sm()).text_color(move || if state.page.with(|now| shown(now)) { Colors::foreground()() } else { Colors::muted()() }))
    };
    let name = move || state.data.with(|data| if data.name.is_empty() { "Issues".to_string() } else { format!("{} issues", data.name) });
    view()
        .flex_row()
        .items_center()
        .gap(rems(0.25))
        .px(rems(1.))
        .py(rems(0.5))
        .border_b_1()
        .border_color(Colors::border())
        .child(text(name).number_of_lines(1).font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground()).mr(rems(0.75)))
        .child(tab("List", IconName::List, Page::List, |p| matches!(p, Page::Issue(_))))
        .child(tab("Board", IconName::SquareKanban, Page::Board, |_| false))
        .child(tab("Epics", IconName::Layers, Page::Epics, |p| matches!(p, Page::Epic(_))))
        .child(view().flex_1())
        .child(icon_button(IconName::Close, rems(0.875), "Close (Esc)", || send(Cmd::CloseIssues)))
}

/// What was wrong with the files in `.po` when they were read: one that can't be read is left
/// as it is and not shown, so the page says so rather than letting an issue quietly go missing.
fn problems_banner(state: State) -> View {
    let problems = move || state.data.with(|data| data.problems.len());
    let summary = move || {
        state.data.with(|data| {
            let first = data.problems.first().cloned().unwrap_or_default();
            match data.problems.len() {
                1 => format!("A problem with the files in .po: {first}"),
                n => format!("{n} problems with the files in .po; the first: {first}"),
            }
        })
    };
    view().child(when(move || problems() > 0, move || {
        view()
            .px(rems(1.))
            .py(rems(0.375))
            .bg(Colors::input_background())
            .border_b_1()
            .border_color(Colors::border())
            .child(text(summary).number_of_lines(2).text_size(text_xs()).text_color(Colors::warning()))
    }).otherwise(|| {}))
}

fn no_project() -> View {
    view().py(rems(1.5)).items_center().child(text("Open a project to see its issues.").text_size(text_sm()).text_color(Colors::muted()))
}

// ---------------------------------------------------------------------------------------------
// The list.

fn list_page(state: State) -> View {
    let has_project = move || state.data.with(|data| data.root.is_some());
    view()
        .flex_1()
        .child(when(has_project, move || {
            view()
                .flex_1()
                .child(
                    view().px(rems(1.)).pt(rems(0.75)).pb(rems(0.5)).child(boxed(
                        IconName::Plus,
                        line_input("issue-new-title", state.new_title, "New issue — try: Fix login !high #bug @epic", move || state.add(None)).auto_focus(true).element_ref(&state.add_field),
                        Some(
                            view()
                                .flex_none()
                                .w(rems(13.))
                                .flex_row()
                                .items_center()
                                .gap(rems(0.5))
                                .child(icon(IconName::Calendar, rems(0.875), Colors::muted()))
                                .child(line_input("issue-new-due", state.new_due, DUE_HINT, move || state.add(None)))
                                .child(due_hint(state.new_due)),
                        ),
                    )),
                )
                .child(todo_offer(state))
                .child(filter_bar(state))
                .child(scroll().flex_shrink(1.).child(dynamic(move || rows(state))))
        })
        .otherwise(no_project))
}

/// The old todo list, offered once, for this project to take: Po's todo page is gone, and the
/// list it kept is still in `todos.json`. Taking it or dismissing it ends the offer everywhere.
fn todo_offer(state: State) -> View {
    let offered = move || state.old_todos.get() > 0;
    view().child(when(offered, move || {
        view()
            .flex_row()
            .items_center()
            .gap(rems(0.75))
            .mx(rems(1.))
            .mb(rems(0.5))
            .px(rems(0.75))
            .py(rems(0.5))
            .rounded(Colors::radius())
            .border_1()
            .border_color(Colors::border())
            .child(text(move || format!("Your old todo list has {} items. Bring them in as issues here?", state.old_todos.get())).flex_1().text_size(text_sm()).text_color(Colors::foreground()))
            .child(super::button("Import here", true, move || {
                shared::change(|tracker, now| crate::issues::import::from_items(&crate::issues::import::read(&crate::issues::import::file()), tracker, now));
                crate::issues::import::handled("imported");
                state.old_todos.set(0);
            }))
            .child(super::button("Dismiss", false, move || {
                crate::issues::import::handled("dismissed");
                state.old_todos.set(0);
            }))
    }).otherwise(|| {}))
}

fn filter_bar(state: State) -> View {
    let scope = move |label: &'static str, scope: query::Scope| chip(label, move || state.filter.with(|f| f.scope == scope), move || state.filter.update(|f| f.scope = scope));
    let group = move |group: Group| chip(group.name(), move || state.group.get() == group, move || state.group.set(group));
    let labels = move || {
        let labels: Vec<String> = state.data.with(|data| data.tracker.config.labels.iter().map(|l| l.name.clone()).collect());
        view().flex_row().flex_wrap().gap(rems(0.25)).child(
            labels
                .into_iter()
                .map(|name| {
                    let (on, set) = (name.clone(), name.clone());
                    chip(format!("#{name}"), move || state.filter.with(|f| f.label.as_ref() == Some(&on)), move || state.filter.update(|f| f.label = if f.label.as_ref() == Some(&set) { None } else { Some(set.clone()) }))
                })
                .collect::<Vec<_>>(),
        )
    };
    view()
        .px(rems(1.))
        .pb(rems(0.5))
        .gap(rems(0.375))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(rems(0.5))
                .child(
                    view()
                        .flex_1()
                        .child(boxed(IconName::Search, line_input("issue-search", state.search, "Filter", || {}).on_change(move |text: &str| {
                            state.search.set(text.to_string());
                            state.filter.update(|f| f.text = text.to_string());
                        }), None)),
                )
                .child(scope("Open", query::Scope::Open))
                .child(scope("All", query::Scope::All))
                .child(scope("Closed", query::Scope::Closed))
                .child(chip("Late", move || state.filter.with(|f| f.late), move || state.filter.update(|f| f.late = !f.late))),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .flex_wrap()
                .gap(rems(0.25))
                .child(caption("Group").mr(rems(0.25)))
                .child(Group::ALL.map(group).to_vec())
                .child(view().w(rems(0.75)))
                .child(dynamic(labels)),
        )
}

fn rows(state: State) -> View {
    let (sections, empty) = state.data.with(|data| {
        let filter = state.filter.get();
        let sections = query::sections(&data.tracker, &filter, state.group.get(), today());
        (sections, data.tracker.issues().next().is_none())
    });
    let mut out = vec![];
    for section in sections {
        if !section.title.is_empty() {
            out.push(
                view()
                    .flex_row()
                    .items_baseline()
                    .gap(rems(0.5))
                    .mt(rems(0.75))
                    .px(rems(0.625))
                    .child(text(section.title.clone()).font_weight(FontWeight::SEMIBOLD).text_size(text_sm()).text_color(Colors::foreground()))
                    .child(text(section.issues.len().to_string()).text_size(text_xs()).text_color(Colors::muted())),
            );
        }
        out.extend(section.issues.iter().map(|number| issue_row(state, *number)));
    }
    if out.is_empty() {
        let message = if empty { "No issues yet. Add one above." } else { "Nothing matches." };
        out.push(view().py(rems(1.5)).items_center().child(text(message).text_size(text_sm()).text_color(Colors::muted())));
    }
    view().gap(rems(0.125)).px(rems(0.75)).pb(rems(0.75)).child(out)
}

pub fn issue_row(state: State, number: u64) -> View {
    let Some((issue, key, progress, parent)) = state.read(|data| {
        let issue = data.tracker.issue(number)?.clone();
        Some((issue.clone(), data.tracker.key(number), data.tracker.progress(number), issue.parent.map(|p| data.tracker.key(p))))
    }) else {
        return view();
    };
    let closed = !issue.status.is_open();
    let (status, priority) = (issue.status, issue.priority);
    let due = issue.due.map(|due| (due, today()));
    let due_tint = match due {
        Some((due, today)) if !closed && due < today => Colors::danger()(),
        Some((due, today)) if !closed && due == today => Colors::warning()(),
        _ => Colors::muted()(),
    };
    view()
        .group()
        .test_id(&format!("issue-{number}"))
        .h(rems(2.))
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .px(rems(0.625))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || state.open_issue(number))
        .child(view().role(AccessibilityRole::Button).label(format!("Status: {}", status.name())).flex_none().on_press(move || state.advance(number)).child(icon(status_icon(status), rems(0.875), move || status_tint(status))))
        .child(icon(IconName::Signal, rems(0.75), move || priority_tint(priority)))
        .child(text(key).flex_none().text_size(text_xs()).text_color(Colors::muted()))
        .child(text(issue.title.clone()).flex_1().number_of_lines(1).style_when(closed, |s| s.line_through()).text_color(if closed { Colors::muted()() } else { Colors::foreground()() }))
        .child(parent.map(|parent| text(format!("↳ {parent}")).flex_none().text_size(text_xs()).text_color(Colors::muted())))
        .child((progress.all > 0).then(|| text(format!("{}/{}", progress.done, progress.all)).flex_none().text_size(text_xs()).text_color(Colors::muted())))
        .child(issue.labels.iter().map(|label| text(format!("#{label}")).flex_none().text_size(text_xs()).text_color(Colors::muted())).collect::<Vec<_>>())
        .child(text(due.map(|(due, today)| due_label(due, today)).unwrap_or_default()).flex_none().text_size(text_xs()).text_color(move || due_tint))
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
    view().flex_1().child(
        scroll().flex_shrink(1.).child(
            view()
                .flex_row()
                .gap(rems(0.5))
                .px(rems(0.75))
                .py(rems(0.75))
                .on_layout(move |bounds: &Bounds<Points>| state.board_width.set(bounds.size.width.0))
                .child(dynamic(move || {
                    let filter = Filter { scope: query::Scope::All, ..state.filter.get() };
                    let columns: Vec<View> = state.data.with(|data| {
                        let today = today();
                        COLUMNS
                            .iter()
                            .map(|status| {
                                let mut cards: Vec<&Issue> = data.tracker.issues().filter(|i| i.status == *status && filter.matches(i, &data.tracker.key(i.number), today)).collect();
                                cards.sort_by_key(|i| (i.priority.rank(), std::cmp::Reverse(i.number)));
                                let count = cards.len();
                                let cards: Vec<View> = cards.into_iter().map(|issue| card(state, issue, data.tracker.key(issue.number))).collect();
                                column(*status, count, cards)
                            })
                            .collect()
                    });
                    view().flex_1().flex_row().gap(rems(0.5)).child(columns)
                })),
        ),
    )
}

fn column(status: Status, count: usize, cards: Vec<View>) -> View {
    view()
        .test_id(&format!("column-{}", status.slug()))
        .flex_1()
        .min_w(Points(0.))
        .gap(rems(0.375))
        .p(rems(0.375))
        .rounded(Colors::radius())
        .bg(Colors::input_background())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(rems(0.375))
                .px(rems(0.25))
                .child(icon(status_icon(status), rems(0.75), move || status_tint(status)))
                .child(text(status.name()).text_size(text_xs()).font_weight(FontWeight::SEMIBOLD).number_of_lines(1).text_color(Colors::foreground()))
                .child(text(count.to_string()).text_size(text_xs()).text_color(Colors::muted())),
        )
        .child(cards)
}

fn card(state: State, issue: &Issue, key: String) -> View {
    let (number, status, priority) = (issue.number, issue.status, issue.priority);
    let left = COLUMNS.iter().position(|s| *s == status).and_then(|at| at.checked_sub(1)).map(|at| COLUMNS[at]);
    let right = COLUMNS.iter().position(|s| *s == status).and_then(|at| COLUMNS.get(at + 1)).copied();
    view()
        .group()
        .test_id(&format!("card-{number}"))
        .gap(rems(0.25))
        .p(rems(0.5))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::border())
        .bg(Colors::popover())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || state.open_issue(number))
        // Dragging a card across the columns moves it to the one it ends over.
        .on_pan_end(move |event: &PanEvent| {
            if event.cancelled {
                return;
            }
            let to = column_after(status, event.translation.x.0, state.board_width.get_untracked());
            if to != status {
                state.set_status(number, to);
            }
        })
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(rems(0.375))
                .child(icon(IconName::Signal, rems(0.625), move || priority_tint(priority)))
                .child(text(key).flex_1().text_size(text_xs()).text_color(Colors::muted()))
                .child(left.map(|to| on_hover(icon_button(IconName::ChevronLeft, rems(0.625), "Move left", move || state.set_status(number, to)))))
                .child(right.map(|to| on_hover(icon_button(IconName::ChevronRight, rems(0.625), "Move right", move || state.set_status(number, to))))),
        )
        .child(text(issue.title.clone()).text_size(text_sm()).text_color(Colors::foreground()))
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
        view().p(rems(1.5)).gap(rems(0.75)).items_center().child(text("This issue is gone.").text_color(Colors::muted())).child(super::button("Back", false, move || state.back()))
    }))
}

/// A labelled row of properties.
fn property(label: &'static str, body: Vec<View>) -> View {
    view().flex_row().items_start().gap(rems(0.75)).child(view().w(rems(5.5)).flex_none().pt(rems(0.25)).child(caption(label))).child(view().flex_1().flex_row().flex_wrap().items_center().gap(rems(0.25)).child(body))
}

fn issue_body(state: State, number: u64) -> AnyElement {
    let key = state.read(|data| data.tracker.key(number));
    let sections = move || {
        let Some(issue) = state.read(|data| data.tracker.issue(number).cloned()) else { return view() };
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
        let (labels, epics, milestones, links, subs) = state.read(|data| {
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
                        .h(rems(1.75))
                        .flex_row()
                        .items_center()
                        .gap(rems(0.5))
                        .px(rems(0.5))
                        .rounded(Colors::radius())
                        .cursor_pointer()
                        .hover(|st| st.bg(Colors::hover()))
                        .on_press(move || state.open_issue(n))
                        .child(icon(status_icon(s), rems(0.75), move || status_tint(s)))
                        .child(text(data.tracker.key(n)).flex_none().text_size(text_xs()).text_color(Colors::muted()))
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
                    view().flex_row().items_center().gap(rems(0.375)).child(icon(name, rems(0.75), Colors::muted())).child(text(label).text_size(text_xs()).text_color(Colors::muted()))
                })
                .collect();
            (labels, epics, milestones, links, subs)
        });
        view()
            .gap(rems(0.75))
            .child(property("Status", statuses))
            .child(property("Priority", priorities))
            .child(property("Labels", labels))
            .child(property("Epic", epics))
            .child((!milestones.is_empty()).then(|| property("Milestone", milestones)))
            .child((!links.is_empty()).then(|| property("Links", links)))
            .child(property("Sub-issues", vec![view().w_full().gap(rems(0.125)).child(subs)]))
    };
    scroll().flex_shrink(1.).child(
        view()
            .gap(rems(0.75))
            .px(rems(1.))
            .py(rems(0.75))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(rems(0.5))
                    .child(icon_button(IconName::ChevronLeft, rems(0.875), "Back (Esc)", move || state.back()))
                    .child(text(key).flex_1().text_size(text_sm()).text_color(Colors::muted()))
                    .child(super::button("Work on this", true, move || send(Cmd::WorkOnIssue(number))))
                    .child(delete_button(state, "Delete issue", move || state.delete(number))),
            )
            .child(boxed(IconName::Pencil, line_input("issue-title", state.title, "Title", move || state.flush()).on_change(move |text: &str| {
                state.title.set(text.to_string());
                state.touch(|edited| edited.title = true);
            }), None))
            .child(text_area("issue-description", state.description, "Description (markdown)", move |text| {
                state.description.set(text.to_string());
                state.touch(|edited| edited.description = true);
            }))
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
        shared::set_project(&repo.0);
        let mut app = start();
        assert!(app.shows("No issues yet. Add one above."));

        // Quick add: tags in the line set the priority; the due date is read as people say it.
        app.type_text("id:issue-new-title", "Fix login !high").unwrap();
        app.type_text("id:issue-new-due", "tomorrow").unwrap();
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
        app.press("id:issue-1").unwrap();
        app.settle();
        assert!(app.shows("Add sub-issue"));
        app.press("role:button[In Review]").unwrap();
        app.settle();
        assert_eq!(issue(1).status, Status::InReview);
        app.clear("id:issue-title").unwrap();
        app.type_text("id:issue-title", "Fix the login").unwrap();
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
        app.press("id:issue-2").unwrap();
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
        app.hover("id:card-1").unwrap();
        app.press("id:card-1 >> label:Move right").unwrap();
        app.settle();
        assert_eq!(issue(1).status, Status::Done);
        // Dragging it back a column, with the pointer, moves it too.
        app.drag("id:card-1", Point::new(Points(-190.), Points(0.))).unwrap();
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
        app.press("id:issue-1").unwrap();
        app.settle();
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
}
