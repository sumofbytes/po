use crate::files::watch::*;
use crate::*;

/// Files matching `query`: name hits first (shorter names first), then path hits, each in
/// walk order; ASCII-case-insensitive substring, like the viewer's search.
/// shortcut: substring only — no fuzzy/typo-tolerant ranking; add it if finding a file by a
/// scrambled query (typing `cvs` for `code_view_state.rs`) matters.
pub(crate) fn match_files(files: &[String], query: &str) -> Vec<String> {
    if query.is_empty() {
        return files.to_vec();
    }
    let query = query.as_bytes();
    // Windows compared with eq_ignore_ascii_case, so no lowercase copy of each path is made.
    let contains = |text: &str| {
        text.as_bytes()
            .windows(query.len())
            .any(|window| window.eq_ignore_ascii_case(query))
    };
    let mut hits: Vec<(u8, usize, &String)> = files
        .iter()
        .filter_map(|file| {
            let name_start = file.rfind('/').map_or(0, |slash| slash + 1);
            let rank = if contains(&file[name_start..]) {
                0
            } else if contains(file) {
                1
            } else {
                return None;
            };
            Some((rank, file.len() - name_start, file))
        })
        .collect();
    hits.sort_by_key(|&(rank, name_len, _)| (rank, name_len));
    hits.into_iter().map(|(_, _, file)| file.clone()).collect()
}

/// What an empty viewer says to do: pick from the tree, or how to get it back.
pub(crate) fn pick_hint(what: &str, tree_visible: bool) -> String {
    if tree_visible {
        format!("Pick {what} in the tree")
    } else {
        format!("Pick {what} with Ctrl+P, or go back to the explorer")
    }
}

/// What the sidebar lists and the viewer shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Source,
    Diff,
}

pub(crate) fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
    })
}

/// Everything about one checkout: its files and git state, and what the explorer, viewer and
/// diff viewer are showing of them.
pub(crate) struct Project {
    pub(crate) root: PathBuf,
    /// What Changes mode diffs against: `HEAD`, or a PR's merge-base when reviewing.
    pub(crate) base: String,
    pub(crate) pr: Option<crate::vcs::pr::Pr>,
    pub(crate) files: Vec<String>,
    pub(crate) changes: Vec<Change>,
    /// Change marks for the explorer, by path.
    pub(crate) marks: Arc<HashMap<String, char>>,
    /// The latest `git diff` for the picked file and `git status`, run off the UI thread;
    /// replacing one drops the stale run.
    pub(crate) diffing: Option<Task<()>>,
    pub(crate) listing: Option<Task<()>>,
    pub(crate) scanning: Option<Task<()>>,
    pub(crate) watching: Option<Task<()>>,
    /// The changed files, folders open until the user closes them.
    pub(crate) changes_tree: FileTree,
    pub(crate) picked: Option<String>,
    /// The picked file's diff as rows, which say whether it has textual changes and how many lines
    /// it adds and removes.
    pub(crate) rows: Arc<Vec<Row>>,
    pub(crate) counts: (usize, usize),
    /// The picked file's old and new versions, which the diff view compares.
    pub(crate) versions: Option<(model::source::Shared, model::source::Shared)>,
    pub(crate) current: Option<String>,
    pub(crate) stamp: Option<Stamp>,
    pub(crate) code: Option<model::source::Shared>,
    /// What the current file is: the markdown flag of it, loaded elsewhere.
    pub(crate) is_markdown: bool,
    pub(crate) notice: Option<String>,
    /// Every file, folders closed until the user opens them.
    pub(crate) files_tree: FileTree,
    /// Set when this is a thread's own checkout, made from another project's repository.
    pub(crate) worktree: Option<crate::vcs::worktree::Worktree>,
    /// The project's shell, started the first time the terminal is opened, and whether the viewer
    /// shows it.
    pub(crate) terminal: Option<rune_ui::terminal::Term>,
    pub(crate) terminal_shown: bool,
    /// Threads closed here, newest first, offered back by the session picker.
    pub(crate) closed: Vec<crate::session::workspace::ThreadState>,
}

impl Project {
    /// Notes a closed thread, newest first. A session is there once, and only so many are kept.
    pub(crate) fn remember(&mut self, thread: crate::session::workspace::ThreadState) {
        self.closed.retain(|known| known.session != thread.session);
        self.closed.insert(0, thread);
        self.closed.truncate(crate::session::workspace::CLOSED_KEPT);
    }

    /// Whether the list shows the branch this project has checked out. A PR is a checkout of
    /// its own and a thread's worktree already names its branch.
    pub(crate) fn shows_branch(&self) -> bool {
        self.pr.is_none() && self.worktree.is_none()
    }

    /// What the project is called in the list.
    pub(crate) fn name(&self) -> String {
        match &self.pr {
            Some(pr) => format!("PR #{} {}", pr.number, pr.title),
            None => {
                let name = self.root.file_name().map_or_else(
                    || self.root.display().to_string(),
                    |n| n.to_string_lossy().to_string(),
                );
                match &self.worktree {
                    Some(wt) => format!(
                        "{} ⎇ {}",
                        wt.spec
                            .repo
                            .file_name()
                            .map_or(name, |n| n.to_string_lossy().to_string()),
                        wt.spec.branch
                    ),
                    None => name,
                }
            }
        }
    }
}
