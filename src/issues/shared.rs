//! The issues of every open project, shared by the page that edits them, the sidebar that lists
//! them and the API. Each repo has one store, whichever of its checkouts are open; one of them is
//! the repo the page shows, chosen when its issues are opened. Every change is saved as it is made,
//! and files changed underneath (git, another window) are picked up by a poller.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::thread::Thread;
use std::time::Duration;

use tokio::sync::watch;

use super::store::{Store, fingerprint, load, primary_root};
use super::{Config, Tracker};
use crate::date::Date;

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
        Self {
            root: None,
            name: String::new(),
            tracker: Tracker::new(Config::new("ISS".into())),
            problems: vec![],
        }
    }
}

/// One repo's issues, as the page and the sidebar see them.
#[derive(Clone, PartialEq, Debug)]
pub struct RepoView {
    /// The repo's primary checkout.
    pub root: PathBuf,
    /// The repo's folder name.
    pub name: String,
    pub tracker: Tracker,
    pub problems: Vec<String>,
}

/// The issues of every repo followed, by name.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Overview {
    /// Each repo followed, with the checkouts of it that are open.
    pub checkouts: BTreeMap<PathBuf, Vec<PathBuf>>,
    pub repos: Vec<RepoView>,
}

impl Overview {
    /// How many issues are open, and how many of those are due before `today`.
    pub fn counts(&self, today: Date) -> (usize, usize) {
        let open = || {
            self.repos
                .iter()
                .flat_map(|repo| repo.tracker.issues())
                .filter(|issue| issue.status.is_open())
        };
        (
            open().count(),
            open()
                .filter(|issue| issue.due.is_some_and(|due| due < today))
                .count(),
        )
    }
}

/// The stores, and what they are held for.
struct Held {
    stores: BTreeMap<PathBuf, Store>,
    /// The repo the page shows. It is held even if no open project wants it any more.
    shown: Option<PathBuf>,
    /// The checkouts of the open projects, as [`track`] was told them.
    wanted: Vec<PathBuf>,
    /// `wanted` by repo, as the poller last worked it out.
    checkouts: BTreeMap<PathBuf, Vec<PathBuf>>,
}

static HELD: Mutex<Held> = Mutex::new(Held {
    stores: BTreeMap::new(),
    shown: None,
    wanted: Vec::new(),
    checkouts: BTreeMap::new(),
});
static VIEW: LazyLock<watch::Sender<View>> = LazyLock::new(|| watch::Sender::new(View::empty()));
static OVERVIEW: LazyLock<watch::Sender<Overview>> =
    LazyLock::new(|| watch::Sender::new(Overview::default()));
static POLLER: OnceLock<Thread> = OnceLock::new();

const POLL_EVERY: Duration = Duration::from_millis(1500);

fn lock() -> MutexGuard<'static, Held> {
    HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[hotpath::measure]
fn publish_view(store: &Store) {
    let name = store
        .root()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let view = View {
        root: Some(store.root().to_path_buf()),
        name,
        tracker: store.tracker.clone(),
        problems: store.problems.clone(),
    };
    VIEW.send_if_modified(|now| {
        let changed = *now != view;
        if changed {
            *now = view;
        }
        changed
    });
}

/// Tells the screens what one repo now holds, if that changed: a change to one repo has no need to
/// copy and compare all the others. A repo the overview doesn't list yet goes through
/// [`publish_overview`].
fn publish_repo(held: &Held, root: &Path) {
    let Some(store) = held.stores.get(root) else {
        return;
    };
    let listed = OVERVIEW.borrow().repos.iter().any(|repo| repo.root == root);
    if !listed {
        return publish_overview(held);
    }
    OVERVIEW.send_if_modified(|now| {
        let Some(repo) = now.repos.iter_mut().find(|repo| repo.root == root) else {
            return false;
        };
        if repo.tracker == store.tracker && repo.problems == store.problems {
            return false;
        }
        repo.tracker = store.tracker.clone();
        repo.problems = store.problems.clone();
        true
    });
}

