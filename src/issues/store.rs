//! A repo's `.po/` folder, read into a [`Tracker`] and written back.
//!
//! ```text
//! .po/config.json         the key prefix, the next number, the labels
//! .po/issues/PO-12.json   an issue
//! .po/epics/launch.json   an epic, with its milestones
//! ```
//!
//! The folder is always the primary checkout's: a worktree made for a thread has its own copy of
//! the repo but shares the issues, so every thread sees the same ones. Writes go to a temp file
//! and are renamed over, a file that can't be read is moved aside as `.bak` rather than written
//! over, and a file is only rewritten when its contents change.
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Config, Epic, Issue, Tracker, default_prefix, is_id};

/// The checkout that owns `path`'s `.po/`: itself, or for a git worktree, the checkout it was
/// made from.
pub fn primary_root(path: &Path) -> PathBuf {
    let own = || path.to_path_buf();
    // A worktree's `.git` is a file naming its own git dir, which names the shared one.
    let Ok(text) = std::fs::read_to_string(path.join(".git")) else { return own() };
    let Some(git_dir) = text.trim().strip_prefix("gitdir:").map(|dir| path.join(dir.trim())) else { return own() };
    let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) else { return own() };
    let common = git_dir.join(common.trim());
    let common = std::fs::canonicalize(&common).unwrap_or(common);
    match common.file_name() {
        Some(name) if name == ".git" => common.parent().map_or_else(own, Path::to_path_buf),
        _ => own(),
    }
}

/// The issues of one repo, and where they are kept.
#[derive(Debug)]
pub struct Store {
    /// The `.po` folder.
    dir: PathBuf,
    pub tracker: Tracker,
    /// What was wrong with the files when they were read: skipped, moved aside, or mended.
    pub problems: Vec<String>,
    /// What the files looked like when they were last read or written.
    seen: u64,
}

/// A number that changes when a file under `dir` is added, removed, or written.
fn fingerprint(dir: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    for folder in ["", "issues", "epics"] {
        let mut entries: Vec<_> = std::fs::read_dir(dir.join(folder)).into_iter().flatten().flatten().filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json")).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let meta = entry.metadata().ok();
            (entry.file_name(), meta.as_ref().map(std::fs::Metadata::len), meta.and_then(|m| m.modified().ok())).hash(&mut hasher);
        }
    }
    hasher.finish()
}

impl Store {
    /// The issues of the repo `project` is a checkout of. A repo with no `.po/` yet has none, and
    /// nothing is made on disk until something is written.
    pub fn open(project: &Path) -> Self {
        let root = primary_root(project);
        let dir = root.join(".po");
        let name = root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let (tracker, problems) = read(&dir, &default_prefix(&name));
        let seen = fingerprint(&dir);
        Self { dir, tracker, problems, seen }
    }

    /// The checkout the issues belong to.
    pub fn root(&self) -> &Path {
        self.dir.parent().unwrap_or(&self.dir)
    }

    /// Whether the files differ from what was last read or written: another window, git, or a
    /// hand edit changed them.
    pub fn changed_on_disk(&self) -> bool {
        fingerprint(&self.dir) != self.seen
    }

