//! The projects and threads of a window, kept so that the next `po` picks up where this one
//! left off, wherever it is started. There is one set for all of them; the directory `po` is
//! started in only decides which project is shown.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::worktree::Spec;

/// A thread that had an agent session, which is what it is resumed from.
#[derive(Clone)]
pub struct ThreadState {
    pub agent: String,
    pub session: String,
    pub title: String,
    /// When it last did anything, in milliseconds since the Unix epoch; 0 if unknown.
    pub active_at: u64,
    /// The session of the thread that started this one, which survives a restart where its id
    /// does not; the child is linked to it again when both come back in one window.
    pub parent_session: Option<String>,
}

pub struct ProjectState {
    pub root: PathBuf,
    /// It was showing changes rather than files.
    pub review: bool,
    /// Set for a thread's own checkout.
    pub worktree: Option<Spec>,
    /// Which of `threads` was on screen.
    pub active: usize,
    pub threads: Vec<ThreadState>,
    /// Threads closed in it, newest first, for the session picker to offer them back. Each is
    /// kept as the agent's session; `active_at` is when it last did anything.
    pub closed: Vec<ThreadState>,
}

/// How many closed threads a project keeps.
pub const CLOSED_KEPT: usize = 10;

#[derive(Default)]
pub struct Workspace {
    pub projects: Vec<ProjectState>,
    /// The viewer pane was open, rather than the conversation filling the middle.
    pub viewer_open: bool,
    /// The viewer was in the middle and the conversation on the right, rather than the reverse.
    pub swapped: bool,
    /// The conversation pane was open while it sat on the right, rather than collapsed. Only
    /// matters while it is there; in the middle it is always shown.
    pub agent_open: bool,
    /// The agent last chosen for a new thread.
    pub agent: Option<String>,
    /// The width, in pixels, of the file tree in the viewer, if dragged.
    pub explorer_width: Option<f32>,
    /// How wide the left side was, and the right side if it had been dragged to a width.
    pub left_width: Option<f32>,
    pub right_width: Option<f32>,
}

/// What a window last wrote: each project and the sessions of its threads. Anything in the file
/// beyond it was added by another window.
pub type Known = HashMap<PathBuf, HashSet<String>>;

fn file() -> PathBuf {
    crate::config::data_dir().join("workspace.json")
}

/// Where the window started in `start` was kept before there was one set for all of them.
fn legacy_file_in(dir: &Path, start: &Path) -> PathBuf {
    // FNV-1a: unlike the standard hasher it is the same in every build.
    let hash = start.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    let name = start.file_name().map_or_else(|| "root".to_string(), |n| n.to_string_lossy().to_string());
    dir.join(format!("{name}-{hash:016x}.json"))
}

/// A saved width in pixels, if it is a sensible one.
fn width(value: &Value) -> Option<f32> {
    value.as_f64().map(|w| w as f32).filter(|w| w.is_finite() && (50.0..=4000.0).contains(w))
}

fn path(value: &Value) -> Option<PathBuf> {
    value.as_str().map(PathBuf::from)
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(String::from)
}

