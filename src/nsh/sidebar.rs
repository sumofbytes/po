//! The left side: New, Search, Issues, what needs the user, and every project with its threads.
//! [`Snapshot`] is what [`crate::Reader`] publishes (already filtered by the search); the screen
//! only draws it, and what is pressed goes back as a [`Cmd`].
use std::sync::LazyLock;

use nsh_core::prelude::*;
use tokio::sync::watch;

use super::bus::{Cmd, send};
use super::{Colors, header_height, icon, leading, icon_button, on_hover, scale, small, text_base};
use crate::ui::icon::IconName;

/// What a thread is up to, for its mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Idle,
    /// Finished a turn nobody has looked at.
    Unread,
    Running,
    /// Waiting for an answer to a permission request.
    Asking,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Thread {
    pub project: u64,
    pub id: u64,
    pub label: String,
    pub mark: Mark,
    /// On screen: the only filled row.
    pub selected: bool,
    /// What its agent may do through Po's API is more than talk.
    pub controlled: bool,
    /// The day this one starts in its project's list ("Today", "Yesterday"...), if it does.
    pub day: Option<&'static str>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Branch {
    pub name: String,
    pub dirty: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub branch: Option<Branch>,
    /// The one on screen has its name at full strength.
    pub active: bool,
    pub collapsed: bool,
    /// More than one project is open, so this one can be closed.
    pub closable: bool,
    pub threads: Vec<Thread>,
    /// A fold under the threads: `(expanded, how many are hidden)`.
    pub fold: Option<(bool, usize)>,
}

/// A thread waiting on the user, with the project it is in.
#[derive(Clone, PartialEq, Debug)]
pub struct Waiting {
    pub thread: Thread,
    pub project: String,
}