/// Tells the screens what the repos followed now hold, if that changed.
#[hotpath::measure]
fn publish_overview(held: &Held) {
    let mut repos: Vec<RepoView> = held
        .checkouts
        .keys()
        .filter_map(|root| held.stores.get(root))
        .map(|store| RepoView {
            root: store.root().to_path_buf(),
            name: store
                .root()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            tracker: store.tracker.clone(),
            problems: store.problems.clone(),
        })
        .collect();
    repos.sort_by(|a, b| (&a.name, &a.root).cmp(&(&b.name, &b.root)));
    let overview = Overview {
        checkouts: held.checkouts.clone(),
        repos,
    };
    OVERVIEW.send_if_modified(|now| {
        let changed = *now != overview;
        if changed {
            *now = overview;
        }
        changed
    });
}

/// The issues of the repo the page shows, now and after every change.
pub fn watch() -> watch::Receiver<View> {
    VIEW.subscribe()
}

/// The issues of every open project, now and after every change.
pub fn overview() -> watch::Receiver<Overview> {
    OVERVIEW.subscribe()
}

/// The thread that polls, started the first time something is held.
fn poller() -> &'static Thread {
    POLLER.get_or_init(|| {
        std::thread::Builder::new()
            .name("issues-poll".into())
            .spawn(|| {
                loop {
                    poll();
                    std::thread::park_timeout(POLL_EVERY);
                }
            })
            .expect("failed to spawn thread")
            .thread()
            .clone()
    })
}

/// Follows the issues of `checkouts`, the open projects, and of no others: the poller reads what is
/// new off the caller's thread and lets go of what is no longer open. Cheap when nothing changed.
pub fn track(checkouts: Vec<PathBuf>) {
    {
        let mut held = lock();
        if held.wanted == checkouts {
            return;
        }
        held.wanted = checkouts;
    }
    poller().unpark();
}

/// Shows the issues of the repo `project` is a checkout of on the page. Cheap when it is the one
/// already shown.
pub fn set_project(project: &Path) {
    let root = primary_root(project);
    let (shown, known) = {
        let held = lock();
        (
            held.shown.as_ref() == Some(&root),
            held.stores.contains_key(&root),
        )
    };
    if shown {
        return;
    }
    // The folder is read before the lock is taken, so a slow disk holds up no one else.
    let opened = (!known).then(|| Store::open(project));
    let mut held = lock();
    let held = &mut *held;
    if let Some(opened) = opened {
        held.stores.entry(root.clone()).or_insert(opened);
    }
    if let Some(store) = held.stores.get(&root) {
        publish_view(store);
        held.shown = Some(root);
    }
    poller();
}

/// Reads in the repos newly wanted, notices files that changed underneath (git, another window) and
/// lets go of what is no longer wanted. The checking and the reading are done without the lock; it
/// is held only to see that nothing happened to a store meanwhile and to swap the reading in.
#[hotpath::measure]
pub(crate) fn poll() {
    let (wanted, known, states) = {
        let held = lock();
        (
            held.wanted.clone(),
            held.stores.keys().cloned().collect::<Vec<_>>(),
            held.stores
                .values()
                .map(Store::watch_state)
                .collect::<Vec<_>>(),
        )
    };
    let mut checkouts: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in wanted {
        checkouts.entry(primary_root(&path)).or_default().push(path);
    }
    let opened: Vec<Store> = checkouts
        .keys()
        .filter(|root| !known.contains(root))
        .map(|root| Store::open(root))
        .collect();
    let readings: Vec<_> = states
        .into_iter()
        .filter(|(_, dir, _, seen)| fingerprint(dir) != *seen)
        .map(|(root, dir, prefix, seen)| {
            let (loaded, read_at) = load(&dir, &prefix);
            (root, loaded, seen, read_at)
        })
        .collect();

    let mut guard = lock();
    let held = &mut *guard;
    let mut changed = !opened.is_empty() || held.checkouts != checkouts;
    for store in opened {
        held.stores
            .entry(store.root().to_path_buf())
            .or_insert(store);
    }
    // By the repo and not the folder: two clones share a folder, and each has a store of its own.
    for (root, loaded, seen, read_at) in readings {
        if let Some(store) = held.stores.get_mut(&root)
            && store.install(loaded, seen, read_at)
        {
            changed = true;
            if held.shown.as_deref() == Some(store.root()) {
                publish_view(store);
            }
        }
    }
    let shown = held.shown.clone();
    held.stores
        .retain(|root, _| checkouts.contains_key(root) || shown.as_ref() == Some(root));
    held.checkouts = checkouts;
    if changed {
        publish_overview(held);
    }
}

