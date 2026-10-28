//! The issues of the repo on screen, shared by the page that edits them, the sidebar that counts
//! them and the API. One repo is open at a time: the project in front. Every change is saved as it
//! is made, and files changed underneath (git, another window) are picked up by a poller.
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, Once};
use std::time::Duration;

use tokio::sync::watch;

use super::store::{Store, fingerprint, load, primary_root};
use super::{Config, Tracker};
use crate::date::today;

/// What a screen sees of the repo's issues.
#[derive(Clone, PartialEq, Debug)]
pub struct View {
    /// The primary checkout; none before a project is shown.
    pub root: Option<PathBuf>,
    /// The repo's folder name.
    pub name: String,
    pub tracker: Tracker,
    pub problems: Vec<String>,
}

impl View {
    fn empty() -> Self {
        Self { root: None, name: String::new(), tracker: Tracker::new(Config::new("ISS".into())), problems: vec![] }
    }
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);
static VIEW: LazyLock<watch::Sender<View>> = LazyLock::new(|| watch::Sender::new(View::empty()));
static POLLER: Once = Once::new();

fn lock() -> MutexGuard<'static, Option<Store>> {
    STORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn publish(store: &Store) {
    let name = store.root().file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let view = View { root: Some(store.root().to_path_buf()), name, tracker: store.tracker.clone(), problems: store.problems.clone() };
    VIEW.send_if_modified(|now| {
        let changed = *now != view;
        if changed {
            *now = view;
        }
        changed
    });
}

/// The issues now and after every change.
pub fn watch() -> watch::Receiver<View> {
    VIEW.subscribe()
}

/// Shows the issues of the repo `project` is a checkout of. Cheap when it is the one already shown.
pub fn set_project(project: &Path) {
    let root = primary_root(project);
    if lock().as_ref().is_some_and(|store| store.root() == root) {
        return;
    }
    // The folder is read before the lock is taken, so a slow disk holds up no one else.
    let opened = Store::open(project);
    publish(&opened);
    *lock() = Some(opened);
    POLLER.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(1500));
                poll();
            }
        });
    });
}

/// Notices files that changed underneath (git, another window) and reads them in. The checking
/// and the reading are done without the lock; it is held only to see that nothing happened to the
/// store meanwhile and to swap the reading in.
fn poll() {
    let Some((dir, prefix, seen)) = lock().as_ref().map(Store::watch_state) else { return };
    if fingerprint(&dir) == seen {
        return;
    }
    let (loaded, read_at) = load(&dir, &prefix);
    let mut guard = lock();
    if let Some(store) = guard.as_mut().filter(|store| store.dir() == dir)
        && store.install(loaded, seen, read_at)
    {
        publish(store);
    }
}

/// Changes the issues with `f`, which is given the time to stamp them with, saves, and tells
/// whoever follows them. `None` when no repo is shown.
pub fn change<R>(f: impl FnOnce(&mut Tracker, u64) -> R) -> Option<R> {
    let mut guard = lock();
    let store = guard.as_mut()?;
    let out = apply(store, f);
    publish(store);
    Some(out)
}

/// Runs `f` on the store's issues and saves them. Files changed underneath since they were read
/// (another window, git) are read first, so a number another window has just used is not handed
/// out again.
fn apply<R>(store: &mut Store, f: impl FnOnce(&mut Tracker, u64) -> R) -> R {
    if store.tracker.dirty.is_empty() && store.changed_on_disk() {
        store.reload();
    }
    let out = f(&mut store.tracker, crate::now_ms());
    if let Err(error) = store.flush() {
        log::warn!("saving {}: {error}", store.dir().display());
    }
    out
}

/// Changes the issues of the repo `root` belongs to with `f`. The repo on screen is changed
/// through the shared store, so the page sees it at once; any other is opened, changed and saved
/// on its own.
pub fn change_repo<R>(root: &Path, f: impl FnOnce(&mut Tracker, u64) -> R) -> R {
    let primary = primary_root(root);
    {
        let mut guard = lock();
        if let Some(store) = guard.as_mut().filter(|store| store.root() == primary) {
            let out = apply(store, f);
            publish(store);
            return out;
        }
    }
    apply(&mut Store::open(root), f)
}

/// Where the issues page is to go.
#[derive(Clone, PartialEq, Debug)]
pub enum Target {
    List,
    Board,
    Epics,
    Issue(u64),
    Epic(String),
}

/// A request to put the issues page somewhere, with a filter and a grouping if it names them.
#[derive(Clone, PartialEq, Debug)]
pub struct Nav {
    pub target: Target,
    pub filter: Option<super::query::Filter>,
    pub group: Option<super::query::Group>,
}

