//! The todos window's screen, laid out like a plain todo app: a field to add one at the top, the
//! list with a box to check each off, and a footer with what is left and which to show. The list
//! itself is [`crate::todo`]'s store; [`crate::todo::TodoPanel`] is the frame Po shows this in.
use nsh_core::prelude::*;

use super::{Colors, icon, rems, text_sm, text_xs, tooltip};
use crate::todo::{self, CLOSE, Date, HEIGHT, Item, Show, Target, due_label, parse_due, today};
use crate::ui::icon::IconName;

const DUE_HINT: &str = "Due: tomorrow, fri, nov 2…";

/// A row of the list: roomier than the sidebar's, as the window has the space.
fn row_height() -> Points {
    rems(2.)
}

/// How far a subtask sits in from its parent: the lead column and the gap after it, so its box
/// lines up with the parent's text.
fn indent(depth: usize) -> Points {
    rems(1.5 * depth as f32)
}

/// The lead column's width, where boxes and chevrons go.
fn lead() -> Points {
    rems(1.)
}

/// What a row shows of its item.
#[derive(Clone, PartialEq)]
struct Entry {
    id: u64,
    text: String,
    done: bool,
    due: Option<Date>,
    folded: bool,
    /// Its todos done, and in all, at every depth.
    progress: (usize, usize),
}

impl Entry {
    fn of(item: &Item) -> Self {
        Self { id: item.id, text: item.text.clone(), done: item.done, due: item.due, folded: item.folded, progress: item.progress() }
    }
}

/// A line of the list.
#[derive(Clone, PartialEq)]
enum Line {
    /// `spaced` keeps it apart from what is above it.
    Header { entry: Entry, spaced: bool },
    Todo { entry: Entry, depth: usize },
    /// The composer, in place of the row it edits or where what it adds will go.
    Composer { target: Target, depth: usize },
    Empty(&'static str),
}

impl Line {
    fn key(&self) -> (u8, u64) {
        match self {
            Line::Header { entry, .. } => (0, entry.id),
            Line::Todo { entry, .. } => (1, entry.id),
            Line::Composer { .. } => (2, 0),
            Line::Empty(_) => (3, 0),
        }
    }
}

/// The lines for `items` at `depth`, each followed by its children unless it is folded, and the
/// composer where it goes. Only what [`Item::shows`] shows is there.
fn lines(items: &[Item], depth: usize, show: Show, composer: Option<Target>, out: &mut Vec<Line>) {
    let filtering = show != Show::All;
    for item in items.iter().filter(|item| item.shows(show)) {
        let id = item.id;
        if composer == Some(Target::Edit(id)) {
            out.push(Line::Composer { target: Target::Edit(id), depth });
        } else if item.header {
            out.push(Line::Header { entry: Entry::of(item), spaced: !out.is_empty() });
        } else {
            out.push(Line::Todo { entry: Entry::of(item), depth });
        }
        let adding = composer == Some(Target::Under(id));
        if !item.folded || filtering || adding {
            let below = if item.header { depth } else { depth + 1 };
            lines(&item.children, below, show, composer, out);
            if adding {
                out.push(Line::Composer { target: Target::Under(id), depth: below });
            }
        }
    }
}

/// The screen's own state: what it shows, and what its fields hold.
#[derive(Clone, Copy)]
struct State {
    list: Signal<todo::Todos>,
    show: Signal<Show>,
    composer: Signal<Option<Target>>,
    /// The add field's, always there.
    new_text: Signal<String>,
    new_due: Signal<String>,
    /// The composer's, for a subtask, a header or an edit.
    text: Signal<String>,
    due: Signal<String>,
    add_field: ElementRef,
}

impl State {
    /// Adds what the add field holds, and leaves it empty and focused for the next one.
    fn add(self) {
        let text = self.new_text.get_untracked().trim().to_string();
        let Some(due) = parse_due(&self.new_due.get_untracked(), today()) else { return };
        if text.is_empty() {
            return;
        }
        todo::change(|todos| todos.add(None, text, due, false));
        // A new todo is one to do, so it shouldn't land out of sight.
        if self.show.get_untracked() == Show::Done {
            self.show.set(Show::All);
        }
        self.new_text.set(String::new());
        self.new_due.set(String::new());
        self.add_field.focus();
    }

