//! A file tree built from a flat list of relative paths, for the explorer and the changes list.
use std::collections::{BTreeMap, HashSet};

use gpui::SharedString;

use super::tree::TreeItem;

/// Which folders are open: those the user flipped from `open_by_default`.
pub struct Folders {
    flipped: HashSet<SharedString>,
    open_by_default: bool,
}

impl Folders {
    pub fn new(open_by_default: bool) -> Self {
        Self { flipped: HashSet::new(), open_by_default }
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.flipped.contains(id) != self.open_by_default
    }

    pub fn set(&mut self, id: &SharedString, open: bool) {
        if open == self.open_by_default {
            self.flipped.remove(id);
        } else {
            self.flipped.insert(id.clone());
        }
    }
}

/// A folder in the explorer, built from the flat file list.
#[derive(Default)]
pub(crate) struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<String>,
}

/// A colour family per file kind, drawn as a small square rather than a glyph so it
/// looks the same whatever fonts are installed.
#[derive(Clone, Copy)]
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

/// The tree of `files`: folders before files at every level, folders open as `open` says.
pub fn file_items(files: &[String], open: &Folders) -> Vec<TreeItem> {
    let mut root = Dir::default();
    for file in files {
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
    fn items(
        dir: &Dir,
        prefix: &str,
        open: &Folders,
    ) -> Vec<TreeItem> {
        let folders = dir.dirs.iter().map(|(name, child)| {
            let id = format!("{prefix}{name}");
            let shown = open.is_open(&id);
            let children = items(child, &format!("{id}/"), open);
            TreeItem::new(id, name.clone()).children(children).expanded(shown)
        });
        let files = dir.files.iter().map(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            TreeItem::new(path.clone(), name.to_string())
        });
        folders.chain(files).collect()
    }
    items(&root, "", open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_open_as_flipped_from_the_default() {
        let id = SharedString::from("src");
        let mut closed = Folders::new(false);
        assert!(!closed.is_open("src"));
        closed.set(&id, true);
        assert!(closed.is_open("src"));
        closed.set(&id, false);
        assert!(!closed.is_open("src"));

        let mut open = Folders::new(true);
        assert!(open.is_open("src"));
        open.set(&id, false);
        assert!(!open.is_open("src"));
        open.set(&id, true);
        assert!(open.is_open("src"));
    }

    #[test]
    fn folders_come_before_files() {
        let files = ["b.rs".to_string(), "a/x.rs".to_string()];
        let items = file_items(&files, &Folders::new(true));
        let labels: Vec<_> = items.iter().map(|item| item.label.to_string()).collect();
        assert_eq!(labels, ["a", "b.rs"]);
    }
}