    /// Reads the files again, for when git or another window changed them. Changes not yet written
    /// are lost, so this is for after a [`Store::flush`].
    pub fn reload(&mut self) {
        let fallback = self.tracker.config.prefix.clone();
        (self.tracker, self.problems) = read(&self.dir, &fallback);
        self.seen = fingerprint(&self.dir);
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes what has changed since the last time. What can't be written stays to be tried
    /// again; the first error is returned.
    pub fn flush(&mut self) -> io::Result<()> {
        let mut first = None;
        let mut note = |result: io::Result<()>| {
            if let Err(error) = result {
                first.get_or_insert(error);
                false
            } else {
                true
            }
        };
        let dirty = std::mem::take(&mut self.tracker.dirty);
        let tracker = &self.tracker;
        let mut left = super::Dirty::default();
        if dirty.config && !note(write(&self.dir.join("config.json"), &tracker.config.to_json())) {
            left.config = true;
        }
        for number in dirty.issues {
            let Some(issue) = tracker.issue(number) else { continue };
            if !note(write(&self.dir.join("issues").join(format!("{}.json", tracker.key(number))), &issue.to_json())) {
                left.issues.insert(number);
            }
        }
        for number in dirty.removed {
            if !note(remove(&self.dir.join("issues").join(format!("{}.json", tracker.key(number))))) {
                left.removed.insert(number);
            }
        }
        for id in dirty.epics {
            let Some(epic) = tracker.epic(&id) else { continue };
            if !note(write(&self.dir.join("epics").join(format!("{id}.json")), &epic.to_json())) {
                left.epics.insert(id);
            }
        }
        for id in dirty.removed_epics {
            if !note(remove(&self.dir.join("epics").join(format!("{id}.json")))) {
                left.removed_epics.insert(id);
            }
        }
        self.tracker.dirty = left;
        self.seen = fingerprint(&self.dir);
        first.map_or(Ok(()), Err)
    }
}

/// Writes `value` to `path`, through a temp file, unless the file already holds it.
fn write(path: &Path, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if std::fs::read(path).is_ok_and(|now| now == bytes) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().ok_or_else(|| io::Error::other("no directory"))?)?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The JSON in `path`. A file that isn't is moved aside as `.bak`, so the first change doesn't
/// write over it.
fn read_json(path: &Path, problems: &mut Vec<String>) -> Option<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                problems.push(format!("{}: {error}", path.display()));
            }
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            problems.push(format!("{}: {error}; kept as a .bak", path.display()));
            if let Err(error) = std::fs::rename(path, path.with_extension("json.bak")) {
                log::warn!("keeping {}: {error}", path.display());
            }
            None
        }
    }
}

/// The `.json` files in `dir` by the name before it, in name order. A missing folder has none.
fn json_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
        .filter_map(|path| Some((path.file_stem()?.to_str()?.to_string(), path)))
        .collect();
    files.sort();
    files
}