    /// Opens the composer for `target`, holding what is there already when it edits.
    fn compose(self, target: Target) {
        let (text, due) = match target {
            Target::Edit(id) => {
                let mut list = self.list.get_untracked();
                list.get(id).map_or_else(Default::default, |item| (item.text.clone(), item.due.map(Date::iso).unwrap_or_default()))
            }
            _ => Default::default(),
        };
        // What it adds to or edits has to be on screen.
        self.show.set(Show::All);
        if let Target::Under(id) = target {
            todo::change(|todos| {
                if let Some(parent) = todos.get(id) {
                    parent.folded = false;
                }
            });
        }
        self.text.set(text);
        self.due.set(due);
        self.composer.set(Some(target));
    }

    fn close_composer(self) {
        self.composer.set(None);
        self.add_field.focus();
    }

    /// Keeps what the composer holds. Adding leaves it open for the next one, under a new header for
    /// the todos that go in it; an edit closes it. Nothing typed closes it as well.
    fn commit(self) {
        let Some(target) = self.composer.get_untracked() else { return };
        let text = self.text.get_untracked().trim().to_string();
        if text.is_empty() {
            return self.close_composer();
        }
        let Some(due) = parse_due(&self.due.get_untracked(), today()) else { return };
        match target {
            Target::Edit(id) => {
                todo::change(|todos| {
                    if let Some(item) = todos.get(id) {
                        item.text = text;
                        if !item.header {
                            item.due = due;
                        }
                    }
                });
                self.close_composer();
            }
            Target::Under(parent) => {
                todo::change(|todos| todos.add(Some(parent), text, due, false));
                self.text.set(String::new());
                self.due.set(String::new());
            }
            Target::Header => {
                let id = todo::change(|todos| todos.add(None, text, None, true));
                self.compose(Target::Under(id));
            }
        }
    }

    /// Esc closes the composer, else empties the add field, else closes the window.
    fn escape(self) {
        if self.composer.get_untracked().is_some() {
            self.close_composer();
        } else if !self.new_text.get_untracked().is_empty() || !self.new_due.get_untracked().is_empty() {
            self.new_text.set(String::new());
            self.new_due.set(String::new());
        } else {
            CLOSE.notify_one();
        }
    }

    fn fold(self, id: u64) {
        todo::change(|todos| {
            if let Some(item) = todos.get(id) {
                item.folded = !item.folded;
            }
        });
    }
}

#[component]
pub fn Todos() -> impl IntoElement {
    let state = State {
        list: signal(todo::watch().borrow().clone()),
        show: signal(Show::All),
        composer: signal(None),
        new_text: signal(String::new()),
        new_due: signal(String::new()),
        text: signal(String::new()),
        due: signal(String::new()),
        add_field: element_ref(),
    };
    spawn(async move {
        let mut todos = todo::watch();
        while todos.changed().await.is_ok() {
            let now = todos.borrow_and_update().clone();
            state.list.set(now);
        }
    });
    // In the fields too, which would otherwise take the first Esc to let go of the keyboard.
    on_key_with("escape", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, move |_| state.escape());

    // The frame around the screen takes the height its parts need, as far as the window has room.
    let parts: [Signal<f32>; 4] = [signal(0.), signal(0.), signal(0.), signal(0.)];
    effect(move || {
        let height: f32 = parts.iter().map(|part| part.get()).sum();
        HEIGHT.send_if_modified(|now| std::mem::replace(now, height) != height);
    });
    let measure = move |part: usize| move |bounds: &Bounds<Points>| parts[part].set(bounds.size.height.0);

    let lines = move || {
        let (show, composer) = (state.show.get(), state.composer.get());
        let mut out = vec![];
        state.list.with(|list| lines(&list.items, 0, show, composer, &mut out));
        if composer == Some(Target::Header) {
            out.push(Line::Composer { target: Target::Header, depth: 0 });
        }
        if out.is_empty() {
            out.push(Line::Empty(match show {
                _ if state.list.with(|list| list.items.is_empty()) => "Nothing to do yet. Add a todo above, or a header to group them.",
                Show::Done => "Nothing done yet",
                _ => "All done",
            }));
        }
        out
    };
    let has_todos = move || state.list.with(|list| list.counts() != (0, false));

    view()
        .flex_1()
        .bg(Colors::popover())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(rems(0.25))
                .px(rems(1.))
                .py(rems(0.5))
                .border_b_1()
                .border_color(Colors::border())
                .on_layout(measure(0))
                .child(text("Todos").flex_1().font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground()))
                .child(icon_button(IconName::Heading, rems(0.875), "Add a header", move || state.compose(Target::Header)))
                .child(icon_button(IconName::Close, rems(0.875), "Close (Esc)", || CLOSE.notify_one())),
        )
        .child(view().px(rems(1.)).pt(rems(0.75)).pb(rems(0.5)).on_layout(measure(1)).child(add_row(state)))
        .child(
            scroll().flex_shrink(1.).child(
                view()
                    .gap(rems(0.125))
                    .px(rems(0.75))
                    .pb(rems(0.75))
                    .on_layout(measure(2))
                    .child(each(lines, Line::key, move |line, _| render_line(state, line))),
            ),
        )
        .child(when(has_todos, move || footer(state).on_layout(measure(3))).otherwise(move || {
            parts[3].set(0.);
        }))
}

