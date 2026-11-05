//! The Home page, where Po opens: what is due today and what is late across every open project,
//! the week, and the threads to pick up. Everything on it is read from the issues store and the
//! sidebar's snapshot; what is pressed goes back as a [`Cmd`] or changes an issue in the store.
use std::path::PathBuf;

use nsh_core::prelude::*;

use super::bus::{Cmd, send};
use super::issues::{priority_tint, status_icon, status_tint};
use super::overlay::{self, Height};
use super::sidebar::{self, Mark, Snapshot};
use super::{Colors, Sizing, Smooth, body, button, caption, icon, note, scale, small, text_sm, text_xs};
use crate::date::{Date, due_label, today};
use crate::issues::shared::{self, Overview};
use crate::issues::{Priority, Status};
use crate::ui::icon::IconName;

/// The page fills the space it is given, so the height it measures is not used.
pub static HEIGHT: Height = overlay::height();

/// The most tasks the Today card lists before it points to the issues page.
const TASKS: usize = 8;
/// The most threads the card of threads lists.
const THREADS: usize = 5;

/// An open issue that is due today or before, or is being worked on.
#[derive(Clone, PartialEq, Debug)]
pub struct Task {
    pub repo: PathBuf,
    pub project: String,
    pub number: u64,
    pub key: String,
    pub title: String,
    pub status: Status,
    pub priority: Priority,
    pub due: Option<Date>,
}

impl Task {
    fn late(&self, today: Date) -> bool {
        self.due.is_some_and(|due| due < today)
    }
}

/// A thread worth picking up.
#[derive(Clone, PartialEq, Debug)]
pub struct Pickup {
    pub project: u64,
    pub id: u64,
    pub label: String,
    pub owner: String,
    pub mark: Mark,
    pub ago: Option<String>,
}

/// What the page shows, worked out from the issues and the sidebar.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Model {
    /// Late first, then due today, then in progress; most urgent first within each.
    pub tasks: Vec<Task>,
    pub due_today: usize,
    pub late: usize,
    pub in_progress: usize,
    /// For each day of the week from Monday, how many open issues are due.
    pub week: Vec<(Date, usize)>,
    pub threads: Vec<Pickup>,
    /// Threads running or asking.
    pub active_threads: usize,
}

/// A priority's rank for sorting: urgent first, none last.
fn rank(priority: Priority) -> u8 {
    match priority {
        Priority::Urgent => 0,
        Priority::High => 1,
        Priority::Medium => 2,
        Priority::Low => 3,
        Priority::None => 4,
    }
}

pub fn model(overview: &Overview, snapshot: &Snapshot, today: Date) -> Model {
    let mut tasks = vec![];
    let monday = today.plus(-(((today.weekday() + 6) % 7) as i64));
    let mut week: Vec<(Date, usize)> = (0..7).map(|ix| (monday.plus(ix), 0)).collect();
    for repo in &overview.repos {
        for issue in repo.tracker.issues().filter(|issue| issue.status.is_open()) {
            if let Some(day) = week.iter_mut().find(|(day, _)| Some(*day) == issue.due) {
                day.1 += 1;
            }
            let due_now = issue.due.is_some_and(|due| due <= today);
            if due_now || issue.status == Status::InProgress {
                tasks.push(Task {
                    repo: repo.root.clone(),
                    project: repo.name.clone(),
                    number: issue.number,
                    key: repo.tracker.key(issue.number),
                    title: issue.title.clone(),
                    status: issue.status,
                    priority: issue.priority,
                    due: issue.due,
                });
            }
        }
    }
    let bucket = |task: &Task| if task.late(today) { 0 } else if task.due == Some(today) { 1 } else { 2 };
    tasks.sort_by(|a, b| (bucket(a), rank(a.priority), a.due, &a.project, a.number).cmp(&(bucket(b), rank(b.priority), b.due, &b.project, b.number)));
    let due_today = tasks.iter().filter(|task| task.due == Some(today)).count();
    let late = tasks.iter().filter(|task| task.late(today)).count();
    let in_progress = tasks.iter().filter(|task| task.status == Status::InProgress).count();

    // Threads waiting on the user come first, then the ones at work or with news; a thread in both
    // lists is one thread.
    let mut seen = std::collections::HashSet::new();
    let owned = snapshot.projects.iter().flat_map(|project| project.threads.iter().map(|thread| (project.name.as_str(), thread)));
    let waiting = snapshot.waiting.iter().map(|item| (item.project.as_str(), &item.thread));
    let all: Vec<_> = waiting.chain(owned).filter(|(_, thread)| seen.insert((thread.project, thread.id))).collect();
    let weight = |mark: Mark| match mark {
        Mark::Asking => 0,
        Mark::Failed => 1,
        Mark::Unread => 2,
        Mark::Running => 3,
        Mark::Idle => 4,
    };
    let mut threads: Vec<Pickup> = all
        .iter()
        .map(|(owner, thread)| Pickup { project: thread.project, id: thread.id, label: thread.label.clone(), owner: owner.to_string(), mark: thread.mark, ago: thread.ago.clone() })
        .collect();
    threads.sort_by_key(|thread| weight(thread.mark));
    let active_threads = threads.iter().filter(|thread| matches!(thread.mark, Mark::Running | Mark::Asking)).count();
    threads.truncate(THREADS);
    Model { tasks, due_today, late, in_progress, week, threads, active_threads }
}

