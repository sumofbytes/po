//! The projects and threads of a window, kept so that the next `po` picks up where this one
//! left off, wherever it is started. There is one set for all of them; the directory `po` is
//! started in only decides which project is shown.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::worktree::Spec;

/// A thread that had an agent session, which is what it is resumed from.
pub struct ThreadState {
    pub agent: String,
    pub session: String,
    pub title: String,
    /// When it last did anything, in milliseconds since the Unix epoch; 0 if unknown.
    pub active_at: u64,
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
}

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
    /// The height, in pixels, the projects list was dragged to, if it was.
    pub split: Option<f32>,
    /// The height, in pixels, of the live agents list above the projects, if dragged.
    pub live_split: Option<f32>,
    /// The live agents list and the projects list were folded down to their headers.
    pub live_folded: bool,
    pub projects_folded: bool,
    /// The file tree in the viewer was hidden.
    pub explorer_collapsed: bool,
    /// The width, in pixels, of the file tree in the viewer, if dragged.
    pub explorer_width: Option<f32>,
    /// How wide the left side was, and the right side if it had been dragged to a width.
    pub left_width: Option<f32>,
    pub right_width: Option<f32>,
}

/// The file for the window started in `start`.
fn file() -> PathBuf {
    crate::config::data_dir().join("workspace.json")
}

