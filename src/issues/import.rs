//! Brings the old checklist (`todos.json`, the Todos page) in as issues.
use super::{Status, Tracker};
use crate::todo::Todos;

/// Adds every task of `tasks` to `tracker` as an issue: still to do is Todo, checked off is Done,
/// the due date is kept, a task under another is its sub-issue, and a header becomes the label of
/// the tasks under it. Returns how many issues were made. The checklist itself is left as it is.
pub fn from_tasks(tasks: &Todos, tracker: &mut Tracker, now: u64) -> usize {
    fn add(items: &[crate::todo::Item], parent: Option<u64>, label: Option<&str>, tracker: &mut Tracker, now: u64) -> usize {
        let mut made = 0;
        for item in items {
            if item.header {
                if !item.text.trim().is_empty() {
                    tracker.define_label(item.text.trim(), "");
                }
                made += add(&item.children, parent, Some(item.text.trim()), tracker, now);
                continue;
            }
            let number = tracker.create(&item.text, now);
            made += 1;
            tracker.update(number, now, |issue| {
                issue.status = if item.done { Status::Done } else { Status::Todo };
                issue.due = item.due;
                issue.parent = parent;
                if let Some(label) = label.filter(|label| !label.is_empty()) {
                    issue.labels.push(label.to_string());
                }
            });
            made += add(&item.children, Some(number), label, tracker, now);
        }
        made
    }
    add(&tasks.items, None, None, tracker, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::Config;
    use crate::todo::Date;

    #[test]
    fn tasks_become_issues_with_their_nesting_headers_dates_and_checks() {
        let mut tasks = Todos::default();
        let due = Date::from_ymd(2026, 11, 2).unwrap();
        let work = tasks.add(None, "Work".into(), None, true);
        let ship = tasks.add(Some(work), "Ship".into(), Some(due), false);
        let tag = tasks.add(Some(ship), "Tag".into(), None, false);
        tasks.add(None, "Milk".into(), None, false);
        tasks.toggle(tag);
        let mut tracker = Tracker::new(Config::new("PO".into()));
        assert_eq!(from_tasks(&tasks, &mut tracker, 5), 3);
        let by_title = |title: &str| tracker.issues().find(|issue| issue.title == title).cloned().unwrap();
        let (ship, tag, milk) = (by_title("Ship"), by_title("Tag"), by_title("Milk"));
        assert_eq!((ship.status, ship.due, ship.labels.as_slice()), (Status::Todo, Some(due), ["Work".to_string()].as_slice()));
        assert_eq!((tag.parent, tag.status), (Some(ship.number), Status::Done));
        assert_eq!((milk.parent, milk.labels.len()), (None, 0));
        assert_eq!(tracker.config.labels.len(), 1);
    }
}