/// Good morning, afternoon or evening.
fn greeting(hour: u32) -> &'static str {
    match hour {
        0..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// Marks an issue Done, in whichever repo it is.
fn complete(repo: &PathBuf, number: u64) {
    shared::change_repo(repo, |tracker, now| {
        tracker.update(number, now, |issue| issue.status = Status::Done);
    });
}

fn card(title: &'static str, lead: IconName) -> View {
    view()
        .gap(scale::s2())
        .p(scale::s4())
        .rounded(super::card_radius())
        .border_1()
        .border_color(super::hairline())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s2())
                .child(icon(lead, scale::s3_5(), Colors::muted()))
                .child(text(title).sized(text_sm()).font_weight(FontWeight::MEDIUM).text_color(Colors::foreground())),
        )
}

fn metric(label: &'static str, value: usize, tint: AdaptiveColor) -> View {
    view()
        .flex_1()
        .min_w(Points(0.))
        .px(scale::s4())
        .py(scale::s3())
        .rounded(super::card_radius())
        .border_1()
        .border_color(super::hairline())
        .child(text(value.to_string()).sized(super::font(1.5)).font_weight(FontWeight::SEMIBOLD).text_color(move || tint))
        .child(caption(label))
}

fn task_row(task: Task, today: Date, armed: bool, press_done: impl Fn(&PathBuf, u64) + Clone + 'static) -> View {
    let (repo, number) = (task.repo.clone(), task.number);
    let late = task.late(today);
    let due = task.due.map(|due| due_label(due, today)).unwrap_or_else(|| task.status.slug().replace('-', " "));
    let (done_repo, work_repo) = (repo.clone(), repo.clone());
    // Marking done can't be undone, so it waits for a second click, like Discard does.
    let (tint, mark) = if armed { (Colors::warning()(), "Mark done — again to confirm") } else { (status_tint(task.status), "Mark done") };
    view()
        .group()
        .flex_row()
        .items_center()
        .gap(scale::s2_5())
        .px(scale::s2())
        .h(scale::s10())
        .rounded(super::soft_radius())
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .test_id(&format!("home-task-{}", task.key))
        // The circle marks it done; the rest of the row opens it.
        .child(
            view()
                .role(AccessibilityRole::Button)
                .label(mark)
                .flex_none()
                .cursor_pointer()
                .style_when(armed, |s| s.border_1().border_color(Colors::warning()))
                .on_press(move || press_done(&done_repo, number))
                .child(icon(status_icon(task.status), scale::s4(), move || tint)),
        )
        .child(view().w(scale::s0_5()).h(scale::s5()).flex_none().rounded(super::pill()).bg(move || priority_tint(task.priority)))
        .child(
            view()
                .flex_1()
                .min_w(Points(0.))
                .cursor_pointer()
                .on_press(move || send(Cmd::OpenIssue(repo.clone(), number)))
                .child(body(task.title.clone()).number_of_lines(1)),
        )
        .child(super::on_hover(view().flex_none().child(super::icon_button(IconName::Terminal, scale::s3(), "Work on this", move || send(Cmd::WorkOnIssue(work_repo.clone(), number))))))
        .child(caption(task.project.clone()).number_of_lines(1).flex_none())
        .child(text(due).sized(text_xs()).text_color(if late { Colors::danger()() } else { Colors::muted()() }).flex_none().w(scale::due_width()))
}

