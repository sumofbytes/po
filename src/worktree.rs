//! A git worktree for one agent thread, so threads working on the same repository don't edit
//! the same files. It is a branch of its own, `po/<id>`, checked out under po's data directory.
//!
//! Nothing here loses work behind the user's back: closing a worktree commits what is left in
//! it and keeps the branch if it has anything on it. Only [`Worktree::discard`] deletes work.
//! Dropping a `Worktree` does nothing, so one that is open when po quits is still there to be
//! picked up again by the next run.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where a worktree is, for git operations that run off the UI thread.
#[derive(Clone, Debug)]
pub struct Spec {
    pub path: PathBuf,
    pub branch: String,
    /// The commit the branch started from; what the worktree's changes are measured against.
    pub base: String,
    /// The checkout the worktree was made from, which a merge lands in.
    pub repo: PathBuf,
}

pub struct Worktree {
    pub spec: Spec,
    /// The worktree was merged, discarded or retired.
    done: bool,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| format!("git: {err}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let why = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(format!("git {}: {}", args.first().copied().unwrap_or_default(), why.trim()))
    }
}

/// The top of the repository `dir` is in, if it is in one.
pub fn repo_of(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).ok().map(PathBuf::from)
}

/// Where a checkout's HEAD is: a branch, or a commit when detached, and whether it has
/// uncommitted changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub name: String,
    pub dirty: bool,
}

/// The branch `dir` has checked out, or the short commit when detached. None outside a repository.
pub fn head_of(dir: &Path) -> Option<Head> {
    let name = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    let name = if name == "HEAD" { git(dir, &["rev-parse", "--short", "HEAD"]).ok()? } else { name };
    let dirty = git(dir, &["status", "--porcelain"]).is_ok_and(|status| !status.is_empty());
    Some(Head { name, dirty })
}

/// Whether `dir` is inside a git repository. Looks for `.git` rather than asking git, so it is
/// cheap enough to ask while drawing.
pub fn in_repo(dir: &Path) -> bool {
    dir.ancestors().any(|dir| dir.join(".git").exists())
}

/// Stages and commits everything in the worktree. False if there was nothing to commit.
fn commit_all(spec: &Spec, message: &str) -> Result<bool, String> {
    if git(&spec.path, &["status", "--porcelain"])?.is_empty() {
        return Ok(false);
    }
    git(&spec.path, &["add", "-A"])?;
    // Not the user's own commit, so their hooks and signing prompts don't apply.
    git(&spec.path, &["-c", "commit.gpgsign=false", "commit", "--no-verify", "-q", "-m", message])?;
    Ok(true)
}

/// How many commits the branch has on top of where it started. An error is not "none": callers
/// must not treat a branch they couldn't inspect as empty.
fn ahead(spec: &Spec) -> Result<usize, String> {
    git(&spec.repo, &["rev-list", "--count", &format!("{}..{}", spec.base, spec.branch)])?
        .parse()
        .map_err(|_| "git rev-list: unexpected output".to_string())
}

/// Takes the worktree's directory away, and its branch too when `delete_branch`.
fn remove(spec: &Spec, delete_branch: bool) {
    let dir = spec.path.to_string_lossy();
    git(&spec.repo, &["worktree", "remove", "--force", &dir]).ok();
    std::fs::remove_dir_all(&spec.path).ok();
    git(&spec.repo, &["worktree", "prune"]).ok();
    if delete_branch {
        git(&spec.repo, &["branch", "-D", &spec.branch]).ok();
    }
}

impl Worktree {
    /// A new worktree on a new branch off the `HEAD` of the repository `from` is in. Work in
    /// `from` that isn't committed isn't in it.
    pub fn create(from: &Path) -> Result<Self, String> {
        Self::create_in(from, &crate::config::data_dir().join("worktrees"))
    }

    /// [`Self::create`], with the worktree under `parent`.
    fn create_in(from: &Path, parent: &Path) -> Result<Self, String> {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let repo = repo_of(from).ok_or("not in a git repository")?;
        let base = git(&repo, &["rev-parse", "HEAD"]).map_err(|_| "the repository has no commits yet".to_string())?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let id = format!("{stamp:x}{:x}", COUNT.fetch_add(1, Ordering::Relaxed));
        let branch = format!("po/{id}");
        let name = repo.file_name().map_or_else(|| "repo".to_string(), |n| n.to_string_lossy().to_string());
        let path = parent.join(format!("{name}-{id}"));
        std::fs::create_dir_all(path.parent().expect("under the data directory")).map_err(|err| err.to_string())?;
        git(&repo, &["worktree", "add", "-b", &branch, &path.to_string_lossy(), &base])?;
        Ok(Self { spec: Spec { path, branch, base, repo }, done: false })
    }

    /// Takes over a worktree made earlier (by a previous run), if it is still there.
    /// The spec comes from a file on disk, so only a checkout under po's own directory on a
    /// `po/` branch is taken: closing one deletes its directory.
    pub fn adopt(spec: Spec) -> Option<Self> {
        let ours = spec.path.starts_with(crate::config::data_dir().join("worktrees"))
            && !spec.path.components().any(|c| c == std::path::Component::ParentDir)
            && spec.branch.starts_with("po/");
        (ours && spec.path.join(".git").exists()).then_some(Self { spec, done: false })
    }