/// A line to type a todo into: its lead icon, the text, then the due date with how it was read.
fn entry(name: &'static str, lead_icon: IconName, value: Signal<String>, placeholder: &'static str, due: Option<Signal<String>>, on_submit: impl Fn() + Copy + 'static, field: Option<ElementRef>) -> View {
    let input = |id: String, value: Signal<String>, placeholder: &'static str| {
        nsh_core::element::input()
            .test_id(&id)
            .value(move || value.get())
            .placeholder(placeholder)
            .placeholder_color(Colors::one(Colors::muted()))
            .selection_color(Colors::one(Colors::caret()))
            .text_color(Colors::foreground())
            .auto_correct(false)
            .submit_behavior(nsh_core::element::SubmitBehavior::Submit)
            .on_change(move |text: &str| value.set(text.to_string()))
            .on_submit(move |_: &str| on_submit())
            .flex_1()
    };
    let mut text_input = input(format!("{name}-text"), value, placeholder).auto_focus(true);
    if let Some(field) = field {
        text_input = text_input.element_ref(&field);
    }
    let due = due.map(|due| {
        let hint = move || match parse_due(&due.get(), today()) {
            Some(Some(date)) => (due_label(date, today()), false),
            Some(None) => (String::new(), false),
            None => ("?".to_string(), true),
        };
        view()
            .flex_none()
            .w(rems(13.))
            .flex_row()
            .items_center()
            .gap(rems(0.5))
            .child(icon(IconName::Calendar, rems(0.875), Colors::muted()))
            .child(input(format!("{name}-due"), due, DUE_HINT))
            .child(
                text(move || hint().0)
                    .flex_none()
                    .text_size(text_xs())
                    .text_color(move || if hint().1 { Colors::danger()() } else { Colors::muted()() }),
            )
    });
    view()
        .flex_1()
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .child(view().w(lead()).flex_none().items_center().child(icon(lead_icon, rems(0.875), Colors::muted())))
        .child(text_input)
        .child(due)
}

/// The field at the top: Enter adds what it holds.
fn add_row(state: State) -> View {
    view()
        .h(row_height() + rems(0.5))
        .flex_row()
        .items_center()
        .px(rems(0.625))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .child(entry("todo-new", IconName::Plus, state.new_text, "What needs to be done?", Some(state.new_due), move || state.add(), Some(state.add_field)))
}

