use std::path::Path;
use std::process::Command;

const MAX_ROWS: usize = 5000;

/// A file that differs from `HEAD`: `M`odified, `A`dded, `D`eleted, `R`enamed or `U`ntracked.
#[derive(Clone, PartialEq)]
pub struct Change {
    pub path: String,
    pub status: char,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Hunk,
    Context,
    Added,
    Removed,
}

/// One line of a unified diff.
pub struct Row {
    pub kind: Kind,
    pub text: String,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    // `git diff --no-index` exits 1 when the files differ.
    (out.status.success() || out.status.code() == Some(1))
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Files changed against `base`, including untracked ones. Empty outside a git repository.
/// `base` is `HEAD` for local work, or a PR's merge-base when reviewing.
pub fn changes(root: &Path, base: &str) -> Vec<Change> {
    let mut out = if base == "HEAD" { status(root) } else { against(root, base) };
    out.sort_by_cached_key(|change| crate::ui::file_tree::order(&change.path));
    out
}

/// Files that differ from an arbitrary commit, plus untracked ones. Renames show as a delete and an add.
fn against(root: &Path, base: &str) -> Vec<Change> {
    let Some(text) = git(root, &["diff", "--name-status", "-z", "--no-renames", base]) else { return vec![] };
    let mut parts = text.split('\0').filter(|part| !part.is_empty());
    let mut out = Vec::new();
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        let status = status.chars().next().unwrap_or('M');
        out.push(Change { path: path.to_string(), status: if status == 'T' { 'M' } else { status } });
    }
    if let Some(text) = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]) {
        out.extend(text.split('\0').filter(|p| !p.is_empty()).map(|p| Change { path: p.to_string(), status: 'U' }));
    }
    out
}

fn status(root: &Path) -> Vec<Change> {
    let Some(text) = git(root, &["status", "--porcelain=v1", "-z", "-uall"]) else { return vec![] };
    let mut parts = text.split('\0').filter(|part| !part.is_empty());
    let mut out = Vec::new();
    while let Some(entry) = parts.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else { continue };
        let mut flags = code.chars();
        let (index, tree) = (flags.next().unwrap_or(' '), flags.next().unwrap_or(' '));
        if matches!(index, 'R' | 'C') {
            parts.next(); // the original name follows a rename
        }
        let status = match (index, tree) {
            ('?', _) => 'U',
            (_, ' ') => index,
            _ => tree,
        };
        out.push(Change { path: path.to_string(), status: if status == 'C' { 'A' } else { status } });
    }
    out
}

/// The diff of `change` against `base`, as rows.
pub fn file_diff(root: &Path, base: &str, change: &Change) -> Vec<Row> {
    let text = if change.status == 'U' {
        git(root, &["diff", "--no-index", "--no-color", "--", "/dev/null", &change.path])
    } else {
        git(root, &["diff", base, "--no-color", "-U4", "--", &change.path])
    };
    text.map(|text| parse(&text)).unwrap_or_default()
}

/// The old and the new version of `change` as text: nothing for the side that doesn't exist (an
/// added file has no old one, a deleted file no new one).
pub fn versions(root: &Path, base: &str, change: &Change) -> (Option<String>, Option<String>) {
    let old = if matches!(change.status, 'A' | 'U') {
        None
    } else {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["show", &format!("{base}:{}", change.path)])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
    };
    let new = if change.status == 'D' { None } else { std::fs::read_to_string(root.join(&change.path)).ok() };
    (old, new)
}

fn parse(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if rows.len() >= MAX_ROWS {
            break;
        }
        let inside_now = line.strip_prefix("@@ ").is_some();
        if inside_now {
            rows.push(Row { kind: Kind::Hunk, text: line.to_string() });
        }
        inside = inside || inside_now;
        if !inside || inside_now {
            continue;
        }
        let (kind, text) = match line.chars().next() {
            Some('+') => (Kind::Added, &line[1..]),
            Some('-') => (Kind::Removed, &line[1..]),
            Some(' ') => (Kind::Context, &line[1..]),
            _ => continue, // "\ No newline at end of file"
        };
        rows.push(Row { kind, text: text.into() });
    }
    rows
}
