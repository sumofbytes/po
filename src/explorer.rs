//! The file tree beside the viewer, built from a flat list of relative paths: every file in the
//! explorer, the changed ones in the changes list. It is only data (which folders are open, what
//! is selected); the screen draws the [`Row`]s it works out.
use std::collections::{BTreeMap, HashSet};

/// A colour family per file kind, drawn as a small square rather than a glyph so it
/// looks the same whatever fonts are installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    Code,
    Data,
    Doc,
    Web,
    Image,
    Other,
}

pub fn file_kind(name: &str) -> FileKind {
    let Some((stem, ext)) = name.rsplit_once('.') else { return FileKind::Other };
    if stem.is_empty() {
        return FileKind::Other;
    }
    match ext.to_ascii_lowercase().as_str() {
        "rs" | "go" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "py" | "rb" | "java" | "kt" | "c" | "h" | "cc"
        | "cpp" | "hpp" | "cs" | "php" | "swift" | "lua" | "ex" | "exs" | "scala" | "dart" | "sh" | "bash" | "zsh"
        | "sql" => FileKind::Code,
        "json" | "yaml" | "yml" | "toml" | "ini" | "xml" | "csv" | "env" | "lock" | "mod" | "sum" => FileKind::Data,
        "md" | "markdown" | "txt" | "rst" | "adoc" => FileKind::Doc,
        "html" | "htm" | "css" | "scss" | "less" | "svg" | "vue" => FileKind::Web,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "avif" => FileKind::Image,
        _ => FileKind::Other,
    }
}

/// A sort key that puts `path` where the tree lists it: folders before files at every level.
pub fn order(path: &str) -> Vec<(bool, String)> {
    let last = path.matches('/').count();
    path.split('/').enumerate().map(|(ix, part)| (ix == last, part.to_string())).collect()
}

/// A visible row of the tree.
#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    /// The path: of the file, or of the folder from the root.
    pub id: String,
    pub name: String,
    pub depth: usize,
    pub folder: bool,
    pub expanded: bool,
    pub selected: bool,
}

/// What the arrow keys do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Up,
    Down,
    /// Closes an open folder.
    Left,
    /// Opens a closed folder.
    Right,
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<String>,
}

/// A tree of files with its open folders: the explorer's, or the changes list's.
pub struct FileTree {
    paths: Vec<String>,
    /// The folders the user flipped from `open_by_default`.
    flipped: HashSet<String>,
    open_by_default: bool,
    selected: Option<String>,
}

impl FileTree {
    pub fn new(open_by_default: bool) -> Self {
        Self { paths: vec![], flipped: HashSet::new(), open_by_default, selected: None }
    }

    fn is_open(&self, id: &str) -> bool {
        self.flipped.contains(id) != self.open_by_default
    }

    fn set_open(&mut self, id: &str, open: bool) {
        if open == self.open_by_default {
            self.flipped.remove(id);
        } else {
            self.flipped.insert(id.to_string());
        }
    }

    /// Shows `paths`, with `selected` selected if it is among them and nothing otherwise.
    #[hotpath::measure(impl_type = "FileTree")]
    pub fn set_paths(&mut self, paths: &[String], selected: Option<&str>) {
        self.paths = paths.to_vec();
        self.selected = selected.filter(|id| self.paths.iter().any(|p| p == id)).map(str::to_string);
    }

    /// Selects `id`, if it is shown.
    pub fn select(&mut self, id: &str) {
        if self.rows().iter().any(|row| row.id == id) {
            self.selected = Some(id.to_string());
        }
    }

    /// The selected file, not a folder.
    pub fn selected_file(&self) -> Option<&str> {
        self.selected.as_deref().filter(|id| self.paths.iter().any(|p| p == id))
    }

    /// Selects the row `id`, and flips it if it is a folder.
    pub fn click(&mut self, id: &str) {
        let Some(row) = self.rows().into_iter().find(|row| row.id == id) else { return };
        self.selected = Some(row.id.clone());
        if row.folder {
            self.set_open(&row.id, !row.expanded);
        }
    }