impl Workspace {
    fn to_json(&self) -> Value {
        let projects: Vec<Value> = self
            .projects
            .iter()
            .map(|project| {
                json!({
                    "root": project.root,
                    "review": project.review,
                    "worktree": project.worktree.as_ref().map(|spec| json!({
                        "path": spec.path, "branch": spec.branch, "base": spec.base, "repo": spec.repo,
                    })),
                    "active": project.active,
                    "threads": project.threads.iter().map(|t| json!({
                        "agent": t.agent, "session": t.session, "title": t.title, "active_at": t.active_at, "parent_session": t.parent_session,
                    })).collect::<Vec<_>>(),
                    "closed": project.closed.iter().map(|t| json!({
                        "agent": t.agent, "session": t.session, "title": t.title, "active_at": t.active_at,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "version": 1, "viewer_open": self.viewer_open, "swapped": self.swapped, "agent_open": self.agent_open, "agent": self.agent, "explorer_width": self.explorer_width, "left_width": self.left_width, "right_width": self.right_width, "projects": projects })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let projects = value["projects"]
            .as_array()?
            .iter()
            .filter_map(|entry| {
                let worktree = match entry["worktree"].is_object() {
                    true => Some(Spec {
                        path: path(&entry["worktree"]["path"])?,
                        branch: text(&entry["worktree"]["branch"])?,
                        base: text(&entry["worktree"]["base"])?,
                        repo: path(&entry["worktree"]["repo"])?,
                    }),
                    false => None,
                };
                let threads = entry["threads"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| {
                        Some(ThreadState { agent: text(&t["agent"])?, session: text(&t["session"])?, title: text(&t["title"]).unwrap_or_default(), active_at: t["active_at"].as_u64().unwrap_or(0), parent_session: text(&t["parent_session"]) })
                    })
                    .collect();
                // The file can be written by anything, so its size is not trusted either.
                let closed = entry["closed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(CLOSED_KEPT)
                    .filter_map(|t| Some(ThreadState { agent: text(&t["agent"])?, session: text(&t["session"])?, title: text(&t["title"]).unwrap_or_default(), active_at: t["active_at"].as_u64().unwrap_or(0), parent_session: None }))
                    .collect();
                Some(ProjectState {
                    root: path(&entry["root"])?,
                    review: entry["review"].as_bool().unwrap_or(false),
                    worktree,
                    active: entry["active"].as_u64().unwrap_or(0) as usize,
                    threads,
                    closed,
                })
            })
            .collect();
        Some(Self { projects, // Files saved before the views were merged only said which layout the window was in.
            viewer_open: value["viewer_open"].as_bool().unwrap_or(value["layout"].as_str() != Some("agents")),
            swapped: value["swapped"].as_bool().unwrap_or(false),
            agent_open: value["agent_open"].as_bool().unwrap_or(true),
            agent: text(&value["agent"]),
            // A saved width that isn't a sensible number is dropped rather than trusted.
            explorer_width: width(&value["explorer_width"]),
            left_width: width(&value["left_width"]),
            right_width: width(&value["right_width"]),
        })
    }

    /// What was saved last, if anything. `start` is where this window was started: before there
    /// was one set for all windows, each directory kept its own, and those are picked up once.
    #[hotpath::measure(impl_type = "Workspace")]
    pub fn load(start: &Path) -> Option<Self> {
        Self::load_from(&file(), &crate::config::data_dir().join("workspaces"), start)
    }

    fn load_from(target: &Path, legacy: &Path, start: &Path) -> Option<Self> {
        if target.exists() {
            return Self::from_json(&crate::config::read_json(target)?);
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(legacy).into_iter().flatten().flatten().map(|entry| entry.path()).collect();
        files.sort();
        // The one for this directory is the base, so its layout is the one kept.
        let own = legacy_file_in(legacy, start);
        files.retain(|file| *file != own);
        files.insert(0, own);
        let mut base: Option<Value> = None;
        for file in files {
            let Some(value) = crate::config::read_json(&file) else { continue };
            match &mut base {
                None => base = Some(value),
                Some(base) => {
                    let known: HashSet<Option<String>> = base["projects"].as_array().into_iter().flatten().map(|p| text(&p["root"])).collect();
                    let more: Vec<Value> = value["projects"].as_array().into_iter().flatten().filter(|p| !known.contains(&text(&p["root"]))).cloned().collect();
                    if let Some(projects) = base["projects"].as_array_mut() {
                        projects.extend(more);
                    }
                }
            }
        }
        Self::from_json(&base?)
    }

    /// What this window has now, for telling it from what other windows add.
    pub fn known(&self) -> Known {
        self.projects.iter().map(|project| (project.root.clone(), project.threads.iter().map(|t| t.session.clone()).collect())).collect()
    }

    /// Saves it, replacing the file in one step so that a crash can't leave half of one.
    /// Projects and threads other windows saved since this one last did (those not in `known`)
    /// are kept, and ones in `known` that are no longer open here were closed here.
    #[hotpath::measure(impl_type = "Workspace")]
    pub fn save(&self, known: &mut Known) {
        let target = file();
        if let Err(error) = self.save_to(&target, known) {
            log::warn!("saving {}: {error}", target.display());
        }
    }

    fn save_to(&self, target: &Path, known: &mut Known) -> std::io::Result<()> {
        let dir = target.parent().ok_or_else(|| std::io::Error::other("no directory"))?;
        std::fs::create_dir_all(dir)?;
        // Windows saving at once take turns, so none reads the file before another has written it.
        let lock = std::fs::File::create(target.with_extension("lock"))?;
        lock.lock()?;
        let mut value = self.to_json();
        let saved = std::fs::read_to_string(target).ok().map(|text| serde_json::from_str::<Value>(&text));
        let disk = match saved {
            Some(Ok(saved)) => saved["projects"].as_array().cloned().unwrap_or_default(),
            Some(Err(error)) => {
                // Keep what can't be read rather than writing over it.
                log::error!("{}: {error}; kept as a .bak", target.display());
                std::fs::copy(target, target.with_extension("json.bak"))?;
                vec![]
            }
            None => vec![],
        };
        let ours = value["projects"].as_array().cloned().unwrap_or_default();
        value["projects"] = Value::Array(merge(ours, disk, known));
        let temp = target.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&temp, serde_json::to_vec_pretty(&value).unwrap_or_default())?;
        std::fs::rename(&temp, target)?;
        *known = self.known();
        Ok(())
    }
}

/// `ours` in the order the file has them, with what other windows added beside them: projects in
/// `disk` that `known` doesn't have, and threads in a project that it doesn't have. What `known`
/// has but `ours` doesn't was closed here. Projects whose directory is gone are let go.
fn merge(ours: Vec<Value>, disk: Vec<Value>, known: &Known) -> Vec<Value> {
    let mut ours: Vec<Option<Value>> = ours.into_iter().map(Some).collect();
    let mut merged = vec![];
    for entry in disk {
        let Some(root) = path(&entry["root"]) else { continue };
        let mine = ours.iter_mut().find(|mine| mine.as_ref().is_some_and(|mine| path(&mine["root"]).as_ref() == Some(&root)));
        match mine.and_then(Option::take) {
            Some(mut mine) => {
                let had = known.get(&root);
                let mut sessions: HashSet<String> = mine["threads"].as_array().into_iter().flatten().filter_map(|t| text(&t["session"])).collect();
                let added: Vec<Value> = entry["threads"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| text(&t["session"]).is_some_and(|session| !had.is_some_and(|had| had.contains(&session)) && sessions.insert(session)))
                    .cloned()
                    .collect();
                if let Some(threads) = mine["threads"].as_array_mut() {
                    threads.extend(added);
                }
                merged.push(mine);
            }
            None if !known.contains_key(&root) && root.is_dir() => merged.push(entry),
            None => {}
        }
    }
    merged.extend(ours.into_iter().flatten());
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_survives_a_round_trip() {
        let saved = Workspace {
            viewer_open: false,
            swapped: true,
            agent_open: false,
            agent: Some("dex".into()),
            explorer_width: Some(240.0),
            left_width: Some(300.0),
            right_width: Some(640.0),
            projects: vec![
                ProjectState {
                    root: "/work/po".into(),
                    review: false,
                    worktree: None,
                    active: 1,
                    threads: vec![
                        ThreadState { agent: "dex".into(), session: "s1".into(), title: "fix the retry".into(), active_at: 1_700_000_000_000, parent_session: None },
                        ThreadState { agent: "opencode".into(), session: "s2".into(), title: "quote \"this\"".into(), active_at: 0, parent_session: Some("s1".into()) },
                    ],
                    closed: vec![ThreadState { agent: "dex".into(), session: "s0".into(), title: "old idea".into(), active_at: 5, parent_session: None }],
                },
                ProjectState {
                    root: "/data/worktrees/po-1".into(),
                    review: true,
                    worktree: Some(Spec { path: "/data/worktrees/po-1".into(), branch: "po/1".into(), base: "abc".into(), repo: "/work/po".into() }),
                    active: 0,
                    threads: vec![],
                    closed: vec![],
                },
            ],
        };
        let back = Workspace::from_json(&saved.to_json()).expect("parses");
        assert!(!back.viewer_open);
        assert!(back.swapped);
        assert!(!back.agent_open);
        assert_eq!(back.agent.as_deref(), Some("dex"));
        assert_eq!(back.explorer_width, Some(240.0));
        assert_eq!((back.left_width, back.right_width), (Some(300.0), Some(640.0)));
        assert_eq!(back.projects.len(), 2);
        assert_eq!(back.projects[0].active, 1);
        assert_eq!(back.projects[0].threads[1].title, "quote \"this\"");
        assert_eq!((back.projects[0].closed.len(), back.projects[0].closed[0].session.as_str()), (1, "s0"));
        let wt = back.projects[1].worktree.as_ref().expect("worktree");
        assert_eq!((wt.branch.as_str(), wt.base.as_str()), ("po/1", "abc"));
        assert!(back.projects[1].review);
    }

    #[test]
    fn files_from_before_the_views_were_merged_still_say_whether_the_viewer_was_open() {
        let old_agents = Workspace::from_json(&json!({ "layout": "agents", "projects": [] })).expect("parses");
        assert!(!old_agents.viewer_open);
        let old_files = Workspace::from_json(&json!({ "layout": "files", "projects": [] })).expect("parses");
        assert!(old_files.viewer_open);
        let neither = Workspace::from_json(&json!({ "projects": [] })).expect("parses");
        assert!(neither.viewer_open);
    }

    #[test]
    fn a_file_with_too_many_closed_threads_keeps_the_newest() {
        let closed: Vec<Value> = (0..50).map(|n| json!({ "agent": "a", "session": format!("s{n}") })).collect();
        let back = Workspace::from_json(&json!({ "projects": [ { "root": "/x", "closed": closed } ] })).expect("parses");
        assert_eq!(back.projects[0].closed.len(), CLOSED_KEPT);
        assert_eq!(back.projects[0].closed[0].session, "s0");
    }

    #[test]
    fn nonsense_widths_are_dropped() {
        for bad in [json!(-5), json!(0), json!(1), json!(1e9), json!("wide"), Value::Null] {
            let back = Workspace::from_json(&json!({ "projects": [], "left_width": bad, "right_width": bad })).expect("parses");
            assert_eq!((back.left_width, back.right_width), (None, None));
        }
    }

    #[test]
    fn damaged_entries_are_skipped() {
        let value = json!({ "projects": [ { "review": true }, { "root": "/x", "threads": [ { "agent": "dex" }, { "agent": "a", "session": "s" } ] } ] });
        let back = Workspace::from_json(&value).expect("parses");
        assert_eq!(back.projects.len(), 1);
        assert_eq!(back.projects[0].threads.len(), 1);
    }

    fn project(root: &str, sessions: &[&str]) -> ProjectState {
        let threads = sessions.iter().map(|s| ThreadState { agent: "a".into(), session: s.to_string(), title: String::new(), active_at: 1, parent_session: None }).collect();
        ProjectState { root: root.into(), review: false, worktree: None, active: 0, threads, closed: vec![] }
    }

    fn window(projects: Vec<ProjectState>) -> Workspace {
        Workspace { projects, ..Default::default() }
    }

    /// The projects, each with its sessions, as the file has them.
    fn contents(target: &Path) -> Vec<(String, Vec<String>)> {
        let saved = Workspace::load_from(target, Path::new("/none"), Path::new("/")).expect("saved");
        saved.projects.iter().map(|p| (p.root.to_string_lossy().to_string(), p.threads.iter().map(|t| t.session.clone()).collect())).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("po-ws-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn row(root: &str, sessions: &[&str]) -> (String, Vec<String>) {
        (root.to_string(), sessions.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn projects_of_other_windows_are_kept_when_saving() {
        let dir = scratch("projects");
        let target = dir.join("workspace.json");
        let (mut a, mut b) = (Known::new(), Known::new());
        window(vec![project("/tmp", &[]), project("/usr", &[])]).save_to(&target, &mut a).unwrap();
        window(vec![project("/bin", &[])]).save_to(&target, &mut b).unwrap();
        // The order the file has is kept, and what was added goes after it.
        assert_eq!(contents(&target), [row("/tmp", &[]), row("/usr", &[]), row("/bin", &[])]);
        // A closes /usr; B's /bin and A's /tmp stay.
        window(vec![project("/tmp", &[])]).save_to(&target, &mut a).unwrap();
        assert_eq!(contents(&target), [row("/tmp", &[]), row("/bin", &[])]);
        // B opens /usr, which A closed; A saving again doesn't take it away.
        window(vec![project("/bin", &[]), project("/usr", &["u"])]).save_to(&target, &mut b).unwrap();
        window(vec![project("/tmp", &[])]).save_to(&target, &mut a).unwrap();
        assert_eq!(contents(&target), [row("/tmp", &[]), row("/bin", &[]), row("/usr", &["u"])]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn threads_of_other_windows_are_kept_in_a_shared_project() {
        let dir = scratch("threads");
        let target = dir.join("workspace.json");
        let (mut a, mut b) = (Known::new(), Known::new());
        window(vec![project("/tmp", &["one"])]).save_to(&target, &mut a).unwrap();
        b.extend(a.clone());
        // A starts a thread; B, which hasn't seen it, saves with its own.
        window(vec![project("/tmp", &["one", "a2"])]).save_to(&target, &mut a).unwrap();
        window(vec![project("/tmp", &["one", "b2"])]).save_to(&target, &mut b).unwrap();
        assert_eq!(contents(&target), [row("/tmp", &["one", "b2", "a2"])]);
        // A closes "one" and it stays closed.
        window(vec![project("/tmp", &["a2"])]).save_to(&target, &mut a).unwrap();
        assert_eq!(contents(&target), [row("/tmp", &["a2", "b2"])]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_unreadable_file_is_kept_as_a_backup() {
        let dir = scratch("corrupt");
        let target = dir.join("workspace.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&target, "{ not json").unwrap();
        window(vec![project("/tmp", &[])]).save_to(&target, &mut Known::new()).unwrap();
        assert_eq!(std::fs::read_to_string(target.with_extension("json.bak")).unwrap(), "{ not json");
        assert_eq!(contents(&target), [row("/tmp", &[])]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn files_from_before_one_set_for_all_windows_are_all_picked_up() {
        let dir = scratch("legacy");
        let legacy = dir.join("workspaces");
        std::fs::create_dir_all(&legacy).unwrap();
        let start = Path::new("/tmp");
        let write = |name: PathBuf, roots: &[&str]| {
            let projects: Vec<Value> = roots.iter().map(|r| json!({ "root": r })).collect();
            std::fs::write(name, json!({ "projects": projects, "left_width": 300.0 }).to_string()).unwrap();
        };
        write(legacy_file_in(&legacy, start), &["/tmp"]);
        write(legacy.join("other.json"), &["/usr", "/tmp"]);
        let loaded = Workspace::load_from(&dir.join("workspace.json"), &legacy, start).expect("loads");
        assert_eq!(loaded.projects.iter().map(|p| p.root.to_string_lossy().to_string()).collect::<Vec<_>>(), ["/tmp", "/usr"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn windows_for_different_directories_have_different_files() {
        let d = Path::new("/d");
        assert_ne!(legacy_file_in(d, Path::new("/a/po")), legacy_file_in(d, Path::new("/b/po")));
        assert_eq!(legacy_file_in(d, Path::new("/a/po")), legacy_file_in(d, Path::new("/a/po")));
    }
}