fn week_strip(week: &[(Date, usize)], today: Date) -> View {
    view().flex_row().gap(scale::s1()).children(week.iter().map(|&(day, due)| {
        let is_today = day == today;
        let (number, name) = (day.ymd().2.to_string(), day.short_weekday());
        view()
            .flex_1()
            .items_center()
            .gap(scale::s1())
            .py(scale::s2())
            .rounded(super::soft_radius())
            .style_when(is_today, |s| s.bg(Colors::selected()))
            .child(caption(name))
            .child(text(number).sized(text_sm()).font_weight(if is_today { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).text_color(Colors::foreground()))
            // A dot for a day with something due.
            .child(view().w(scale::s1()).h(scale::s1()).rounded(super::pill()).style_when(due > 0, |s| s.bg(Colors::warning())))
    }).collect::<Vec<_>>())
}

fn thread_mark(mark: Mark) -> (&'static str, AdaptiveColor) {
    match mark {
        Mark::Asking => ("Needs you", Colors::warning()()),
        Mark::Failed => ("Failed", Colors::danger()()),
        Mark::Unread => ("New reply", Colors::added()()),
        Mark::Running => ("Working", Colors::muted()()),
        Mark::Idle => ("", Colors::muted()()),
    }
}

fn thread_row(thread: Pickup) -> View {
    let (project, id) = (thread.project, thread.id);
    let (status, tint) = thread_mark(thread.mark);
    let meta = [Some(thread.owner.clone()), (!status.is_empty()).then(|| status.to_string()), thread.ago.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    view()
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .py(scale::s1_5())
        .test_id(&format!("home-thread-{id}"))
        .child(
            view()
                .flex_1()
                .min_w(Points(0.))
                .child(body(thread.label.clone()).number_of_lines(1))
                .child(text(meta).sized(text_xs()).number_of_lines(1).text_color(move || tint)),
        )
        .child(button("Resume", false, move || send(Cmd::OpenThread(project, id))))
}

#[component]
pub fn Home() -> impl IntoElement {
    let overview = signal(shared::overview().borrow().clone());
    let snapshot = signal(sidebar::watch().borrow().clone());
    spawn(async move {
        let mut changes = shared::overview();
        while changes.changed().await.is_ok() {
            let now = changes.borrow_and_update().clone();
            overview.set(now);
        }
    });
    spawn(async move {
        let mut changes = sidebar::watch();
        while changes.changed().await.is_ok() {
            let now = changes.borrow_and_update().clone();
            snapshot.set(now);
        }
    });
    // Worked out once per change of either, not once per place it is read.
    let page = computed(move || overview.with(|overview| snapshot.with(|snapshot| model(overview, snapshot, today()))));

    // Marking an issue done waits for a second press, since it can't be undone.
    let done = signal(0u64);
    let press_done = move |repo: &PathBuf, number: u64| {
        if done.get_untracked() == number {
            complete(repo, number);
            done.set(0);
        } else {
            done.set(number);
        }
    };

    let tasks = move || {
        let model = page.get();
        let day = today();
        let waiting = done.get();
        let listed = model.tasks.iter().take(TASKS).cloned().map(|task| { let armed = task.number == waiting; task_row(task, day, armed, press_done.clone()) }).collect::<Vec<_>>();
        let more = model.tasks.len().saturating_sub(TASKS);
        card("Today", IconName::Calendar)
            .child(if listed.is_empty() { Some(note("Nothing is due today. Pick something from the issues, or start a thread.")) } else { None })
            .child(listed)
            .child((more > 0).then(|| {
                view().cursor_pointer().on_press(|| send(Cmd::ToggleIssues)).px(scale::s2()).py(scale::s1()).child(caption(format!("{more} more in Issues")))
            }))
    };
    let side = move || {
        let model = page.get();
        let day = today();
        let threads = model.threads.iter().cloned().map(thread_row).collect::<Vec<_>>();
        view()
            .gap(scale::s4())
            .child(card("This week", IconName::Calendar).child(week_strip(&model.week, day)))
            .child(
                card("Start", IconName::Plus).child(
                    view()
                        .flex_row()
                        .gap(scale::s2())
                        .child(button("New thread", true, || send(Cmd::NewThread)))
                        .child(button("Open issues", false, || send(Cmd::ToggleIssues))),
                ),
            )
            .child(card("Threads", IconName::Terminal).child(if threads.is_empty() { Some(note("No threads yet.")) } else { None }).child(threads))
    };
    let metrics = move || {
        let model = page.get();
        view()
            .flex_row()
            .gap(scale::s3())
            .child(metric("Due today", model.due_today, Colors::foreground()()))
            .child(metric("Late", model.late, if model.late > 0 { Colors::danger()() } else { Colors::muted()() }))
            .child(metric("In progress", model.in_progress, Colors::warning()()))
            .child(metric("Agent threads", model.active_threads, Colors::info()()))
    };

    view().flex_1().bg(Colors::popover()).child(
        scroll().flex_1().child(
            view()
                .gap(scale::s5())
                .px(scale::s6())
                .py(scale::s6())
                .child(
                    view()
                        .gap(scale::s1())
                        .child(small(today().long()))
                        .child(text(greeting(crate::date::hour())).sized(super::font(2.)).font_weight(FontWeight::SEMIBOLD).text_color(Colors::foreground()))
                        .child(note("A clear view of what matters today.")),
                )
                .child(dynamic(metrics))
                .child(
                    view()
                        .flex_row()
                        .items_start()
                        .gap(scale::s4())
                        .child(view().flex_1().min_w(Points(0.)).child(dynamic(tasks)))
                        .child(view().w(scale::form_label_width() * 2.).flex_none().child(dynamic(side))),
                ),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::{Config, Tracker};
    use crate::issues::shared::RepoView;

    #[test]
    fn the_greeting_follows_the_hour() {
        assert_eq!(greeting(7), "Good morning");
        assert_eq!(greeting(13), "Good afternoon");
        assert_eq!(greeting(21), "Good evening");
    }

    #[test]
    fn today_lists_late_then_due_then_started_and_counts_them() {
        let day = Date::from_ymd(2026, 11, 5).unwrap();
        let mut tracker = Tracker::new(Config::new("PO".into()));
        let mut add = |title: &str, status: Status, priority: Priority, due: Option<Date>| {
            let number = tracker.create(title, 1);
            tracker.update(number, 2, |issue| (issue.status, issue.priority, issue.due) = (status, priority, due));
        };
        add("later", Status::Todo, Priority::Urgent, Some(day.plus(3)));
        add("started", Status::InProgress, Priority::None, None);
        add("due", Status::Todo, Priority::Low, Some(day));
        add("late", Status::Todo, Priority::None, Some(day.plus(-2)));
        add("done", Status::Done, Priority::High, Some(day));
        let overview = Overview { checkouts: Default::default(), repos: vec![RepoView { root: "/r/po".into(), name: "po".into(), tracker, problems: vec![] }] };
        let model = model(&overview, &Snapshot::default(), day);
        let titles: Vec<&str> = model.tasks.iter().map(|task| task.title.as_str()).collect();
        assert_eq!(titles, ["late", "due", "started"]);
        assert_eq!((model.due_today, model.late, model.in_progress), (1, 1, 1));
        // The week of Monday Nov 2: today is its fourth day.
        assert_eq!(model.week.len(), 7);
        assert_eq!(model.week[0].0, day.plus(-3));
        assert_eq!(model.week[3].1, 1, "the one due today is on its day");
        assert_eq!(model.week.iter().map(|d| d.1).sum::<usize>(), 3, "every open issue due in the week is counted, done ones are not");
    }

    #[test]
    fn threads_that_need_the_user_come_first_and_none_is_listed_twice() {
        use super::super::sidebar::{Project, Thread, Waiting};
        let thread = |id, mark| Thread { project: 1, id, label: format!("t{id}"), name: format!("t{id}"), mark, selected: false, controlled: false, ago: None, closing: false };
        let snapshot = Snapshot {
            waiting: vec![Waiting { thread: thread(3, Mark::Asking), project: "po".into() }],
            projects: vec![Project { id: 1, name: "po".into(), active: true, collapsed: false, closable: false, threads: vec![thread(1, Mark::Idle), thread(2, Mark::Running), thread(3, Mark::Asking)], fold: None }],
            ..Snapshot::default()
        };
        let model = model(&Overview::default(), &snapshot, Date::from_ymd(2026, 11, 5).unwrap());
        assert_eq!(model.threads.iter().map(|t| t.id).collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(model.active_threads, 2);
    }

    #[test]
    fn the_page_greets_lists_threads_and_reports_what_is_pressed() {
        use super::super::sidebar::{Project, Thread};
        use nsh_engine::EngineConfig;
        use nsh_testing::Headless;

        let mut guard = super::super::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let thread = Thread { project: 1, id: 10, label: "Fix the flicker".into(), name: "Fix the flicker".into(), mark: Mark::Asking, selected: false, controlled: false, ago: Some("3h".into()), closing: false };
        sidebar::publish(Snapshot {
            projects: vec![Project { id: 1, name: "po".into(), active: true, collapsed: false, closable: false, threads: vec![thread], fold: None }],
            ..Snapshot::default()
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Home>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(1000.), Points(800.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        for shown in ["Due today", "Late", "In progress", "Agent threads", "Today", "This week", "Fix the flicker", "Resume", "New thread"] {
            assert!(app.shows(shown), "{shown} is on screen: {:?}", app.screen().texts());
        }
        let mut next = |app: &mut Headless| {
            app.settle();
            std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>()
        };
        app.press("text:Resume").unwrap();
        assert_eq!(next(&mut app), [Cmd::OpenThread(1, 10)]);
        app.press("text:New thread").unwrap();
        assert_eq!(next(&mut app), [Cmd::NewThread]);
        // What the test lent the sidebar no test after it reads.
        sidebar::publish(Snapshot::default());
        assert_eq!(sidebar::watch().borrow().projects.len(), 0);
    }
}