fn composer(state: State, target: Target, depth: usize) -> View {
    let header = match target {
        Target::Header => true,
        Target::Edit(id) => state.list.with_untracked(|list| list.items.iter().any(|item| item.id == id && item.header)),
        Target::Under(_) => false,
    };
    let lead_icon = if header { IconName::Heading } else { IconName::Square };
    let placeholder = match target {
        Target::Header => "Header",
        Target::Under(id) if state.list.with_untracked(|list| list.items.iter().any(|item| item.id == id && item.header)) => "Todo",
        _ => "Subtask",
    };
    let label = if matches!(target, Target::Edit(_)) { "Cancel (Esc)" } else { "Done adding (Esc)" };
    view()
        .h(row_height())
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .px(rems(0.625))
        .ml(indent(depth))
        .rounded(Colors::radius())
        .bg(Colors::input_background())
        .child(entry("todo-composer", lead_icon, state.text, placeholder, (!header).then_some(state.due), move || state.commit(), None))
        .child(icon_button(IconName::Close, rems(0.75), label, move || state.close_composer()))
}

fn render_line(state: State, line: ReadSignal<Line>) -> AnyElement {
    match line.get_untracked() {
        Line::Header { entry, .. } => header_row(state, entry.id, line).into_element(),
        Line::Todo { entry, .. } => todo_row(state, entry.id, line).into_element(),
        Line::Composer { target, depth } => composer(state, target, depth).into_element(),
        Line::Empty(message) => view()
            .py(rems(1.5))
            .items_center()
            .child(text(message).text_size(text_sm()).text_color(Colors::muted()))
            .into_element(),
    }
}

fn entry_of(line: ReadSignal<Line>) -> Entry {
    match line.get() {
        Line::Header { entry, .. } | Line::Todo { entry, .. } => entry,
        _ => unreachable!("a row's line keeps its kind"),
    }
}

fn header_row(state: State, id: u64, line: ReadSignal<Line>) -> View {
    let spaced = move || matches!(line.get(), Line::Header { spaced: true, .. });
    let open = move || !entry_of(line).folded || state.show.get() != Show::All;
    let progress = move || {
        let (done, all) = entry_of(line).progress;
        if all > 0 { format!("{done}/{all}") } else { String::new() }
    };
    row()
        .mt(move || if spaced() { rems(0.75) } else { Points(0.) })
        .on_press(move || state.fold(id))
        .child(
            view()
                .w(lead())
                .flex_none()
                .child(dynamic(move || icon(if open() { IconName::ChevronDown } else { IconName::ChevronRight }, rems(0.875), Colors::muted()))),
        )
        .child(
            view()
                .flex_1()
                .flex_row()
                .items_baseline()
                .gap(rems(0.5))
                .child(text(move || entry_of(line).text).number_of_lines(1).flex_shrink(1.).font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground()))
                .child(text(progress).flex_none().text_size(text_xs()).text_color(Colors::muted())),
        )
        .child(on_hover(icon_button(IconName::Plus, rems(0.75), "Add a todo under this header", move || state.compose(Target::Under(id)))))
        .child(on_hover(icon_button(IconName::Pencil, rems(0.75), "Rename header", move || state.compose(Target::Edit(id)))))
        .child(on_hover(icon_button(IconName::Close, rems(0.75), "Remove header, keep its todos", move || todo::change(|todos| todos.remove(id)))))
}