/// Where the window started in `start` was kept before there was one set for all of them.
fn legacy_file(start: &Path) -> PathBuf {
    // FNV-1a: unlike the standard hasher it is the same in every build.
    let hash = start.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    let name = start.file_name().map_or_else(|| "root".to_string(), |n| n.to_string_lossy().to_string());
    crate::config::data_dir().join("workspaces").join(format!("{name}-{hash:016x}.json"))
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
                        "agent": t.agent, "session": t.session, "title": t.title, "active_at": t.active_at,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "version": 1, "viewer_open": self.viewer_open, "swapped": self.swapped, "agent_open": self.agent_open, "agent": self.agent, "split": self.split, "live_split": self.live_split, "live_folded": self.live_folded, "projects_folded": self.projects_folded, "explorer_collapsed": self.explorer_collapsed, "explorer_width": self.explorer_width, "left_width": self.left_width, "right_width": self.right_width, "projects": projects })
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
                        Some(ThreadState { agent: text(&t["agent"])?, session: text(&t["session"])?, title: text(&t["title"]).unwrap_or_default(), active_at: t["active_at"].as_u64().unwrap_or(0) })
                    })
                    .collect();
                Some(ProjectState {
                    root: path(&entry["root"])?,
                    review: entry["review"].as_bool().unwrap_or(false),
                    worktree,
                    active: entry["active"].as_u64().unwrap_or(0) as usize,
                    threads,
                })
            })
            .collect();
        Some(Self { projects, // Files saved before the views were merged only said which layout the window was in.
            viewer_open: value["viewer_open"].as_bool().unwrap_or(value["layout"].as_str() != Some("agents")),
            swapped: value["swapped"].as_bool().unwrap_or(false),
            agent_open: value["agent_open"].as_bool().unwrap_or(true),
            agent: text(&value["agent"]),
            // A saved height that isn't a sensible number is dropped rather than trusted.
            explorer_collapsed: value["explorer_collapsed"].as_bool().unwrap_or(false),
            explorer_width: width(&value["explorer_width"]),
            left_width: width(&value["left_width"]),
            right_width: width(&value["right_width"]),
            live_folded: value["live_folded"].as_bool().unwrap_or(false),
            projects_folded: value["projects_folded"].as_bool().unwrap_or(false),
            live_split: value["live_split"].as_f64().map(|height| height as f32).filter(|height| height.is_finite() && *height > 0.),
            split: value["split"].as_f64().map(|height| height as f32).filter(|height| height.is_finite() && *height > 0.),
        })
    }

    /// What the window started in `start` saved last, if anything.
    pub fn load(start: &Path) -> Option<Self> {
        let value = crate::config::read_json(&file()).or_else(|| crate::config::read_json(&legacy_file(start)))?;
        Self::from_json(&value)
    }

    /// Saves it, replacing the file in one step so that a crash can't leave half of one.
    /// Projects other windows saved are kept: `known` holds every project this window has had,
    /// so one in the file that isn't in it belongs to another window, and one that is but is no
    /// longer open was closed here.
    pub fn save(&self, known: &mut HashSet<PathBuf>) {
        let target = file();
        let Some(dir) = target.parent() else { return };
        known.extend(self.projects.iter().map(|project| project.root.clone()));
        let mut value = self.to_json();
        let others = crate::config::read_json(&target).and_then(|saved| saved["projects"].as_array().cloned()).unwrap_or_default();
        if let Some(projects) = value["projects"].as_array_mut() {
            projects.extend(others.into_iter().filter(|entry| path(&entry["root"]).is_some_and(|root| !known.contains(&root))));
        }
        let temp = target.with_extension("json.tmp");
        let written = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&temp, serde_json::to_vec_pretty(&value).unwrap_or_default()))
            .and_then(|()| std::fs::rename(&temp, &target));
        if let Err(error) = written {
            log::warn!("saving {}: {error}", target.display());
        }
    }
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
            split: Some(212.5),
            live_split: Some(140.0),
            live_folded: true,
            projects_folded: false,
            explorer_collapsed: true,
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
                        ThreadState { agent: "dex".into(), session: "s1".into(), title: "fix the retry".into(), active_at: 1_700_000_000_000 },
                        ThreadState { agent: "opencode".into(), session: "s2".into(), title: "quote \"this\"".into(), active_at: 0 },
                    ],
                },
                ProjectState {
                    root: "/data/worktrees/po-1".into(),
                    review: true,
                    worktree: Some(Spec { path: "/data/worktrees/po-1".into(), branch: "po/1".into(), base: "abc".into(), repo: "/work/po".into() }),
                    active: 0,
                    threads: vec![],
                },
            ],
        };
        let back = Workspace::from_json(&saved.to_json()).expect("parses");
        assert!(!back.viewer_open);
        assert!(back.swapped);
        assert!(!back.agent_open);
        assert_eq!(back.agent.as_deref(), Some("dex"));
        assert_eq!(back.split, Some(212.5));
        assert_eq!(back.live_split, Some(140.0));
        assert!(back.explorer_collapsed);
        assert_eq!(back.explorer_width, Some(240.0));
        assert!(back.live_folded && !back.projects_folded);
        assert_eq!((back.left_width, back.right_width), (Some(300.0), Some(640.0)));
        assert_eq!(back.projects.len(), 2);
        assert_eq!(back.projects[0].active, 1);
        assert_eq!(back.projects[0].threads[1].title, "quote \"this\"");
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
    fn nonsense_widths_are_dropped() {
        for bad in [json!(-5), json!(0), json!(1), json!(1e9), json!("wide"), Value::Null] {
            let back = Workspace::from_json(&json!({ "projects": [], "left_width": bad, "right_width": bad })).expect("parses");
            assert_eq!((back.left_width, back.right_width), (None, None));
        }
    }

    #[test]
    fn a_nonsense_split_is_dropped() {
        for bad in [json!(-5), json!("tall"), json!(0), Value::Null] {
            let back = Workspace::from_json(&json!({ "projects": [], "split": bad })).expect("parses");
            assert_eq!(back.split, None);
        }
    }

    #[test]
    fn damaged_entries_are_skipped() {
        let value = json!({ "projects": [ { "review": true }, { "root": "/x", "threads": [ { "agent": "dex" }, { "agent": "a", "session": "s" } ] } ] });
        let back = Workspace::from_json(&value).expect("parses");
        assert_eq!(back.projects.len(), 1);
        assert_eq!(back.projects[0].threads.len(), 1);
    }

    #[test]
    fn projects_of_other_windows_are_kept_when_saving() {
        let dir = std::env::temp_dir().join(format!("po-ws-{}", std::process::id()));
        // SAFETY: no other test reads this variable.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        let project = |root: &str| ProjectState { root: root.into(), review: false, worktree: None, active: 0, threads: vec![] };
        let window = |roots: &[&str]| Workspace { projects: roots.iter().map(|r| project(r)).collect(), ..Default::default() };
        let (mut a, mut b) = (HashSet::new(), HashSet::new());
        window(&["/x", "/y"]).save(&mut a);
        window(&["/z"]).save(&mut b);
        let roots = |ws: Workspace| ws.projects.iter().map(|p| p.root.to_string_lossy().to_string()).collect::<Vec<_>>();
        assert_eq!(roots(Workspace::load(Path::new("/")).expect("saved")), ["/z", "/x", "/y"]);
        // A closes /y; B's /z and A's remaining /x stay.
        window(&["/x"]).save(&mut a);
        assert_eq!(roots(Workspace::load(Path::new("/")).expect("saved")), ["/x", "/z"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn windows_for_different_directories_have_different_files() {
        assert_ne!(legacy_file(Path::new("/a/po")), legacy_file(Path::new("/b/po")));
        assert_eq!(legacy_file(Path::new("/a/po")), legacy_file(Path::new("/a/po")));
    }
}
