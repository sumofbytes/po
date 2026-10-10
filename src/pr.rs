use std::path::{Path, PathBuf};
use std::process::Command;

/// A pull request checked out into a throwaway git worktree, for review.
///
/// `base` is the merge-base with the PR's target branch, so diffs against it show
/// exactly what the PR changes. Dropping the session removes the worktree.
pub struct Pr {
    pub worktree: PathBuf,
    pub base: String,
    pub number: u64,
    pub title: String,
    /// The clone the worktree was registered against; `None` when it was a standalone clone.
    source: Option<PathBuf>,
}

fn run(dir: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| format!("{program}: {err}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// `owner/repo` from a `https://github.com/owner/repo/pull/N` URL.
fn repo_of(url: &str) -> Option<String> {
    let rest = url.split("github.com/").nth(1)?;
    let mut parts = rest.split('/');
    Some(format!("{}/{}", parts.next()?, parts.next()?))
}

impl Pr {
    /// Checks out the PR named by `spec` (a number, branch or URL, as `gh pr view` accepts)
    /// from the repository at `cwd`, or from a fresh clone when `cwd` is another repository.
    pub fn open(cwd: &Path, spec: &str) -> Result<Self, String> {
        let info = run(cwd, "gh", &["pr", "view", spec, "--json", "number,title,baseRefName,url"])?;
        let info: serde_json::Value = serde_json::from_str(&info).map_err(|err| err.to_string())?;
        let number = info["number"].as_u64().ok_or("gh returned no PR number")?;
        let title = info["title"].as_str().unwrap_or_default().to_string();
        let base_ref = info["baseRefName"].as_str().ok_or("gh returned no base branch")?;
        let repo = info["url"].as_str().and_then(repo_of).ok_or("gh returned no PR url")?;

        let local = run(cwd, "git", &["rev-parse", "--show-toplevel"]).ok().filter(|top| {
            run(Path::new(top), "git", &["remote", "get-url", "origin"])
                .is_ok_and(|url| url.to_lowercase().contains(&repo.to_lowercase()))
        });
        let worktree = std::env::temp_dir().join(format!("po-pr-{}-{number}", std::process::id()));
        let head_ref = format!("refs/po/pr/{number}");
        let base_remote_ref = format!("refs/po/base/{number}");
        let dir = worktree.to_string_lossy().into_owned();

        let source = match local {
            Some(top) => {
                let top = PathBuf::from(top);
                run(&top, "git", &["fetch", "--no-tags", "origin", &format!("+refs/pull/{number}/head:{head_ref}")])?;
                run(&top, "git", &["fetch", "--no-tags", "origin", &format!("+refs/heads/{base_ref}:{base_remote_ref}")])?;
                run(&top, "git", &["worktree", "add", "--detach", &dir, &head_ref])?;
                Some(top)
            }
            None => {
                run(cwd, "gh", &["repo", "clone", &repo, &dir, "--", "--filter=blob:none", "--no-checkout"])?;
                run(&worktree, "git", &["fetch", "--no-tags", "origin", &format!("+refs/pull/{number}/head:{head_ref}")])?;
                run(&worktree, "git", &["fetch", "--no-tags", "origin", &format!("+refs/heads/{base_ref}:{base_remote_ref}")])?;
                run(&worktree, "git", &["checkout", "--detach", &head_ref])?;
                None
            }
        };
        // Built before the merge-base so an early failure below still cleans up on drop.
        let mut pr = Self { worktree: worktree.clone(), base: String::new(), number, title, source };
        pr.base = run(&worktree, "git", &["merge-base", &base_remote_ref, &head_ref])?;
        Ok(pr)
    }

    /// Takes over a worktree that another process checked out, to clean it up on drop.
    pub fn adopt(worktree: PathBuf, base: String, number: u64, title: String, source: Option<PathBuf>) -> Self {
        Self { worktree, base, number, title, source }
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

impl Drop for Pr {
    fn drop(&mut self) {
        if let Some(source) = &self.source {
            let dir = self.worktree.to_string_lossy();
            run(source, "git", &["worktree", "remove", "--force", &dir]).ok();
            for kind in ["pr", "base"] {
                run(source, "git", &["update-ref", "-d", &format!("refs/po/{kind}/{}", self.number)]).ok();
            }
        }
        std::fs::remove_dir_all(&self.worktree).ok();
    }
}
