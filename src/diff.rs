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

/// One line of a unified diff, with its line numbers on each side.
pub struct Row {
    pub kind: Kind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    // `git diff --no-index` exits 1 when the files differ.
    (out.status.success() || out.status.code() == Some(1))
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Files changed against `HEAD`, including untracked ones. Empty outside a git repository.
pub fn changes(root: &Path) -> Vec<Change> {
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
    // Folders before files at every level, as the explorer lists them.
    out.sort_by_cached_key(|change| {
        let parts: Vec<&str> = change.path.split('/').collect();
        let last = parts.len() - 1;
        parts.iter().enumerate().map(|(ix, part)| (ix == last, part.to_string())).collect::<Vec<_>>()
    });
    out
}

/// The diff of `change` against `HEAD`, as rows.
pub fn file_diff(root: &Path, change: &Change) -> Vec<Row> {
    let text = if change.status == 'U' {
        git(root, &["diff", "--no-index", "--no-color", "--", "/dev/null", &change.path])
    } else {
        git(root, &["diff", "HEAD", "--no-color", "-U4", "--", &change.path])
    };
    text.map(|text| parse(&text)).unwrap_or_default()
}

fn parse(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let (mut old, mut new) = (0, 0);
    let mut inside = false;
    for line in text.lines() {
        if rows.len() >= MAX_ROWS {
            break;
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            let mut numbers = header.split(' ').take(2).map(|side| {
                side.trim_start_matches(['-', '+']).split(',').next().and_then(|n| n.parse::<u32>().ok())
            });
            (old, new) = (numbers.next().flatten().unwrap_or(1), numbers.next().flatten().unwrap_or(1));
            inside = true;
            rows.push(Row { kind: Kind::Hunk, old: None, new: None, text: line.to_string() });
            continue;
        }
        if !inside {
            continue;
        }
        let (kind, text) = match line.chars().next() {
            Some('+') => (Kind::Added, &line[1..]),
            Some('-') => (Kind::Removed, &line[1..]),
            Some(' ') => (Kind::Context, &line[1..]),
            _ => continue, // "\ No newline at end of file"
        };
        let row = match kind {
            Kind::Added => Row { kind, old: None, new: Some(new), text: text.into() },
            Kind::Removed => Row { kind, old: Some(old), new: None, text: text.into() },
            _ => Row { kind, old: Some(old), new: Some(new), text: text.into() },
        };
        if kind != Kind::Added {
            old += 1;
        }
        if kind != Kind::Removed {
            new += 1;
        }
        rows.push(row);
    }
    rows
}