/// What an agent has put to the user: a line, and the label of its yes.
#[derive(Clone, PartialEq, Debug)]
pub struct Suggestion {
    pub id: u64,
    pub text: String,
    pub yes: &'static str,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    /// The glass look is on: headers have no line under them.
    pub glass: bool,
    pub waiting: Vec<Waiting>,
    pub projects: Vec<Project>,
    pub suggestions: Vec<Suggestion>,
    /// What a worktree is doing, under the Projects header.
    pub status: Option<String>,
    pub searching: bool,
    pub issues_open: bool,
    /// How many of the project's issues are open, and how many of those are late.
    pub issues: (usize, usize),
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

/// The sidebar's spacing, on a 4px grid, as the GPUI one it replaces had it.
mod space {
    use super::{Points, scale};
    pub fn gutter() -> Points { scale::s3() }
    pub fn inset() -> Points { scale::s3() }
    pub fn lead() -> Points { scale::s4() }
    pub fn row() -> Points { scale::s8() + scale::s0_5() }
    pub fn tree() -> Points { scale::s7() + scale::s0_5() }
    pub fn more() -> Points { scale::s7() }
    pub fn item_gap() -> Points { Points(0.) }
    pub fn group_gap() -> Points { scale::s4() }
    pub fn list_top() -> Points { scale::s1() }
    pub fn list_bottom() -> Points { scale::s3() }
    pub fn action_gap() -> Points { scale::s1() }
    pub fn actions_top() -> Points { scale::s4() }
    pub fn section_gap() -> Points { scale::s2() }
    pub fn section() -> Points { scale::s8() }
}

/// Muted text at the size of every other line of the sidebar.
fn quiet(content: impl IntoReactive<SharedString>) -> Text {
    text(content).text_size(text_base()).line_height(leading(text_base())).text_color(Colors::muted())
}

/// The most rows Needs you shows before it scrolls.
const NEEDS_YOU_ROWS: f32 = 5.;

/// A row of the sidebar: its icon in the lead column if it has one, then what is added.
fn side_row(lead: Option<View>) -> View {
    view()
        .group()
        .flex_row()
        .items_center()
        .gap(scale::s2_5())
        .px(space::inset())
        .rounded(super::card_radius())
        .cursor_pointer()
        .transition_colors()
        .duration(220.)
        .ease_out()
        .child(lead)
}

/// The lead column, its content centred, so a chevron and the marks under it share an axis.
fn centered(content: impl IntoElement) -> View {
    view().w(space::lead()).flex_none().items_center().justify_center().child(content)
}

/// The lead column, its content at the left edge, where a header's title starts.
fn lead(content: impl IntoElement) -> View {
    view().w(space::lead()).flex_none().items_center().justify_start().child(content)
}

/// A thread's state, in the lead column under its project's chevron: a dot while it runs or has
/// news, a ! when it asks, nothing when idle.
fn mark(mark: Mark) -> View {
    let size = scale::s1_5() + scale::s0_5();
    let dot = |color: fn() -> AdaptiveColor| view().w(size).h(size).rounded(super::pill()).bg(color);
    centered(match mark {
        Mark::Idle => view().into_element(),
        Mark::Asking => text("!").text_size(text_base()).font_weight(FontWeight::BOLD).text_color(Colors::warning()).into_element(),
        Mark::Running => dot(|| Colors::primary()()).into_element(),
        Mark::Unread => dot(|| Colors::added()()).into_element(),
    })
}

/// A quiet section title with what can be done there.
fn section(title: impl IntoReactive<SharedString>, action: Option<View>) -> View {
    view()
        .h(space::section() + space::section_gap())
        .pt(space::section_gap())
        .flex_none()
        .flex_row()
        .items_center()
        .justify_between()
        .px(space::gutter() + space::inset())
        .child(text(title).text_size(super::text_sm()).line_height(leading(super::text_sm())).font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
        .child(action)
}

fn grant_badge() -> View {
    view()
        .flex_none()
        .px(scale::s1())
        .rounded(Colors::radius())
        .bg(Colors::hover())
        .tooltip(super::tooltip("Agent control granted"))
        .child(small("⌁"))
}

fn thread_row(thread: &Thread, project: Option<&str>, close_tip: &'static str) -> View {
    let (pid, tid) = (thread.project, thread.id);
    let selected = thread.selected;
    let label = text(thread.label.clone())
        .number_of_lines(1)
        .flex_shrink(1.)
        .text_size(text_base())
        .line_height(leading(text_base()))
        .text_color(move || if selected { Colors::foreground()() } else { Colors::muted()() })
        .style_when(selected, |s| s.font_weight(FontWeight::MEDIUM));
    let title = match project {
        // The title, then the project it is in, which the row no longer sits under.
        Some(project) => view()
            .flex_1()
            .flex_row()
            .items_baseline()
            .gap(scale::s2())
            .child(label)
            .child(quiet(project.to_string()).number_of_lines(1).flex_shrink(1.)),
        None => view().flex_1().flex_row().child(label.flex_1()),
    };
    let row = side_row(Some(mark(thread.mark)))
        .h(space::tree())
        .test_id(&format!("agent-thread-{tid}"))
        .on_press(move || send(Cmd::OpenThread(pid, tid)))
        .child(title)
        .child(thread.controlled.then(grant_badge))
        .child(on_hover(icon_button(IconName::Close, scale::s3(), close_tip, move || send(Cmd::CloseThread(pid, tid)))));
    if selected { row.bg(Colors::selected()) } else { row.hover(|s| s.bg(Colors::hover())) }
}

/// The day, quietly, over the threads of that day.
fn day_separator(day: &'static str) -> View {
    view()
        .w_full()
        .h(space::more() + space::section_gap())
        .flex_none()
        .flex_row()
        .items_end()
        .pl(space::inset() + space::lead() + scale::s2_5())
        .pr(space::inset())
        .pb(space::section_gap())
        .child(text(day).flex_none().text_size(super::text_xs()).line_height(leading(super::text_xs())).font_weight(FontWeight::MEDIUM).text_color(Colors::dim(Colors::muted())))
}

fn project_group(project: &Project) -> View {
    let pid = project.id;
    let active = project.active;
    let label = move || if active { Colors::foreground()() } else { Colors::muted()() };
    let chevron = if project.collapsed { IconName::ChevronRight } else { IconName::ChevronDown };
    let name = text(project.name.clone())
        .number_of_lines(1)
        .flex_1()
        .min_w_0()
        .text_size(text_base())
        .line_height(leading(text_base()))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(label);
    // Pressing the row opens the project and folds or unfolds its threads; the buttons on it are
    // their own targets.
    let head = side_row(Some(centered(icon(chevron, scale::s4(), Colors::muted()))))
        .h(space::tree())
        .test_id(&format!("agent-project-{pid}"))
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || send(Cmd::ToggleProject(pid)))
        .child(name)
        .child(on_hover(icon_button(IconName::Plus, scale::s3(), "New thread in this project", move || send(Cmd::StartThread(pid)))))
        .child(project.closable.then(|| on_hover(icon_button(IconName::Close, scale::s3(), "Close project", move || send(Cmd::CloseProject(pid))))));
    let fold = project.fold.map(|(expanded, hidden)| {
        let label = if expanded { "Show less".to_string() } else { format!("{hidden} more") };
        side_row(Some(lead(view())))
            .h(space::more())
            .hover(|s| s.bg(Colors::hover()))
            .on_press(move || send(Cmd::ToggleMore(pid)))
            .child(text(label).text_size(super::text_sm()).line_height(leading(super::text_sm())).text_color(Colors::dim(Colors::muted())))
    });
    view()
        .gap(space::item_gap())
        .child(head)
        .child(
            project
                .threads
                .iter()
                .flat_map(|thread| [thread.day.map(day_separator), Some(thread_row(thread, None, "Close thread"))])
                .flatten()
                .collect::<Vec<_>>(),
        )
        .child(fold)
}

/// What agents have put to the user: one line each, with a way to say yes or no.
fn suggestion(item: &Suggestion) -> View {
    let (id, yes) = (item.id, item.yes);
    let button = |label: &'static str, primary: bool, on_press: Box<dyn Fn() + 'static>| {
        let b = view()
            .role(AccessibilityRole::Button)
            .label(label)
            .px(scale::s3())
            .py(scale::s0_5())
            .rounded(super::pill())
            .cursor_pointer()
            .transition_colors()
            .duration(220.)
            .ease_out()
            .on_press(on_press)
            .child(text(label).text_size(text_base()).text_color(move || if primary { Colors::primary_foreground()() } else { Colors::foreground()() }));
        if primary { b.bg(Colors::primary()) } else { b.hover(|s| s.bg(Colors::hover())) }
    };
    view()
        .mx(space::gutter())
        .my(scale::s1())
        .px(scale::s3())
        .py(scale::s2())
        .gap(scale::s2())
        .rounded(super::card_radius())
        .border_1()
        .border_color(Colors::border())
        .bg(Colors::hover())
        .child(quiet(item.text.clone()))
        .child(
            view()
                .flex_row()
                .gap(scale::s1())
                .child(button(yes, true, Box::new(move || send(Cmd::Answer(id, true)))))
                .child(button("Not now", false, Box::new(move || send(Cmd::Answer(id, false))))),
        )
}

/// A plain text action below the title bar: it lights up under the pointer.
fn action(icon_name: IconName, label: &'static str, tip: &'static str, selected: Option<bool>, on_press: impl Fn() + 'static) -> View {
    let row = side_row(Some(lead(icon(icon_name, scale::s4(), Colors::muted()))))
        .h(space::row())
        .tooltip(super::tooltip(tip))
        .on_press(on_press)
        .child(text(label).flex_1().text_size(text_base()).line_height(leading(text_base())).text_color(move || if selected == Some(true) { Colors::foreground()() } else { Colors::foreground()().opacity(crate::theme::tokens::HOVER) }).style_when(selected == Some(true), |s| s.font_weight(FontWeight::MEDIUM)));
    if selected == Some(true) { row.bg(Colors::selected()) } else { row.hover(|s| s.bg(Colors::hover())) }
}

#[component]
pub fn Sidebar() -> impl IntoElement {
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

    let query = signal(String::new());
    let focused = signal(false);
    let field = element_ref();
    let search = move || {
        let filled = !query.get().is_empty();
        let in_use = filled || focused.get();
        view()
            .h(space::row())
            .flex_row()
            .items_center()
            .gap(scale::s2())
            .px(space::inset())
            .rounded(Colors::radius())
            .on_press(move || field.focus())
            .transition_colors()
            .duration(220.)
            .ease_out()
            .transition_colors()
            .duration(220.)
            .ease_out()
            .hover(|s| s.bg(Colors::hover()))
            .style_when(in_use, |s| s.bg(Colors::hover()))
            .child(lead(icon(IconName::Search, scale::s4(), Colors::muted())))
            .child(
                nsh_core::element::input()
                    .test_id("sidebar-search")
                    .element_ref(&field)
                    .value(move || query.get())
                    .placeholder("Search")
                    .placeholder_color(Colors::one(Colors::muted()))
                    .selection_color(Colors::one(Colors::caret()))
                    .text_size(text_base())
                    .text_color(Colors::muted())
                    .auto_correct(false)
                    .on_focus(move || focused.set(true))
                    .on_blur(move || focused.set(false))
                    .on_change(move |text: &str| {
                        query.set(text.to_string());
                        send(Cmd::SideQuery(text.to_string()));
                    })
                    .flex_1(),
            )
            .child(
                (!query.get().is_empty()).then(|| {
                    icon_button(IconName::Close, scale::s3(), "Clear search", move || {
                        query.set(String::new());
                        send(Cmd::SideQuery(String::new()));
                    })
                }),
            )
    };

    let glass = move || state.with(|s| s.glass);
    let issues = move || {
        let (left, late) = state.with(|s| s.issues);
        let open = state.with(|s| s.issues_open);
        let count = (left > 0).then(|| {
            view()
                .min_w(scale::s5())
                .flex_none()
                .items_center()
                .justify_center()
                .tooltip(super::tooltip(if late > 0 { "Some are late" } else { "Open issues" }))
                .child(text(left.to_string()).text_size(text_base()).text_color(if late > 0 { Colors::danger()() } else { Colors::muted()() }))
        });
        action(IconName::CircleDot, "Issues", "Issues", Some(open), || send(Cmd::ToggleIssues)).child(count)
    };

    let waiting = move || {
        let waiting = state.with(|s| s.waiting.clone());
        (!waiting.is_empty()).then(|| {
            let most = space::list_top() + space::list_bottom() + (space::row() + space::item_gap()) * NEEDS_YOU_ROWS;
            view()
                .flex_none()
                .child(section(format!("Needs you · {}", waiting.len()), None))
                .child(
                    scroll().max_h(most).child(
                        view()
                            .gap(space::item_gap())
                            .px(space::gutter())
                            .pt(space::list_top())
                            .pb(space::list_bottom())
                            .child(waiting.iter().map(|item| thread_row(&item.thread, Some(&item.project), "Close thread")).collect::<Vec<_>>()),
                    ),
                )
        })
    };

    let tree = move || {
        let (projects, searching) = state.with(|s| (s.projects.clone(), s.searching));
        let empty = projects.is_empty() && searching && state.with(|s| s.waiting.is_empty());
        scroll().flex_1().child(
            view()
                .gap(space::group_gap())
                .px(space::gutter())
                .pt(space::list_top())
                .pb(space::list_bottom())
                .child(projects.iter().map(project_group).collect::<Vec<_>>())
                .child(empty.then(|| side_row(None).h(space::more()).child(quiet("No matches")))),
        )
    };

    view()
        .flex_1()
        .bg(Colors::sidebar())
        .child(
            view()
                .h(header_height())
                .flex_none()
                .justify_center()
                .px(space::gutter() + space::inset())
                .style_when(move || !glass(), |s| s.border_b_1().border_color(super::hairline()))
                .child(text("Po").text_size(scale::s4() + scale::s0_5()).line_height(leading(text_base())).font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground())),
        )
        .child(
            view()
                .flex_none()
                .gap(space::action_gap())
                .px(space::gutter())
                .pt(space::actions_top())
                .child(action(IconName::Plus, "New", "New thread (Ctrl+N)", None, || send(Cmd::NewThread)))
                .child(dynamic(search))
                .child(dynamic(issues)),
        )
        .child(dynamic(waiting))
        .child(
            view()
                .flex_1()
                .child(section(
                    "Projects",
                    Some(icon_button(IconName::Plus, scale::s3(), "Add a project: open a folder (Ctrl+Shift+O)", || send(Cmd::AddProject))),
                ))
                .child(dynamic(move || {
                    state.with(|s| s.status.clone()).map(|status| {
                        view().px(space::gutter() + space::inset()).py(scale::s1()).max_h(scale::lines(4.)).child(quiet(status))
                    })
                }))
                .child(dynamic(move || state.with(|s| s.suggestions.clone()).iter().map(suggestion).collect::<Vec<_>>()))
                .child(dynamic(tree)),
        )
        .child(
            view()
                .flex_none()
                .px(space::gutter())
                .py(scale::s3())
                .border_t_1()
                .border_color(super::hairline())
                .child(action(IconName::Settings, "Settings", "Settings (Ctrl+,)", None, || send(Cmd::OpenSettings))),
        )
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;

