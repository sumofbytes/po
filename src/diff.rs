use std::ops::Range;
use std::path::Path;
use std::process::Command;

use gpui::HighlightStyle;

use crate::viewer::CodeColors;

const MAX_ROWS: usize = 5000;
/// Versions of a file bigger than this are diffed without syntax colors.
const MAX_COLORED: usize = 2 * 1024 * 1024;

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

/// Files changed against `base`, including untracked ones. Empty outside a git repository.
/// `base` is `HEAD` for local work, or a PR's merge-base when reviewing.
pub fn changes(root: &Path, base: &str) -> Vec<Change> {
    let mut out = if base == "HEAD" { status(root) } else { against(root, base) };
    // Folders before files at every level, as the explorer lists them.
    out.sort_by_cached_key(|change| {
        let parts: Vec<&str> = change.path.split('/').collect();
        let last = parts.len() - 1;
        parts.iter().enumerate().map(|(ix, part)| (ix == last, part.to_string())).collect::<Vec<_>>()
    });
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

/// Syntax colors for both versions of a changed file. Each version is parsed whole, so a
/// line is colored as it reads in the file rather than as a fragment cut out of it.
#[derive(Default)]
pub struct Colors {
    old: Option<Version>,
    new: Option<Version>,
}

struct Version {
    colors: CodeColors,
    text: String,
    /// Where each line starts in `text`.
    starts: Vec<usize>,
}

impl Version {
    fn new(text: String, language: &str) -> Option<Self> {
        if text.len() > MAX_COLORED {
            return None;
        }
        let colors = CodeColors::new(&text, language)?;
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(ix, _)| ix + 1)).collect();
        Some(Self { colors, text, starts })
    }

    /// Where line `number` (from 1) is in the text, if it still reads `line`: the file may
    /// have moved on since the diff was taken.
    fn line(&self, number: u32, line: &str) -> Option<Range<usize>> {
        let start = *self.starts.get((number as usize).checked_sub(1)?)?;
        let end = self.starts.get(number as usize).map_or(self.text.len(), |next| next - 1);
        let end = if self.text[..end].ends_with('\r') { end - 1 } else { end };
        (self.text[start..end] == *line).then_some(start..end)
    }
}

/// The theme's styles for each version's captures, looked up once per frame.
pub type Palettes = [Vec<Option<HighlightStyle>>; 2];

impl Colors {
    pub fn palettes(&self, cx: &gpui::App) -> Palettes {
        let palette = |version: &Option<Version>| version.as_ref().map(|v| v.colors.palette(cx)).unwrap_or_default();
        [palette(&self.old), palette(&self.new)]
    }

    /// Syntax colors for `text`, line `number` of the old version or the new one.
    pub fn line(&self, old: bool, number: u32, text: &str, palettes: &Palettes) -> Vec<(Range<usize>, HighlightStyle)> {
        let (version, palette) = if old { (&self.old, &palettes[0]) } else { (&self.new, &palettes[1]) };
        let Some(version) = version else { return vec![] };
        version.line(number, text).map(|range| version.colors.styles_in(range, palette)).unwrap_or_default()
    }
}

/// Parses the old and new versions of `change` as `language` (a name or file extension).
pub fn colors(root: &Path, base: &str, change: &Change, language: &str) -> Colors {
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
    Colors {
        old: old.and_then(|text| Version::new(text, language)),
        new: new.and_then(|text| Version::new(text, language)),
    }
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

/// One side of a side-by-side row.
pub struct Cell {
    pub kind: Kind,
    pub number: u32,
    pub text: String,
}

/// A side-by-side row: a hunk header, or an old and a new cell. Runs of removed and added
/// lines are paired off, so a changed line sits opposite its replacement.
pub enum Pair {
    Hunk(String),
    Line(Option<Cell>, Option<Cell>),
}

pub fn split(rows: &[Row]) -> Vec<Pair> {
    let mut out = Vec::new();
    let (mut removed, mut added): (Vec<Cell>, Vec<Cell>) = (vec![], vec![]);
    let flush = |out: &mut Vec<Pair>, removed: &mut Vec<Cell>, added: &mut Vec<Cell>| {
        let (mut old, mut new) = (std::mem::take(removed).into_iter(), std::mem::take(added).into_iter());
        loop {
            let (a, b) = (old.next(), new.next());
            if a.is_none() && b.is_none() {
                break;
            }
            out.push(Pair::Line(a, b));
        }
    };
    for row in rows {
        match row.kind {
            Kind::Removed => removed.push(Cell { kind: row.kind, number: row.old.unwrap_or(0), text: row.text.clone() }),
            Kind::Added => added.push(Cell { kind: row.kind, number: row.new.unwrap_or(0), text: row.text.clone() }),
            Kind::Hunk => {
                flush(&mut out, &mut removed, &mut added);
                out.push(Pair::Hunk(row.text.clone()));
            }
            Kind::Context => {
                flush(&mut out, &mut removed, &mut added);
                let cell = |number: Option<u32>| Cell { kind: Kind::Context, number: number.unwrap_or(0), text: row.text.clone() };
                out.push(Pair::Line(Some(cell(row.old)), Some(cell(row.new))));
            }
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_colored_in_the_context_of_their_whole_version() {
        // The string opens a line earlier, so on its own the second line wouldn't read as one.
        let text = "let s = \"a\nb\";\r\nlet t = 1;\n".to_string();
        let version = Version::new(text, "rs").unwrap();
        assert_eq!(version.line(2, "b\";"), Some(11..14));
        assert_eq!(version.line(2, "changed since"), None);
        assert_eq!(version.line(9, "b\";"), None);
        let names = version.colors.palette_len();
        let colors = Colors { old: None, new: Some(version) };
        let palettes = [vec![], vec![Some(HighlightStyle::default()); names]];
        assert_eq!(colors.line(false, 2, "b\";", &palettes).first().map(|(range, _)| range.clone()), Some(0..2));
        assert!(colors.line(true, 2, "b\";", &palettes).is_empty());
    }
}
