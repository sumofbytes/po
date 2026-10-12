//! The projects and threads of a window, kept so that the next `po` started in the same
//! directory picks up where this one left off. A window is identified by the directory it was
//! started in, so windows for different directories don't overwrite each other.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::worktree::Spec;

/// A thread that had an agent session, which is what it is resumed from.
pub struct ThreadState {
    pub agent: String,
    pub session: String,
    pub title: String,
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
    /// The window was showing the agents view rather than files.
    pub agents: bool,
    /// The agent last chosen for a new thread.
    pub agent: Option<String>,
    /// The height, in pixels, the agents view gives its projects above the explorer, if dragged.
    pub split: Option<f32>,
}

/// The file for the window started in `start`.
fn file(start: &Path) -> PathBuf {
    // FNV-1a: unlike the standard hasher it is the same in every build.
    let hash = start.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    let name = start.file_name().map_or_else(|| "root".to_string(), |n| n.to_string_lossy().to_string());
    crate::config::data_dir().join("workspaces").join(format!("{name}-{hash:016x}.json"))
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
                        "agent": t.agent, "session": t.session, "title": t.title,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "version": 1, "layout": if self.agents { "agents" } else { "files" }, "agent": self.agent, "split": self.split, "projects": projects })
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
                        Some(ThreadState { agent: text(&t["agent"])?, session: text(&t["session"])?, title: text(&t["title"]).unwrap_or_default() })
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
        Some(Self { projects, agents: value["layout"].as_str() == Some("agents"), agent: text(&value["agent"]),
            // A saved height that isn't a sensible number is dropped rather than trusted.
            split: value["split"].as_f64().map(|height| height as f32).filter(|height| height.is_finite() && *height > 0.),
        })
    }

    /// What the window started in `start` saved last, if anything.
    pub fn load(start: &Path) -> Option<Self> {
        Self::from_json(&crate::config::read_json(&file(start))?)
    }

    /// Saves it as the window started in `start`'s, replacing the file in one step so that a
    /// crash can't leave half of one.
    pub fn save(&self, start: &Path) {
        let target = file(start);
        let Some(dir) = target.parent() else { return };
        let temp = target.with_extension("json.tmp");
        let written = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&temp, serde_json::to_vec_pretty(&self.to_json()).unwrap_or_default()))
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
            agents: true,
            agent: Some("dex".into()),
            split: Some(212.5),
            projects: vec![
                ProjectState {
                    root: "/work/po".into(),
                    review: false,
                    worktree: None,
                    active: 1,
                    threads: vec![
                        ThreadState { agent: "dex".into(), session: "s1".into(), title: "fix the retry".into() },
                        ThreadState { agent: "opencode".into(), session: "s2".into(), title: "quote \"this\"".into() },
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
        assert!(back.agents);
        assert_eq!(back.agent.as_deref(), Some("dex"));
        assert_eq!(back.split, Some(212.5));
        assert_eq!(back.projects.len(), 2);
        assert_eq!(back.projects[0].active, 1);
        assert_eq!(back.projects[0].threads[1].title, "quote \"this\"");
        let wt = back.projects[1].worktree.as_ref().expect("worktree");
        assert_eq!((wt.branch.as_str(), wt.base.as_str()), ("po/1", "abc"));
        assert!(back.projects[1].review);
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
    fn windows_for_different_directories_have_different_files() {
        assert_ne!(file(Path::new("/a/po")), file(Path::new("/b/po")));
        assert_eq!(file(Path::new("/a/po")), file(Path::new("/a/po")));
    }
}
