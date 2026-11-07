//! The Home page's data: what is due today and what is late across every open project, the week,
//! and the threads to pick up, worked out from the issues store and the sidebar's snapshot. No view
//! code; the screen draws it.
use std::path::{Path, PathBuf};

use super::sidebar::{Mark, Snapshot};
use crate::date::Date;
use crate::issues::shared::{self, Overview};
use crate::issues::{Priority, Status};

/// The most tasks the Today card lists before it points to the issues page.
pub const TASKS: usize = 8;
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
    pub fn late(&self, today: Date) -> bool {
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
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        0..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// Marks an issue Done, in whichever repo it is.
pub fn complete(repo: &Path, number: u64) {
    shared::change_repo(repo, |tracker, now| {
        tracker.update(number, now, |issue| issue.status = Status::Done);
    });
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
        use crate::model::sidebar::{Project, Thread, Waiting};
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
}