    fn thread(project: u64, id: u64, label: &str, mark: Mark, selected: bool) -> Thread {
        Thread { project, id, label: label.into(), mark, selected, controlled: false, day: None }
    }

    fn start() -> Headless {
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Sidebar>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(300.), Points(700.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app
    }

    #[test]
    fn the_sidebar_draws_its_snapshot_and_reports_what_is_pressed() {
        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        publish(Snapshot {
            waiting: vec![Waiting { thread: thread(2, 20, "Fix the build", Mark::Asking, false), project: "other".into() }],
            projects: vec![
                Project {
                    id: 1,
                    name: "po".into(),
                    branch: Some(Branch { name: "nsh".into(), dirty: true }),
                    active: true,
                    collapsed: false,
                    closable: true,
                    threads: vec![thread(1, 10, "Port the sidebar", Mark::Running, true), thread(1, 11, "Old idea", Mark::Idle, false)],
                    fold: Some((false, 3)),
                },
                Project { id: 2, name: "other".into(), branch: None, active: false, collapsed: true, closable: true, threads: vec![], fold: None },
            ],
            suggestions: vec![Suggestion { id: 7, text: "“Fix the build” wants to open /tmp/x".into(), yes: "Open" }],
            issues_open: false,
            issues: (2, 0),
            ..Snapshot::default()
        });
        let mut app = start();
        for shown in ["Needs you · 1", "Fix the build", "Port the sidebar", "Old idea", "3 more", "po", "Projects", "Open", "Not now", "Issues", "2"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        assert!(!app.shows("nsh"), "a project's branch is not in the sidebar: {:?}", app.screen().texts());
        let mut next = |app: &mut Headless| {
            app.settle();
            std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>()
        };

        app.press("text:Port the sidebar").unwrap();
        assert_eq!(next(&mut app), [Cmd::OpenThread(1, 10)]);
        app.press("text:3 more").unwrap();
        assert_eq!(next(&mut app), [Cmd::ToggleMore(1)]);
        app.press("id:agent-project-2").unwrap();
        assert_eq!(next(&mut app), [Cmd::ToggleProject(2)]);
        app.press("text:Open").unwrap();
        assert_eq!(next(&mut app), [Cmd::Answer(7, true)]);
        app.press("text:Not now").unwrap();
        assert_eq!(next(&mut app), [Cmd::Answer(7, false)]);
        app.press("text:New").unwrap();
        assert_eq!(next(&mut app), [Cmd::NewThread]);
        app.press("text:Settings").unwrap();
        assert_eq!(next(&mut app), [Cmd::OpenSettings]);
        app.press("text:Issues").unwrap();
        assert_eq!(next(&mut app), [Cmd::ToggleIssues]);
        app.press("label:Add a project: open a folder (Ctrl+Shift+O)").unwrap();
        assert_eq!(next(&mut app), [Cmd::AddProject]);
        app.hover("text:Old idea").unwrap();
        app.press("id:agent-thread-11 >> label:Close thread").unwrap();
        assert_eq!(next(&mut app), [Cmd::CloseThread(1, 11)]);

        // The filter reports what is typed, and a cross clears it.
        app.type_text("id:sidebar-search", "po").unwrap();
        assert_eq!(next(&mut app).last(), Some(&Cmd::SideQuery("po".into())));
        app.press("label:Clear search").unwrap();
        assert_eq!(next(&mut app), [Cmd::SideQuery(String::new())]);

        // A new snapshot is drawn: with nothing waiting the section goes.
        publish(Snapshot { searching: true, ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("No matches") && !app.shows("Needs you · 1"), "{:?}", app.screen().texts());
    }
}
