//! Brings the old todo list in as issues. Po used to keep one checklist for all projects in
//! `todos.json`; issues replaced it, and this reads what it left behind.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Status, Tracker};
use crate::date::Date;

/// One line of the old list: a todo, or a header that grouped the todos under it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub text: String,
    pub header: bool,
    pub done: bool,
    pub due: Option<Date>,
    pub children: Vec<Item>,
}

/// Where the old list was kept.
pub fn file() -> PathBuf {
    crate::config::data_dir().join("todos.json")
}

fn items(list: &Value, top: bool) -> Vec<Item> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(Item {
                text: entry["text"].as_str()?.to_string(),
                // Headers were only ever at the top.
                header: top && entry["header"].as_bool().unwrap_or(false),
                done: entry["done"].as_bool().unwrap_or(false),
                due: entry["due"].as_str().and_then(Date::from_iso),
                children: items(&entry["children"], false),
            })
        })
        .collect()
}

/// The old list in `path`; nothing if there is none or it can't be read.
pub fn read(path: &Path) -> Vec<Item> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    serde_json::from_str::<Value>(&text).map(|value| items(&value["items"], true)).unwrap_or_default()
}

/// Where it is noted that the old list has been dealt with: imported somewhere, or dismissed. It
/// is offered once, to one project, so importing in two does not make the list twice.
pub fn marker() -> PathBuf {
    crate::config::data_dir().join("todos.json.handled")
}

/// How many todos `todos` holds that are still on offer: none once `marker` exists.
pub fn on_offer_at(todos: &Path, marker: &Path) -> usize {
    fn count(items: &[Item]) -> usize {
        items.iter().map(|item| usize::from(!item.header) + count(&item.children)).sum()
    }
    if marker.exists() { 0 } else { count(&read(todos)) }
}

/// How many todos of the old list are still on offer.
pub fn on_offer() -> usize {
    on_offer_at(&file(), &marker())
}

/// Notes that the old list has been dealt with (`how` says what was done), so it is not offered
/// again.
pub fn handled(how: &str) {
    let marker = marker();
    if let Err(error) = marker.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&marker, how)) {
        log::warn!("writing {}: {error}", marker.display());
    }
}

/// Adds every todo of `old` to `tracker` as an issue: still to do is Todo, checked off is Done,
/// the due date is kept, a todo under another is its sub-issue, and a header becomes the label of
/// the todos under it. Returns how many issues were made. The old list is left as it is.
pub fn from_items(old: &[Item], tracker: &mut Tracker, now: u64) -> usize {
    fn add(items: &[Item], parent: Option<u64>, label: Option<&str>, tracker: &mut Tracker, now: u64) -> usize {
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
    add(old, None, None, tracker, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::Config;

    const OLD: &str = r#"{ "version": 1, "items": [
        { "text": "Work", "header": true, "done": false, "due": null, "folded": false, "children": [
            { "text": "Ship", "header": false, "done": false, "due": "2026-11-02", "folded": false, "children": [
                { "text": "Tag", "header": false, "done": true, "due": null, "folded": false, "children": [] } ] } ] },
        { "text": "Milk", "header": false, "done": false, "due": null, "folded": false, "children": [] } ] }"#;

    #[test]
    fn the_old_file_becomes_issues_with_its_nesting_headers_dates_and_checks() {
        let dir = std::env::temp_dir().join(format!("po-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.json");
        std::fs::write(&path, OLD).unwrap();
        let old = read(&path);
        assert_eq!(old.len(), 2);
        let mut tracker = Tracker::new(Config::new("PO".into()));
        assert_eq!(from_items(&old, &mut tracker, 5), 3);
        let by_title = |title: &str| tracker.issues().find(|issue| issue.title == title).cloned().unwrap();
        let (ship, tag, milk) = (by_title("Ship"), by_title("Tag"), by_title("Milk"));
        let due = Date::from_ymd(2026, 11, 2).unwrap();
        assert_eq!((ship.status, ship.due, ship.labels.as_slice()), (Status::Todo, Some(due), ["Work".to_string()].as_slice()));
        assert_eq!((tag.parent, tag.status), (Some(ship.number), Status::Done));
        assert_eq!((milk.parent, milk.labels.len()), (None, 0));
        assert_eq!(tracker.config.labels.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_old_list_is_on_offer_until_it_has_been_dealt_with() {
        let dir = std::env::temp_dir().join(format!("po-import-offer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (todos, marker) = (dir.join("todos.json"), dir.join("todos.json.handled"));
        assert_eq!(on_offer_at(&todos, &marker), 0, "no file, nothing to offer");
        std::fs::write(&todos, OLD).unwrap();
        assert_eq!(on_offer_at(&todos, &marker), 3, "headers are not todos");
        std::fs::write(&marker, "dismissed").unwrap();
        assert_eq!(on_offer_at(&todos, &marker), 0, "once handled, not again, in any project");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_file_has_nothing_to_bring_in() {
        let dir = std::env::temp_dir().join(format!("po-import-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read(&dir.join("todos.json")).is_empty());
        std::fs::write(dir.join("todos.json"), "{ not json").unwrap();
        assert!(read(&dir.join("todos.json")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