    /// Commits what is in the worktree and merges its branch into the repository's checkout,
    /// then removes the worktree and the branch. Returns what the merge landed in. On
    /// conflicts the merge is undone and everything is left as it was.
    pub fn merge(spec: &Spec, message: &str) -> Result<String, String> {
        commit_all(spec, message)?;
        let into = git(&spec.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "HEAD".into());
        if ahead(spec)? > 0
            && let Err(error) = git(&spec.repo, &["merge", "--no-edit", &spec.branch])
        {
            git(&spec.repo, &["merge", "--abort"]).ok();
            return Err(format!("could not merge into {into}; the branch {} is kept\n{error}", spec.branch));
        }
        remove(spec, true);
        Ok(into)
    }

    /// Throws the worktree and its branch away, with everything done in them.
    pub fn discard(spec: &Spec) {
        remove(spec, true);
    }

    /// Records that [`Self::merge`] or [`Self::discard`] has taken the worktree away.
    pub fn finished(&mut self) {
        self.done = true;
    }

    /// Closes the worktree without losing work: what is left in it is committed, and the
    /// branch is kept if it has any commits. Returns the branch when it was kept.
    pub fn retire(&mut self) -> Option<String> {
        if self.done {
            return None;
        }
        self.done = true;
        if let Err(error) = commit_all(&self.spec, "po: work in progress") {
            log::warn!("worktree {}: {error}", self.spec.branch);
            // Removing it would lose what could not be saved.
            return Some(self.spec.branch.clone());
        }
        let keep = match ahead(&self.spec) {
            Ok(n) => n > 0,
            Err(error) => {
                // Can't tell whether it holds work, so it is kept.
                log::warn!("worktree {}: {error}", self.spec.branch);
                true
            }
        };
        remove(&self.spec, !keep);
        keep.then(|| self.spec.branch.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("{e}"))
    }

    /// A repository with one commit, in a directory of its own.
    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("po-wt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "user.email", "t@example.com"]);
        sh(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "first"]);
        dir.canonicalize().unwrap()
    }

    fn create(repo: &Path) -> Result<Worktree, String> {
        Worktree::create_in(repo, &std::env::temp_dir().join(format!("po-wt-data-{}", std::process::id())))
    }

    fn branches(repo: &Path) -> String {
        sh(repo, &["branch", "--list", "po/*"])
    }

    #[test]
    fn merging_lands_the_work_and_removes_the_worktree() {
        let repo = repo("merge");
        let wt = create(&repo).unwrap();
        assert!(wt.spec.path.is_dir());
        std::fs::write(wt.spec.path.join("b.txt"), "new\n").unwrap();
        let spec = wt.spec.clone();
        let into = Worktree::merge(&spec, "add b").unwrap();
        assert_eq!(into, "main");
        assert_eq!(std::fs::read_to_string(repo.join("b.txt")).unwrap(), "new\n");
        assert!(!spec.path.exists());
        assert!(branches(&repo).is_empty());
        let mut wt = wt;
        wt.finished();
    }

    #[test]
    fn a_conflicting_merge_changes_nothing() {
        let repo = repo("conflict");
        let wt = create(&repo).unwrap();
        std::fs::write(wt.spec.path.join("a.txt"), "from the worktree\n").unwrap();
        std::fs::write(repo.join("a.txt"), "from the checkout\n").unwrap();
        sh(&repo, &["commit", "-q", "-am", "diverge"]);
        let error = Worktree::merge(&wt.spec.clone(), "edit a").unwrap_err();
        assert!(error.contains("could not merge"), "{error}");
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "from the checkout\n");
        assert!(sh(&repo, &["status", "--porcelain"]).is_empty(), "the merge was left half done");
        assert!(wt.spec.path.is_dir());
        assert!(branches(&repo).contains(&wt.spec.branch));
    }

    #[test]
    fn closing_keeps_work_and_drops_an_empty_branch() {
        let repo = repo("retire");
        let mut untouched = create(&repo).unwrap();
        assert_eq!(untouched.retire(), None);
        assert!(branches(&repo).is_empty());

        let mut worked = create(&repo).unwrap();
        std::fs::write(worked.spec.path.join("c.txt"), "kept\n").unwrap();
        let branch = worked.retire().expect("a branch with work is kept");
        assert!(!worked.spec.path.exists());
        assert_eq!(sh(&repo, &["show", &format!("{branch}:c.txt")]), "kept");
    }

    #[test]
    fn discarding_deletes_the_branch() {
        let repo = repo("discard");
        let mut wt = create(&repo).unwrap();
        std::fs::write(wt.spec.path.join("d.txt"), "gone\n").unwrap();
        Worktree::discard(&wt.spec);
        wt.finished();
        assert!(!wt.spec.path.exists());
        assert!(branches(&repo).is_empty());
    }

    #[test]
    fn a_folder_outside_git_is_refused() {
        let dir = std::env::temp_dir().join(format!("po-wt-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(create(&dir).is_err());
    }

    #[test]
    fn closing_keeps_the_branch_when_it_cannot_be_inspected() {
        let repo = repo("blind");
        let mut wt = create(&repo).unwrap();
        std::fs::write(wt.spec.path.join("e.txt"), "precious\n").unwrap();
        wt.spec.base = "not-a-commit".into();
        assert!(ahead(&wt.spec).is_err());
        let branch = wt.spec.branch.clone();
        assert_eq!(wt.retire(), Some(branch.clone()));
        assert!(branches(&repo).contains(&branch));
    }

    #[test]
    fn only_po_s_own_worktrees_are_adopted() {
        let dir = std::env::temp_dir().join(format!("po-wt-adopt-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let spec = Spec { path: dir.clone(), branch: "po/x".into(), base: "b".into(), repo: dir };
        assert!(Worktree::adopt(spec).is_none());
    }
}