fn todo_row(state: State, id: u64, line: ReadSignal<Line>) -> View {
    let depth = move || match line.get() {
        Line::Todo { depth, .. } => depth,
        _ => 0,
    };
    let done = move || entry_of(line).done;
    // Late is loud, today is a warning, and anything once it is done is quiet.
    let due = move || entry_of(line).due.map(|due| (due, today()));
    let due_tint = move || match due() {
        Some((due, today)) if !done() && due < today => Colors::danger()(),
        Some((due, today)) if !done() && due == today => Colors::warning()(),
        _ => Colors::muted()(),
    };
    // Folding a todo is on its count of subtasks, since its lead column is its box.
    let count = move || {
        let entry = entry_of(line);
        let (done, all) = entry.progress;
        let folded = entry.folded && state.show.get() == Show::All;
        if all > 0 { format!("{}{done}/{all}", if folded { "▸ " } else { "" }) } else { String::new() }
    };
    let has_subtasks = move || entry_of(line).progress.1 > 0;
    row()
        .test_id(&format!("todo-{id}"))
        .ml(move || indent(depth()))
        // A press on the row checks it off, as in any todo list; the pencil edits it.
        .on_press(move || todo::change(|todos| todos.toggle(id)))
        .child(
            view()
                .w(lead())
                .flex_none()
                .child(dynamic(move || {
                    let (name, tint): (_, fn() -> AdaptiveColor) = if done() { (IconName::SquareCheck, || Colors::muted()()) } else { (IconName::Square, || Colors::foreground()()) };
                    icon(name, rems(0.875), tint)
                })),
        )
        .child(
            text(move || entry_of(line).text)
                .flex_1()
                .number_of_lines(1)
                .style_when(done, |s| s.line_through())
                .text_color(move || if done() { Colors::muted()() } else { Colors::foreground()() }),
        )
        .child(when(has_subtasks, move || {
            text(count)
                .flex_none()
                .text_size(text_xs())
                .text_color(Colors::muted())
                .hover(|s| s.text_color(Colors::foreground()))
                .on_press(move || state.fold(id))
        }))
        .child(text(move || due().map(|(due, today)| due_label(due, today)).unwrap_or_default()).flex_none().text_size(text_xs()).text_color(due_tint))
        .child(on_hover(icon_button(IconName::Plus, rems(0.75), "Add a subtask", move || state.compose(Target::Under(id)))))
        .child(on_hover(icon_button(IconName::Pencil, rems(0.75), "Edit, or set its due date", move || state.compose(Target::Edit(id)))))
        .child(on_hover(icon_button(IconName::Close, rems(0.75), "Delete, with its subtasks", move || todo::change(|todos| todos.remove(id)))))
}

/// A row of the list: its controls show while the pointer is on it.
fn row() -> View {
    view()
        .group()
        .h(row_height())
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .px(rems(0.625))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
}

/// A row's control that only shows while the pointer is on the row.
fn on_hover(control: View) -> View {
    control.opacity(0.).group_hover(|s| s.opacity(1.))
}

/// A small ghost button with an icon.
fn icon_button(name: IconName, size: Points, label: &'static str, on_press: impl Fn() + 'static) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(label)
        .flex_none()
        .p(rems(0.25))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(tooltip(label))
        .on_press(on_press)
        .child(icon(name, size, Colors::muted()))
}