fn read(dir: &Path, fallback_prefix: &str) -> (Tracker, Vec<String>) {
    let mut problems = vec![];
    let config = read_json(&dir.join("config.json"), &mut problems).map_or_else(|| Config::new(fallback_prefix.to_string()), |value| Config::from_json(&value, fallback_prefix));
    let mut issues = vec![];
    for (stem, path) in json_files(&dir.join("issues")) {
        let number = stem.strip_prefix(config.prefix.as_str()).and_then(|rest| rest.strip_prefix('-')).and_then(|n| n.parse::<u64>().ok()).filter(|n| *n > 0);
        let Some(number) = number else {
            problems.push(format!("{}: not named {}-<number>; skipped", path.display(), config.prefix));
            continue;
        };
        let Some(value) = read_json(&path, &mut problems) else { continue };
        match Issue::from_json(number, &value) {
            Some(issue) => issues.push(issue),
            None => problems.push(format!("{}: no title; skipped", path.display())),
        }
    }
    let mut epics = vec![];
    for (id, path) in json_files(&dir.join("epics")) {
        if !is_id(&id) {
            problems.push(format!("{}: not a lowercase name; skipped", path.display()));
            continue;
        }
        let Some(value) = read_json(&path, &mut problems) else { continue };
        match Epic::from_json(id, &value) {
            Some(epic) => epics.push(epic),
            None => problems.push(format!("{}: no name; skipped", path.display())),
        }
    }
    let (tracker, mended) = Tracker::from_parts(config, issues, epics);
    problems.extend(mended);
    (tracker, problems)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::issues::{Link, Priority, Status};

    /// A folder of its own, with a repo's name, removed when it goes.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let base = std::env::temp_dir().join(format!("po-issues-{}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)));
            let path = base.join(name);
            std::fs::create_dir_all(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    fn files(dir: &Path) -> Vec<String> {
        json_files(dir).into_iter().map(|(name, _)| name).collect()
    }

    #[test]
    fn opening_a_repo_with_no_issues_makes_nothing() {
        let repo = Temp::new("po");
        let store = Store::open(&repo.0);
        assert_eq!(store.tracker.config.prefix, "PO");
        assert_eq!(store.tracker.issues().count(), 0);
        assert!(store.problems.is_empty());
        assert!(!repo.0.join(".po").exists());
    }

    #[test]
    fn what_is_written_is_read_back() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        let t = &mut store.tracker;
        let epic = t.create_epic("Launch", 1);
        let beta = {
            let mut id = String::new();
            t.update_epic(&epic, 2, |e| id = e.add_milestone("Beta", None));
            id
        };
        let a = t.create("Ship it", 3);
        let b = t.create("Tag it", 4);
        t.update(a, 5, |i| {
            i.status = Status::InProgress;
            i.priority = Priority::High;
            i.labels = vec!["bug".into()];
            i.epic = Some(epic.clone());
            i.milestone = Some(beta.clone());
            i.links = vec![Link::Pr(7)];
        });
        t.update(b, 5, |i| i.parent = Some(a));
        t.define_label("bug", "#e5484d");
        store.flush().unwrap();
        assert_eq!(files(&repo.0.join(".po/issues")), ["PO-1", "PO-2"]);
        assert_eq!(files(&repo.0.join(".po/epics")), ["launch"]);

        let again = Store::open(&repo.0);
        assert!(again.problems.is_empty(), "{:?}", again.problems);
        assert_eq!(again.tracker.config, store.tracker.config);
        assert_eq!(again.tracker.config.next, 3);
        assert!(again.tracker.issues().eq(store.tracker.issues()));
        assert_eq!(again.tracker.epics(), store.tracker.epics());
        assert!(again.tracker.dirty.is_empty());
    }

    #[test]
    fn a_flush_writes_only_what_changed() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        let (a, b) = (store.tracker.create("a", 1), store.tracker.create("b", 1));
        store.flush().unwrap();
        assert!(store.tracker.dirty.is_empty());
        let file = |n: u64| repo.0.join(format!(".po/issues/PO-{n}.json"));
        let before = std::fs::read(file(b)).unwrap();
        // Poke a's file; a flush with nothing changed leaves it alone, and b's change leaves a's.
        std::fs::write(file(a), "{\"title\":\"hand edited\"}").unwrap();
        store.flush().unwrap();
        store.tracker.update(b, 2, |i| i.status = Status::Done);
        store.flush().unwrap();
        assert_eq!(std::fs::read_to_string(file(a)).unwrap(), "{\"title\":\"hand edited\"}");
        assert_ne!(std::fs::read(file(b)).unwrap(), before);
        // One field, one line.
        let diff = std::fs::read_to_string(file(b)).unwrap().lines().zip(String::from_utf8(before).unwrap().lines()).filter(|(x, y)| x != y).count();
        assert_eq!(diff, 2, "status and updated");
        // No temp files are left behind.
        assert_eq!(std::fs::read_dir(repo.0.join(".po/issues")).unwrap().count(), 2);
    }

    #[test]
    fn deleted_issues_and_epics_lose_their_files() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        let a = store.tracker.create("a", 1);
        let e = store.tracker.create_epic("Launch", 1);
        store.flush().unwrap();
        store.tracker.remove(a, 2);
        store.tracker.remove_epic(&e, 2);
        store.flush().unwrap();
        assert!(files(&repo.0.join(".po/issues")).is_empty() && files(&repo.0.join(".po/epics")).is_empty());
        // The number is not given out again.
        assert_eq!(Store::open(&repo.0).tracker.config.next, 2);
        // Removing what was never written is no error.
        let b = store.tracker.create("b", 3);
        store.tracker.remove(b, 4);
        store.flush().unwrap();
    }

    #[test]
    fn numbers_from_another_branch_move_the_counter_past_them() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        store.tracker.create("mine", 1);
        store.flush().unwrap();
        // A merge brings in PO-40 without the config that counted it.
        std::fs::write(repo.0.join(".po/issues/PO-40.json"), r#"{"title":"theirs"}"#).unwrap();
        let mut store = Store::open(&repo.0);
        assert_eq!(store.tracker.config.next, 41);
        assert_eq!(store.problems.len(), 1);
        assert_eq!(store.tracker.create("next", 2), 41);
        store.flush().unwrap();
        assert!(Store::open(&repo.0).problems.is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_kept_and_the_rest_still_load() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        store.tracker.create("fine", 1);
        store.tracker.create_epic("Launch", 1);
        store.flush().unwrap();
        std::fs::write(repo.0.join(".po/issues/PO-2.json"), "<<<<<<< HEAD\n{").unwrap();
        std::fs::write(repo.0.join(".po/issues/notes.json"), "{}").unwrap();
        std::fs::write(repo.0.join(".po/issues/PO-3.json"), r#"{"status":"done"}"#).unwrap();
        std::fs::write(repo.0.join(".po/issues/OTHER-1.json"), r#"{"title":"x"}"#).unwrap();
        std::fs::write(repo.0.join(".po/epics/Not An Id.json"), r#"{"name":"x"}"#).unwrap();
        let store = Store::open(&repo.0);
        assert_eq!(store.tracker.issues().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["fine"]);
        assert_eq!(store.tracker.epics().len(), 1);
        assert_eq!(store.problems.len(), 5, "{:?}", store.problems);
        assert!(repo.0.join(".po/issues/PO-2.json.bak").exists() && !repo.0.join(".po/issues/PO-2.json").exists());
        // A bad config is kept too, and the prefix falls back to the repo's.
        std::fs::write(repo.0.join(".po/config.json"), "nope").unwrap();
        let store = Store::open(&repo.0);
        assert!(repo.0.join(".po/config.json.bak").exists());
        assert_eq!((store.tracker.config.prefix.as_str(), store.tracker.config.next), ("PO", 2));
    }

    #[test]
    fn a_worktree_shares_the_primary_checkouts_issues() {
        let repo = Temp::new("po");
        let tree = Temp::new("po-fix");
        // What `git worktree add` leaves: a `.git` file, the git dir it names, and its `commondir`.
        std::fs::create_dir_all(repo.0.join(".git/worktrees/fix")).unwrap();
        std::fs::write(repo.0.join(".git/worktrees/fix/commondir"), "../..\n").unwrap();
        std::fs::write(tree.0.join(".git"), format!("gitdir: {}\n", repo.0.join(".git/worktrees/fix").display())).unwrap();
        assert_eq!(primary_root(&tree.0), repo.0);
        assert_eq!(primary_root(&repo.0), repo.0);

        let mut from_tree = Store::open(&tree.0);
        assert_eq!(from_tree.dir(), repo.0.join(".po"));
        // The prefix is the repo's, not the worktree folder's.
        assert_eq!(from_tree.tracker.config.prefix, "PO");
        from_tree.tracker.create("from a thread", 1);
        from_tree.flush().unwrap();
        assert!(!tree.0.join(".po").exists());
        assert_eq!(Store::open(&repo.0).tracker.issues().count(), 1);
        // A submodule's `.git` file has no commondir, and a folder with none is itself.
        std::fs::create_dir_all(repo.0.join(".git/modules/sub")).unwrap();
        std::fs::write(tree.0.join(".git"), format!("gitdir: {}\n", repo.0.join(".git/modules/sub").display())).unwrap();
        assert_eq!(primary_root(&tree.0), tree.0);
        std::fs::remove_file(tree.0.join(".git")).unwrap();
        assert_eq!(primary_root(&tree.0), tree.0);
    }

    #[test]
    fn changes_underneath_are_noticed_but_ones_own_writes_are_not() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        assert!(!store.changed_on_disk());
        store.tracker.create("a", 1);
        store.flush().unwrap();
        assert!(!store.changed_on_disk());
        std::fs::write(repo.0.join(".po/issues/PO-2.json"), r#"{"title":"b"}"#).unwrap();
        assert!(store.changed_on_disk());
        store.reload();
        assert!(!store.changed_on_disk());
    }

    #[test]
    fn reload_picks_up_changes_made_underneath() {
        let repo = Temp::new("po");
        let mut store = Store::open(&repo.0);
        store.tracker.create("a", 1);
        store.flush().unwrap();
        std::fs::write(repo.0.join(".po/issues/PO-2.json"), r#"{"title":"b","status":"todo"}"#).unwrap();
        std::fs::write(repo.0.join(".po/config.json"), r#"{"prefix":"PO","next":3}"#).unwrap();
        store.reload();
        assert_eq!(store.tracker.issues().count(), 2);
        assert_eq!(store.tracker.issue(2).unwrap().status, Status::Todo);
    }
}