    /// Moves the selection, or opens or closes the selected folder.
    pub fn step(&mut self, step: Step) {
        let rows = self.rows();
        let at = self.selected.as_ref().and_then(|id| rows.iter().position(|row| &row.id == id));
        match step {
            Step::Up => {
                let ix = at.unwrap_or(0).checked_sub(1).unwrap_or(rows.len().saturating_sub(1));
                self.selected = rows.get(ix).map(|row| row.id.clone());
            }
            Step::Down => {
                let ix = at.map_or(0, |ix| ix + 1);
                let ix = if ix < rows.len() { ix } else { 0 };
                self.selected = rows.get(ix).map(|row| row.id.clone());
            }
            Step::Left | Step::Right => {
                let Some(row) = at.map(|ix| &rows[ix]).filter(|row| row.folder && row.expanded == (step == Step::Left)) else { return };
                self.set_open(&row.id, !row.expanded);
            }
        }
    }

    /// The visible rows: folders before files at every level, folders open as the user left them.
    #[hotpath::measure(impl_type = "FileTree")]
    pub fn rows(&self) -> Vec<Row> {
        let mut root = Dir::default();
        for file in &self.paths {
            let mut dir = &mut root;
            let mut parts = file.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_some() {
                    dir = dir.dirs.entry(part.to_string()).or_default();
                } else {
                    dir.files.push(file.clone());
                }
            }
        }
        let mut out = Vec::new();
        self.walk(&root, "", 0, &mut out);
        out
    }

    fn walk(&self, dir: &Dir, prefix: &str, depth: usize, out: &mut Vec<Row>) {
        for (name, child) in &dir.dirs {
            let id = format!("{prefix}{name}");
            let expanded = self.is_open(&id);
            let selected = self.selected.as_deref() == Some(&id);
            out.push(Row { id: id.clone(), name: name.clone(), depth, folder: true, expanded, selected });
            if expanded {
                self.walk(child, &format!("{id}/"), depth + 1, out);
            }
        }
        for path in &dir.files {
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            let selected = self.selected.as_deref() == Some(path);
            out.push(Row { id: path.clone(), name, depth, folder: false, expanded: false, selected });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(tree: &FileTree) -> Vec<String> {
        tree.rows().into_iter().map(|row| row.id).collect()
    }

    fn files() -> Vec<String> {
        ["b.rs", "a/x.rs", "a/c/y.rs"].map(String::from).to_vec()
    }

    #[test]
    fn folders_come_before_files_and_open_as_flipped_from_the_default() {
        let mut open = FileTree::new(true);
        open.set_paths(&files(), None);
        assert_eq!(ids(&open), ["a", "a/c", "a/c/y.rs", "a/x.rs", "b.rs"]);
        open.click("a");
        assert_eq!(ids(&open), ["a", "b.rs"]);

        let mut closed = FileTree::new(false);
        closed.set_paths(&files(), None);
        assert_eq!(ids(&closed), ["a", "b.rs"]);
        closed.click("a");
        assert_eq!(ids(&closed), ["a", "a/c", "a/x.rs", "b.rs"]);
    }

    #[test]
    fn order_puts_folders_before_files() {
        assert!(order("a/z.rs") < order("b.rs"));
        assert!(order("a/b/c.rs") < order("a/a.rs"));
    }

    #[test]
    fn the_keys_move_the_selection_and_fold_folders() {
        let mut tree = FileTree::new(false);
        tree.set_paths(&files(), None);
        tree.step(Step::Down);
        assert_eq!(tree.selected.as_deref(), Some("a"));
        tree.step(Step::Right);
        assert_eq!(ids(&tree), ["a", "a/c", "a/x.rs", "b.rs"]);
        tree.step(Step::Left);
        assert_eq!(ids(&tree), ["a", "b.rs"]);
        tree.step(Step::Up);
        assert_eq!(tree.selected.as_deref(), Some("b.rs"));
        assert_eq!(tree.selected_file(), Some("b.rs"));
        tree.step(Step::Down);
        assert_eq!(tree.selected.as_deref(), Some("a"));
        assert_eq!(tree.selected_file(), None);
    }

    #[test]
    fn a_new_list_keeps_only_a_selection_it_still_has() {
        let mut tree = FileTree::new(true);
        tree.set_paths(&files(), Some("b.rs"));
        assert_eq!(tree.selected_file(), Some("b.rs"));
        tree.set_paths(&["c.rs".to_string()], Some("b.rs"));
        assert_eq!(tree.selected_file(), None);
    }
}