/// What is left, which todos to show, and a way to clear the done ones.
fn footer(state: State) -> View {
    let left = move || {
        let left = state.list.with(|list| list.counts().0);
        if left == 1 { "1 item left".to_string() } else { format!("{left} items left") }
    };
    let any_done = move || state.list.with(|list| list.counts().1);
    let link = move |label: &'static str, on: Option<Show>, press: Box<dyn Fn()>| {
        let selected = move || on.is_some_and(|show| state.show.get() == show);
        text(label)
            .cursor_pointer()
            .text_size(text_sm())
            .text_color(move || if selected() { Colors::foreground()() } else { Colors::muted()() })
            .style_when(selected, |s| s.font_weight(FontWeight::MEDIUM))
            .hover(|s| s.text_color(Colors::foreground()))
            .on_press(press)
    };
    let tab = move |label: &'static str, show: Show| {
        link(label, Some(show), Box::new(move || {
            state.show.set(show);
            state.composer.set(None);
        }))
    };
    view()
        .flex_row()
        .items_center()
        .gap(rems(1.))
        .px(rems(1.))
        .py(rems(0.5))
        .border_t_1()
        .border_color(Colors::border())
        .child(text(left).flex_1().number_of_lines(1).text_size(text_sm()).text_color(Colors::muted()))
        .child(tab("All", Show::All))
        .child(tab("Active", Show::Active))
        .child(tab("Done", Show::Done))
        .child(
            view()
                .w(rems(5.5))
                .flex_row()
                .justify_end()
                .child(when(any_done, move || link("Clear done", None, Box::new(|| todo::change(|todos| todos.clear_done()))))),
        )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::FutureExt as _;
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    fn todo_named(text: &str) -> Option<Item> {
        fn find(items: &[Item], text: &str) -> Option<Item> {
            items.iter().find_map(|item| if item.text == text { Some(item.clone()) } else { find(&item.children, text) })
        }
        find(&todo::watch().borrow().items, text)
    }

    fn start() -> Headless {
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Todos>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(600.), Points(700.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app
    }

    /// The one test of the screen: the list is the store's, which is shared, so its steps run in order.
    #[test]
    fn the_screen_adds_checks_off_nests_filters_and_clears_todos() {
        todo::change(|todos| *todos = todo::Todos::default());
        let mut app = start();
        assert!(app.shows("Nothing to do yet. Add a todo above, or a header to group them."));

        // The add field: Enter adds what it holds, with its due date as people say it.
        app.type_text("id:todo-new-text", "Buy milk").unwrap();
        app.type_text("id:todo-new-due", "tomorrow").unwrap();
        assert!(app.shows("Tomorrow"), "the due date is read as it is typed");
        app.submit("id:todo-new-due").unwrap();
        app.settle();
        let milk = todo_named("Buy milk").expect("added");
        assert_eq!(milk.due, Some(today().plus(1)));
        assert!(app.shows("1 item left"));

        // A press on the row checks it off; Active then hides it.
        app.press("text:Buy milk").unwrap();
        app.settle();
        assert!(todo_named("Buy milk").unwrap().done);
        app.press("Active").unwrap();
        app.settle();
        assert!(!app.shows("Buy milk"));
        assert!(app.shows("All done"));
        app.press("All").unwrap();
        app.settle();

        // A header, then the composer stays open for the todos that go under it.
        app.press("role:button[Add a header]").unwrap();
        app.settle();
        assert!(app.screen().count("text:Header").unwrap() > 0, "the composer is for a header");
        app.type_text("id:todo-composer-text", "Work").unwrap();
        app.submit("id:todo-composer-text").unwrap();
        app.settle();
        assert!(app.screen().count("text:Todo").unwrap() > 0, "the composer is for a todo");
        app.type_text("id:todo-composer-text", "Ship").unwrap();
        app.submit("id:todo-composer-text").unwrap();
        app.settle();
        let work = todo::watch().borrow().items.iter().find(|item| item.header).cloned().expect("a header");
        assert_eq!(work.text, "Work");
        assert_eq!(work.children.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Ship"]);
        app.press_key("escape").unwrap();
        app.settle();
        assert_eq!(app.screen().count("id:todo-composer-text").unwrap(), 0, "Esc closes the composer");

        // Pressing the header folds it.
        app.press("text:Work").unwrap();
        app.settle();
        assert!(!app.shows("Ship"));
        app.press("text:Work").unwrap();
        app.settle();

        // A subtask, from the row's own button.
        let ship = format!("id:todo-{}", todo_named("Ship").unwrap().id);
        app.hover(&ship).unwrap();
        app.press(&format!("{ship} >> label:Add a subtask")).unwrap();
        app.settle();
        assert!(app.screen().count("text:Subtask").unwrap() > 0, "the composer is for a subtask");
        app.type_text("id:todo-composer-text", "Tag").unwrap();
        app.submit("id:todo-composer-text").unwrap();
        app.settle();
        assert_eq!(todo_named("Ship").unwrap().children.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["Tag"]);
        app.press_key("escape").unwrap();
        app.settle();

        // Clear done drops the done todo.
        app.press("Clear done").unwrap();
        app.settle();
        assert!(todo_named("Buy milk").is_none());
        assert!(app.shows("2 items left"));

        // With nothing typed and nothing open, Esc asks the frame to close.
        app.press_key("escape").unwrap();
        app.wait(Duration::from_millis(50)).unwrap();
        assert!(todo::CLOSE.notified().now_or_never().is_some());
        drop(app);
        let _ = std::fs::remove_dir_all(todo::file().parent().unwrap());
    }
}