/// Changes the issues the page shows with `f`, which is given the time to stamp them with, saves,
/// and tells whoever follows them. `None` when no repo is shown. Only tests use it: the page and
/// the API name the repo they change.
#[cfg(test)]
pub fn change<R>(f: impl FnOnce(&mut Tracker, u64) -> R) -> Option<R> {
    let mut guard = lock();
    let root = guard.shown.clone()?;
    change_held(&mut guard, &root, f).ok()
}

/// Changes the store of `root` and tells whoever follows it; gives `f` back if it isn't held.
fn change_held<R, F: FnOnce(&mut Tracker, u64) -> R>(
    held: &mut Held,
    root: &Path,
    f: F,
) -> Result<R, F> {
    let Some(store) = held.stores.get_mut(root) else {
        return Err(f);
    };
    let out = apply(store, f);
    if held.shown.as_deref() == Some(root) {
        publish_view(&held.stores[root]);
    }
    publish_repo(held, root);
    Ok(out)
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

/// Changes the issues of the repo `root` belongs to with `f`. A repo held is changed through the
/// shared store, so the page and the sidebar see it at once; any other is opened, changed and saved
/// on its own.
pub fn change_repo<R>(root: &Path, f: impl FnOnce(&mut Tracker, u64) -> R) -> R {
    let primary = primary_root(root);
    let f = match change_held(&mut lock(), &primary, f) {
        Ok(out) => return out,
        Err(f) => f,
    };
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
    *PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(nav);
    NUDGE.send_modify(|count| *count += 1);
}

/// The request waiting for the page, if there is one; the page takes it once.
pub fn take_nav() -> Option<Nav> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Moves the issues linked to `branch` to Done: the branch was merged.
pub fn branch_merged(project: &Path, branch: &str) {
    change_repo(project, |tracker, now| {
        let linked: Vec<u64> = tracker
            .issues()
            .filter(|issue| {
                issue.status.is_open()
                    && issue
                        .links
                        .iter()
                        .any(|link| matches!(link, super::Link::Branch(name) if name == branch))
            })
            .map(|issue| issue.number)
            .collect();
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
            let base =
                std::env::temp_dir().join(format!("po-shared-{}-{}", std::process::id(), name));
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
        std::fs::write(
            repo.0.join(format!(".po/issues/{prefix}-5.json")),
            r#"{"title":"theirs"}"#,
        )
        .unwrap();
        assert_eq!(change(|t, now| t.create("next", now)), Some(6));
        let view = VIEW.borrow().clone();
        assert_eq!(
            view.tracker
                .issues()
                .map(|i| i.title.as_str())
                .collect::<Vec<_>>(),
            ["mine", "theirs", "next"]
        );
        // Changing a repo that is not the one shown works on its own copy.
        let other = Repo::new("shared-d");
        assert_eq!(
            change_repo(&other.0, |t, now| t.create("elsewhere", now)),
            1
        );
        assert_eq!(VIEW.borrow().tracker.issues().count(), 3);
    }

    #[test]
    fn changes_are_saved_and_followed_and_a_merged_branch_finishes_its_issues() {
        let _turn = turn();
        let repo = Repo::new("shared-a");
        assert_eq!(
            change(|_, _| ()).is_some(),
            VIEW.borrow().root.is_some(),
            "no project shown yet means nothing to change"
        );
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
        assert!(
            repo.0
                .join(format!(
                    ".po/issues/{}-1.json",
                    seen.borrow().tracker.config.prefix
                ))
                .exists()
        );
        branch_merged(&repo.0, "po/x");
        let view = VIEW.borrow().clone();
        assert_eq!(
            (
                view.tracker.issue(a).unwrap().status,
                view.tracker.issue(b).unwrap().status
            ),
            (Status::Done, Status::Backlog)
        );
        // Showing the same repo again keeps what is there; another repo replaces it.
        set_project(&repo.0);
        assert_eq!(VIEW.borrow().tracker.issues().count(), 2);
        let other = Repo::new("shared-b");
        set_project(&other.0);
        assert_eq!(VIEW.borrow().tracker.issues().count(), 0);
        assert_eq!(VIEW.borrow().name, "shared-b");
    }

    /// Runs the poller's round here, so a test need not wait for it.
    fn tracked(checkouts: &[&Repo]) -> Overview {
        track(checkouts.iter().map(|repo| repo.0.clone()).collect());
        poll();
        overview().borrow().clone()
    }

    /// The titles of the open issues of each repo, by repo name.
    fn titles(overview: &Overview) -> Vec<(String, Vec<String>)> {
        overview
            .repos
            .iter()
            .map(|repo| {
                (
                    repo.name.clone(),
                    repo.tracker
                        .issues()
                        .filter(|i| i.status.is_open())
                        .map(|i| i.title.clone())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn every_open_project_is_held_whichever_the_page_shows() {
        let _turn = turn();
        let (one, two) = (Repo::new("over-one"), Repo::new("over-two"));
        // Neither has been shown; what is on disk is read and what happens underneath is noticed.
        change_repo(&one.0, |t, now| t.create("first", now));
        let held = tracked(&[&one, &two]);
        assert_eq!(
            titles(&held),
            [
                ("over-one".to_string(), vec!["first".to_string()]),
                ("over-two".to_string(), vec![])
            ]
        );
        assert_eq!(held.checkouts[&two.0], std::slice::from_ref(&two.0));
        change_repo(&two.0, |t, now| t.create("second", now));
        assert_eq!(
            overview().borrow().counts(crate::date::today()).0,
            2,
            "a change through the store is seen at once"
        );
        let prefix = std::fs::read_dir(two.0.join(".po/issues"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .split('-')
            .next()
            .unwrap()
            .to_string();
        std::fs::write(
            two.0.join(format!(".po/issues/{prefix}-2.json")),
            r#"{"title":"from git"}"#,
        )
        .unwrap();
        assert_eq!(tracked(&[&one, &two]).counts(crate::date::today()).0, 3);

        // What is finished is not counted; showing one repo on the page leaves the other alone.
        change_repo(&one.0, |t, now| {
            t.update(1, now, |i| i.status = Status::Done)
        });
        assert_eq!(overview().borrow().counts(crate::date::today()).0, 2);
        set_project(&two.0);
        assert_eq!(VIEW.borrow().name, "over-two");
        assert_eq!(tracked(&[&one, &two]).repos.len(), 2);

        // A change to a repo not on the page does not reach the page.
        let before = VIEW.borrow().clone();
        change_repo(&one.0, |t, now| t.create("elsewhere", now));
        assert_eq!(*VIEW.borrow(), before);
        assert_eq!(overview().borrow().counts(crate::date::today()).0, 3);

        // A project closed stops being held, once the page has moved on from it.
        assert_eq!(tracked(&[&one]).repos.len(), 1);
        assert_eq!(change(|t, _| t.issues().count()), Some(2));
        set_project(&one.0);
        poll();
        assert!(
            !lock().stores.contains_key(&two.0),
            "let go of once the page has moved on"
        );
        track(vec![]);
        poll();
    }

    #[test]
    fn late_issues_are_counted_among_the_open_ones() {
        let _turn = turn();
        let repo = Repo::new("over-late");
        tracked(&[&repo]);
        let today = crate::date::today();
        change_repo(&repo.0, |t, now| {
            let (a, b, c) = (t.create("a", now), t.create("b", now), t.create("c", now));
            t.update(a, now, |i| i.due = Some(today.plus(-1)));
            t.update(b, now, |i| i.due = Some(today));
            t.update(c, now, |i| {
                (i.due, i.status) = (Some(today.plus(-3)), Status::Done)
            });
        });
        assert_eq!(overview().borrow().counts(today), (2, 1));
        track(vec![]);
        poll();
    }

    #[test]
    fn two_checkouts_of_a_repo_share_one_store() {
        let _turn = turn();
        let repo = Repo::new("over-main");
        let worktree = repo.0.parent().unwrap().join("linked");
        let git_dir = repo.0.join(".git/worktrees/linked");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
        change_repo(&repo.0, |t, now| t.create("one", now));
        track(vec![repo.0.clone(), worktree.clone()]);
        poll();
        let now = overview().borrow().clone();
        assert_eq!(now.repos.len(), 1, "held once");
        assert_eq!(now.checkouts[&repo.0], [repo.0.clone(), worktree]);
        track(vec![]);
        poll();
    }
}