static PENDING: Mutex<Option<Nav>> = Mutex::new(None);
static NUDGE: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::Sender::new(0));

/// Asks the page to go to `nav`, as soon as it is on screen.
pub fn navigate(nav: Nav) {
    *PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(nav);
    NUDGE.send_modify(|count| *count += 1);
}

/// The request waiting for the page, if there is one; the page takes it once.
pub fn take_nav() -> Option<Nav> {
    PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
}

/// Ticks whenever [`navigate`] is called.
pub fn nav_watch() -> watch::Receiver<u64> {
    NUDGE.subscribe()
}

/// How many issues are open, and how many of those are late, for the sidebar's row.
pub fn summary() -> (usize, usize) {
    let view = VIEW.borrow();
    (view.tracker.open_count(), view.tracker.late(today()))
}

/// Moves the issues linked to `branch` to Done: the branch was merged.
pub fn branch_merged(project: &Path, branch: &str) {
    change_repo(project, |tracker, now| {
        let linked: Vec<u64> = tracker.issues().filter(|issue| issue.status.is_open() && issue.links.iter().any(|link| matches!(link, super::Link::Branch(name) if name == branch))).map(|issue| issue.number).collect();
        for number in linked {
            tracker.update(number, now, |issue| issue.status = super::Status::Done);
        }
    });
}


#[cfg(test)]
pub(crate) mod testing {
    use std::sync::{Mutex, MutexGuard};

    /// The shared issues are one per process, so tests that use them take turns.
    static TURN: Mutex<()> = Mutex::new(());

    pub fn turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A folder with a repo's name and a `.git`, gone when this is dropped.
    pub struct Repo(pub std::path::PathBuf);

    impl Repo {
        pub fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!("po-shared-{}-{}", std::process::id(), name));
            let path = base.join(name);
            std::fs::create_dir_all(path.join(".git")).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Repo, turn};
    use super::*;
    use crate::issues::{Link, Status};

    #[test]
    fn a_change_first_reads_what_another_window_wrote() {
        let _turn = turn();
        let repo = Repo::new("shared-c");
        set_project(&repo.0);
        change(|t, now| t.create("mine", now)).unwrap();
        let prefix = VIEW.borrow().tracker.config.prefix.clone();
        // Another window made PO-5 and has not been heard from yet.
        std::fs::write(repo.0.join(format!(".po/issues/{prefix}-5.json")), r#"{"title":"theirs"}"#).unwrap();
        assert_eq!(change(|t, now| t.create("next", now)), Some(6));
        let view = VIEW.borrow().clone();
        assert_eq!(view.tracker.issues().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["mine", "theirs", "next"]);
        // Changing a repo that is not the one shown works on its own copy.
        let other = Repo::new("shared-d");
        assert_eq!(change_repo(&other.0, |t, now| t.create("elsewhere", now)), 1);
        assert_eq!(VIEW.borrow().tracker.issues().count(), 3);
    }

    #[test]
    fn changes_are_saved_and_followed_and_a_merged_branch_finishes_its_issues() {
        let _turn = turn();
        let repo = Repo::new("shared-a");
        assert_eq!(change(|_, _| ()).is_some(), VIEW.borrow().root.is_some(), "no project shown yet means nothing to change");
        set_project(&repo.0);
        let mut seen = watch();
        let (a, b) = change(|t, now| {
            let (a, b) = (t.create("a", now), t.create("b", now));
            t.update(a, now, |i| i.links = vec![Link::Branch("po/x".into())]);
            t.update(b, now, |i| i.links = vec![Link::Branch("po/y".into())]);
            (a, b)
        })
        .unwrap();
        assert!(seen.has_changed().unwrap());
        assert_eq!(seen.borrow_and_update().tracker.issues().count(), 2);
        assert_eq!(summary().0, 2);
        assert!(repo.0.join(format!(".po/issues/{}-1.json", seen.borrow().tracker.config.prefix)).exists());
        branch_merged(&repo.0, "po/x");
        let view = VIEW.borrow().clone();
        assert_eq!((view.tracker.issue(a).unwrap().status, view.tracker.issue(b).unwrap().status), (Status::Done, Status::Backlog));
        assert_eq!(summary().0, 1);
        // Showing the same repo again keeps what is there; another repo replaces it.
        set_project(&repo.0);
        assert_eq!(VIEW.borrow().tracker.issues().count(), 2);
        let other = Repo::new("shared-b");
        set_project(&other.0);
        assert_eq!(VIEW.borrow().tracker.issues().count(), 0);
        assert_eq!(VIEW.borrow().name, "shared-b");
    }
}
