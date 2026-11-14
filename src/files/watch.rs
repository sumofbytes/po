use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(crate) const SKIP: [&str; 5] = ["target", "node_modules", "dist", "build", "__pycache__"];

pub(crate) const MAX_FILES: usize = 5000;

pub(crate) fn ignored(name: &str) -> bool {
    name.starts_with('.') || SKIP.contains(&name)
}

/// The tree under `dir` without dotfiles, `SKIP` folders and anything the `.gitignore` files
/// exclude (they apply even outside a git repository).
pub(crate) fn walker(dir: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(dir)
        .hidden(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.depth() == 0 || !ignored(&entry.file_name().to_string_lossy()))
        .build()
}

/// Watches `dir` and its subfolders one by one, so skipped folders such as
/// `target` never take up watches.
pub(crate) fn watch_dirs(watcher: &mut impl notify::Watcher, dir: &Path) {
    for entry in walker(dir).flatten() {
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            watcher
                .watch(entry.path(), notify::RecursiveMode::NonRecursive)
                .ok();
        }
    }
}

pub(crate) type Watch = (
    notify::RecommendedWatcher,
    crate::dev::profile::UnboundedReceiver<notify::Event>,
);

/// A watcher on every folder under `root`. Adding the watches costs a syscall per folder, which
/// adds up to seconds on a big tree, so callers run this off the UI thread.
#[hotpath::measure]
pub(crate) fn start_watcher(root: &Path) -> Option<Watch> {
    let (tx, rx) = hotpath::channel!(futures::channel::mpsc::unbounded(), label = "file events");
    let mut watcher =
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // Reads are no change, and whatever scans the tree (an indexer, `git status`) causes them
            // by the hundred; each would wake the watcher for nothing.
            if let Ok(event) = event
                && !event.kind.is_access()
            {
                tx.unbounded_send(event).ok();
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                log::error!("watch: {error}");
                return None;
            }
        };
    watch_dirs(&mut watcher, root);
    Some((watcher, rx))
}

/// The explorer's file list and the watcher, started before the window opens so they run while
/// the GPU is being set up instead of after it.
pub(crate) struct Prefetch {
    pub(crate) root: PathBuf,
    pub(crate) files: std::thread::JoinHandle<Vec<String>>,
    pub(crate) watch: futures::channel::oneshot::Receiver<Option<Watch>>,
}

impl Prefetch {
    pub(crate) fn start(root: PathBuf) -> Self {
        let scan = root.clone();
        let files = std::thread::Builder::new()
            .name("po-walk".into())
            .spawn(move || {
                let mut files = Vec::new();
                walk(&scan, &mut files);
                crate::dev::trace::mark("walk (thread)");
                files
            })
            .expect("failed to spawn thread");
        let (done, watch) = futures::channel::oneshot::channel();
        let target = root.clone();
        std::thread::Builder::new()
            .name("po-watch".into())
            .spawn(move || {
                let watch = start_watcher(&target);
                crate::dev::trace::mark("watcher (thread)");
                done.send(watch).ok();
            })
            .expect("failed to spawn thread");
        Self { root, files, watch }
    }
}

#[hotpath::measure]
pub(crate) fn walk(root: &Path, out: &mut Vec<String>) {
    for entry in walker(root).flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        if entry.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

pub(crate) type Stamp = (SystemTime, u64);

pub(crate) fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

pub(crate) fn language(path: &str) -> &'static str {
    match path.rsplit_once('.').map_or("", |(_, e)| e) {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "mjs" | "jsx" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "json" => "JSON",
        "toml" => "TOML",
        "md" => "Markdown",
        "go" => "Go",
        "ex" | "exs" => "Elixir",
        "c" | "h" => "C",
        "sh" => "Shell",
        _ => "Plain text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    
    
    
    
    
    

    #[test]
    fn elixir_files_are_named() {
        assert_eq!(language("lib/app.ex"), "Elixir");
        assert_eq!(language("lib/app.exs"), "Elixir");
    }
}
