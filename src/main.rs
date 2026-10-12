#![recursion_limit = "1024"]
mod acp;
mod config;
mod diff;
mod pr;
mod theme;
mod trace;
mod ui;
mod viewer;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use acp::{Client, Event, PermissionOption, SessionInfo};
use diff::{Change, Kind, Pair, Row};
use theme::Palette;
use ui::button::Button;
use ui::icon::{Icon, IconName, IconSize};
use ui::input::{Input, InputEvent, InputState};
use ui::list_item::ListItem;
use ui::menu::{MenuItem, MenuState, dropdown};
use ui::resizable::{SidebarsState, sidebars};
use ui::tree::{TreeEvent, TreeItem, TreeState, tree};
use futures::StreamExt;
use serde_json::Value;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState, ParentElement,
    Render, SharedString, StyleRefinement, Styled, Subscription, Task, Window, WindowBounds, WindowOptions, div, list,
    prelude::*, px, rems, size, KeyBinding,
};

gpui::actions!(po, [ToggleLeft, ToggleRight, ShowSource, ShowChanges, ToggleSplit, NewThread, ShowFinder, CloseFinder, FinderUp, FinderDown]);

const SKIP: [&str; 5] = ["target", "node_modules", "dist", "build", "__pycache__"];
const MAX_FILES: usize = 5000;

fn ignored(name: &str) -> bool {
    name.starts_with('.') || SKIP.contains(&name)
}

/// The tree under `dir` without dotfiles, `SKIP` folders and anything the `.gitignore` files
/// exclude (they apply even outside a git repository).
fn walker(dir: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(dir)
        .hidden(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.depth() == 0 || !ignored(&entry.file_name().to_string_lossy()))
        .build()
}

/// Watches `dir` and its subfolders one by one, so skipped folders such as
/// `target` never take up watches.
fn watch_dirs(watcher: &mut impl notify::Watcher, dir: &Path) {
    for entry in walker(dir).flatten() {
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            watcher.watch(entry.path(), notify::RecursiveMode::NonRecursive).ok();
        }
    }
}

type Watch = (notify::RecommendedWatcher, futures::channel::mpsc::UnboundedReceiver<notify::Event>);

/// A watcher on every folder under `root`. Adding the watches costs a syscall per folder, which
/// adds up to seconds on a big tree, so callers run this off the UI thread.
fn start_watcher(root: &Path) -> Option<Watch> {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
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
struct Prefetch {
    root: PathBuf,
    files: std::thread::JoinHandle<Vec<String>>,
    watch: futures::channel::oneshot::Receiver<Option<Watch>>,
}

impl Prefetch {
    fn start(root: PathBuf) -> Self {
        let scan = root.clone();
        let files = std::thread::spawn(move || {
            let mut files = Vec::new();
            walk(&scan, &mut files);
            trace::mark("walk (thread)");
            files
        });
        let (done, watch) = futures::channel::oneshot::channel();
        let target = root.clone();
        std::thread::spawn(move || {
            let watch = start_watcher(&target);
            trace::mark("watcher (thread)");
            done.send(watch).ok();
        });
        Self { root, files, watch }
    }
}

fn walk(root: &Path, out: &mut Vec<String>) {
    for entry in walker(root).flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

type Stamp = (SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn language(path: &str) -> &'static str {
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

/// The highlighter's name for a file: its extension, which the viewer maps to a grammar.
/// `.exs` is Elixir too; the viewer only knows that grammar as "elixir".
fn syntax(path: &str) -> SharedString {
    let ext = path.rsplit_once('.').map_or("", |(_, ext)| ext).to_lowercase();
    match ext.as_str() {
        "exs" => "elixir",
        _ => &ext,
    }
    .into()
}

/// The syntaxes of the most common file kinds in `files`, plus Markdown for the agent's
/// replies, to compile ahead of the first file that needs them.
fn common_syntaxes(files: &[String]) -> Vec<String> {
    let mut counts: HashMap<SharedString, usize> = HashMap::new();
    for file in files.iter().filter(|file| file.contains('.')) {
        *counts.entry(syntax(file)).or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    let mut out: Vec<String> = counts.into_iter().take(4).map(|(name, _)| name.to_string()).collect();
    out.push("md".into());
    out
}

/// A folder in the explorer, built from the flat file list.
#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<String>,
}

/// Every folder that holds one of `files`.
fn folders(files: &[String]) -> HashSet<SharedString> {
    let mut out = HashSet::new();
    for file in files {
        for (ix, _) in file.match_indices('/') {
            out.insert(SharedString::from(file[..ix].to_string()));
        }
    }
    out
}

/// A colour family per file kind, drawn as a small square rather than a glyph so it
/// looks the same whatever fonts are installed.
#[derive(Clone, Copy)]
enum FileKind {
    Code,
    Data,
    Doc,
    Web,
    Image,
    Other,
}

fn file_kind(name: &str) -> FileKind {
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

/// The explorer's tree.
fn explorer_items(files: &[String], expanded: &HashSet<SharedString>) -> Vec<TreeItem> {
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
        expanded: &HashSet<SharedString>,
    ) -> Vec<TreeItem> {
        let folders = dir.dirs.iter().map(|(name, child)| {
            let id = format!("{prefix}{name}");
            let open = expanded.contains(id.as_str());
            let children = items(child, &format!("{id}/"), expanded);
            TreeItem::new(id, name.clone()).children(children).expanded(open)
        });
        let files = dir.files.iter().map(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            TreeItem::new(path.clone(), name.to_string())
        });
        folders.chain(files).collect()
    }
    items(&root, "", expanded)
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Waiting,
    Working,
    Done,
    Failed,
}

#[derive(Clone)]
struct ToolBlock {
    id: String,
    title: String,
    status: Step,
    open: bool,
    arguments: String,
    result: String,
    path: Option<String>,
}

#[derive(Clone)]
struct Ask {
    request: Value,
    title: String,
    options: Vec<PermissionOption>,
    answered: Option<String>,
}

#[derive(Clone)]
enum Block {
    Text(String),
    Thought(String),
    Tool(ToolBlock),
    Permission(Ask),
    /// The agent's to-do list; each update replaces the whole list.
    Plan(Vec<(String, Step)>),
}

#[derive(Clone)]
enum Msg {
    User(String),
    Agent(Vec<Block>),
}

/// The file a tool call names: its locations, a diff, or a path in its input.
fn touched(update: &Value) -> Option<String> {
    let input = &update["rawInput"];
    update["locations"][0]["path"]
        .as_str()
        .or_else(|| update["content"].as_array()?.iter().find_map(|item| item["path"].as_str()))
        .or_else(|| input.as_str().filter(|text| !text.contains(char::is_whitespace)))
        .or_else(|| ["path", "file_path", "filePath"].iter().find_map(|key| input[key].as_str()))
        .map(String::from)
}

fn step(status: &str) -> Step {
    match status {
        "in_progress" => Step::Working,
        "completed" => Step::Done,
        "failed" => Step::Failed,
        _ => Step::Waiting,
    }
}

fn pretty(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// A diff content block as `-`/`+` lines, without the lines the edit leaves alone.
fn diff_text(item: &Value) -> String {
    const LIMIT: usize = 200;
    let old: Vec<&str> = item["oldText"].as_str().unwrap_or_default().lines().collect();
    let new: Vec<&str> = item["newText"].as_str().unwrap_or_default().lines().collect();
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(a, b)| a == b).count();
    let removed = &old[head..old.len() - tail];
    let added = &new[head..new.len() - tail];
    let mut out = vec![format!("edit {}", item["path"].as_str().unwrap_or("?"))];
    out.extend(removed.iter().chain(added).enumerate().take(LIMIT).map(|(i, line)| {
        format!("{} {line}", if i < removed.len() { '-' } else { '+' })
    }));
    if removed.len() + added.len() > LIMIT {
        out.push(format!("… {} more lines", removed.len() + added.len() - LIMIT));
    }
    out.join("\n")
}

/// Files matching `query`: name hits first (shorter names first), then path hits, each in
/// walk order; ASCII-case-insensitive substring, like the viewer's search.
/// shortcut: substring only — no fuzzy/typo-tolerant ranking; add it if finding a file by a
/// scrambled query (typing `cvs` for `code_view_state.rs`) matters.
fn match_files(files: &[String], query: &str) -> Vec<String> {
    if query.is_empty() {
        return files.to_vec();
    }
    let query = query.as_bytes();
    // Windows compared with eq_ignore_ascii_case, so no lowercase copy of each path is made.
    let contains = |text: &str| text.as_bytes().windows(query.len()).any(|window| window.eq_ignore_ascii_case(query));
    let mut hits: Vec<(u8, usize, &String)> = files
        .iter()
        .filter_map(|file| {
            let name_start = file.rfind('/').map_or(0, |slash| slash + 1);
            let rank = if contains(&file[name_start..]) { 0 } else if contains(file) { 1 } else { return None };
            Some((rank, file.len() - name_start, file))
        })
        .collect();
    hits.sort_by_key(|&(rank, name_len, _)| (rank, name_len));
    hits.into_iter().map(|(_, _, file)| file.clone()).collect()
}

/// A prompt as the user typed it, without the note `send` puts in front about the open file.
fn without_context(text: &str) -> &str {
    text.strip_prefix("[The user has `")
        .and_then(|rest| rest.split_once("]\n\n"))
        .map_or(text, |(_, prompt)| prompt)
}

/// The text a tool call carries: content blocks, diffs, then its raw output.
fn tool_result(update: &Value) -> String {
    let mut out = Vec::new();
    for item in update["content"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("content") => {
                if let Some(text) = item["content"]["text"].as_str() {
                    out.push(text.to_string());
                }
            }
            Some("diff") => out.push(diff_text(item)),
            Some("terminal") => out.push("terminal output".into()),
            _ => {}
        }
    }
    if out.is_empty() {
        return pretty(&update["rawOutput"]);
    }
    out.join("\n")
}

/// What the sidebar lists and the viewer shows.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Source,
    Diff,
}

struct Reader {
    mode: Mode,
    changes: Vec<Change>,
    changes_tree: Entity<TreeState>,
    collapsed: HashSet<SharedString>,
    picked: Option<String>,
    /// The picked file's diff, its side-by-side pairs and how many lines it adds and removes.
    rows: Rc<Vec<Row>>,
    pairs: Rc<Vec<Pair>>,
    counts: (usize, usize),
    /// The diff's rows as laid out, inline or split; only those in view are drawn.
    diff_list: ListState,
    /// Change marks for the explorer, by path.
    marks: Rc<HashMap<String, char>>,
    /// The latest `git diff` for the picked file and `git status`, run off the UI thread;
    /// replacing one drops the stale run.
    diffing: Option<Task<()>>,
    listing: Option<Task<()>>,
    scanning: Option<Task<()>>,
    root: PathBuf,
    /// What Changes mode diffs against: `HEAD`, or a PR's merge-base when reviewing.
    base: String,
    pr: Option<pr::Pr>,
    /// The directory `po` was started in: where PRs are checked out from and where Close returns to.
    home: PathBuf,
    pr_input: Entity<InputState>,
    /// Progress or failure of the last attempt to open a PR.
    pr_status: Option<String>,
    opening: bool,
    watching: Option<gpui::Task<()>>,
    files: Vec<String>,
    current: Option<String>,
    stamp: Option<Stamp>,
    code: Option<Entity<viewer::CodeViewState>>,
    /// The Ctrl+P finder, when open: its query field, the matching files and which is picked.
    finder: Option<Entity<InputState>>,
    finder_matches: Rc<Vec<String>>,
    finder_selected: usize,
    finder_list: ListState,
    finder_open: bool,
    /// Keeps the finder input's event subscription alive for the reader's lifetime.
    finder_events: Option<Subscription>,
    notice: Option<String>,
    tree: Entity<TreeState>,
    expanded: HashSet<SharedString>,
    draft: Entity<InputState>,
    thread: Vec<Msg>,
    busy: bool,
    ready: bool,
    /// A resumed session's history is being replayed.
    loading: bool,
    /// Choosing a session to resume: the agent's list once it arrives.
    picking: bool,
    sessions: Option<Vec<SessionInfo>>,
    painted: bool,
    status: String,
    client: Option<Client>,
    agent: Option<String>,
    agents: Vec<config::Agent>,
    generation: u64,
    /// The thread's messages as laid out; only those in view are drawn.
    thread_list: ListState,
    sidebars: Entity<SidebarsState>,
    /// The selection across the agent's replies.
    selection: Entity<ui::markdown::Selection>,
    thread_focus: gpui::FocusHandle,
    /// The New Thread menu, so a shortcut can open it.
    thread_menu: Entity<MenuState>,
    left_open: bool,
    right_open: bool,
    /// Whether diffs show old and new side by side instead of inline.
    split: bool,
    /// Keeps the shortcuts routed to the reader when no input is focused.
    focus: gpui::FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Reader {
    fn new(
        home: PathBuf,
        pr: Option<pr::Pr>,
        command: Option<String>,
        prefetch: Prefetch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = pr.as_ref().map_or_else(|| home.clone(), |pr| pr.worktree.clone());
        let prefetch = Some(prefetch).filter(|prefetch| prefetch.root == root);
        let (files, watch) = match prefetch {
            Some(Prefetch { files, watch, .. }) => (files.join().unwrap_or_default(), Some(watch)),
            None => {
                let mut files = Vec::new();
                walk(&root, &mut files);
                (files, None)
            }
        };
        trace::mark("walk");
        viewer::prewarm(common_syntaxes(&files));
        let pr_input = cx.new(|cx| {
            InputState::new(window, cx).auto_grow(1, 1).submit_on_enter(true).placeholder("PR number or URL")
        });
        let draft = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Message the agent")
        });
        let tree = cx.new(|cx| TreeState::new(cx));
        let changes_tree = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
            // Switching modes unmounts whatever was focused (the explorer, the viewer), which
            // leaves nothing focused and the shortcuts unrouted, so take focus back.
            cx.on_focus_lost(window, |reader, window, cx| reader.focus.focus(window, cx)),
            cx.subscribe(&tree, |reader, _, event: &TreeEvent, _| match event {
                TreeEvent::Expanded(id) => {
                    reader.expanded.insert(id.clone());
                }
                TreeEvent::Collapsed(id) => {
                    reader.expanded.remove(id);
                }
            }),
            cx.subscribe(&changes_tree, |reader, _, event: &TreeEvent, _| match event {
                TreeEvent::Expanded(id) => {
                    reader.collapsed.remove(id);
                }
                TreeEvent::Collapsed(id) => {
                    reader.collapsed.insert(id.clone());
                }
            }),
            // Moving the selection with the keyboard opens the file too, like a click.
            cx.observe_in(&tree, window, |reader, tree, window, cx| {
                let path = tree.read(cx).selected_entry().filter(|e| !e.folder).map(|e| e.id.to_string());
                if let Some(path) = path.filter(|p| reader.mode == Mode::Source && reader.current.as_deref() != Some(p)) {
                    reader.open(&path, window, cx);
                }
            }),
            cx.observe(&changes_tree, |reader, tree, cx| {
                let path = tree.read(cx).selected_entry().filter(|e| !e.folder).map(|e| e.id.to_string());
                if let Some(path) = path.filter(|p| reader.mode == Mode::Diff && reader.picked.as_deref() != Some(p)) {
                    reader.pick(&path, cx);
                }
            }),
            cx.subscribe_in(&pr_input, window, |reader, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    reader.open_pr(window, cx);
                }
            }),
            cx.subscribe_in(&draft, window, |reader, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    reader.send(window, cx);
                }
            }),
        ];
        let mut agents = config::agents(&config::load());
        if let Some(command) = command.as_ref().filter(|c| !agents.iter().any(|agent| agent.id == **c)) {
            agents.push(config::Agent { id: command.clone(), name: "Custom".into(), command: command.clone() });
        }
        let mut reader = Self {
            mode: Mode::Source,
            changes: vec![],
            changes_tree,
            collapsed: HashSet::new(),
            picked: None,
            rows: Rc::new(vec![]),
            pairs: Rc::new(vec![]),
            counts: (0, 0),
            diff_list: ListState::new(0, ListAlignment::Top, px(400.)),
            marks: Rc::default(),
            diffing: None,
            listing: None,
            scanning: None,
            root,
            base: pr.as_ref().map_or_else(|| "HEAD".to_string(), |pr| pr.base.clone()),
            pr,
            home,
            pr_input,
            pr_status: None,
            opening: false,
            watching: None,
            files,
            current: None,
            stamp: None,
            code: None,
            finder: None,
            finder_matches: Rc::new(vec![]),
            finder_selected: 0,
            finder_list: ListState::new(0, ListAlignment::Top, px(400.)),
            finder_open: false,
            finder_events: None,
            notice: None,
            tree,
            expanded: HashSet::new(),
            draft,
            thread: vec![],
            busy: false,
            ready: false,
            loading: false,
            picking: false,
            sessions: None,
            painted: false,
            status: String::new(),
            client: None,
            agent: None,
            agents,
            generation: 0,
            thread_list: ListState::new(0, ListAlignment::Top, px(400.)),
            sidebars: cx.new(|_| SidebarsState::new(px(260.0), px(460.0))),
            selection: cx.new(|_| ui::markdown::Selection::default()),
            thread_focus: cx.focus_handle(),
            thread_menu: cx.new(MenuState::new),
            left_open: true,
            right_open: true,
            split: false,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        reader.focus.focus(window, cx);
        trace::mark("reader built");
        reader.refresh_tree(cx);
        trace::mark("tree");
        if let Some(command) = command {
            reader.connect(&command, false, window, cx);
            trace::mark("connect");
        }
        reader.watch(watch, window, cx);
        trace::mark("watch");
        reader
    }

    /// Rebuilds the explorer from `files`, keeping folders open and the current file selected.
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let items = explorer_items(&self.files, &self.expanded);
        let current = self.current.clone().map(SharedString::from);
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            if let Some(ix) = current.and_then(|id| tree.index_of(&id)) {
                tree.set_selected_index(Some(ix), cx);
            }
        });
    }

    /// Takes a fresh list of changed files. In diff mode it picks a file if none is, and
    /// reloads the picked file's diff when `rediff` says it may have changed.
    fn apply_changes(&mut self, changes: Vec<Change>, rebuild: bool, rediff: bool, cx: &mut Context<Self>) {
        let listed = rebuild || self.changes != changes;
        self.changes = changes;
        if listed {
            self.marks = Rc::new(self.changes.iter().map(|c| (c.path.clone(), c.status)).collect());
        }
        if self.mode != Mode::Diff {
            return;
        }
        if listed {
            let paths: Vec<String> = self.changes.iter().map(|c| c.path.clone()).collect();
            let mut expanded = folders(&paths);
            expanded.retain(|id| !self.collapsed.contains(id));
            let items = explorer_items(&paths, &expanded);
            self.changes_tree.update(cx, |tree, cx| tree.set_items(items, cx));
        }
        let keep = self.picked.clone().filter(|path| self.changes.iter().any(|c| &c.path == path));
        match keep {
            Some(path) if rediff || listed => self.pick(&path, cx),
            Some(_) => {}
            None => match self.changes.first().map(|c| c.path.clone()) {
                Some(path) => self.pick(&path, cx),
                None => {
                    self.picked = None;
                    self.diffing = None;
                    self.set_rows(vec![]);
                }
            },
        }
        cx.notify();
    }

    /// Lists the changed files with `git` off the UI thread, then applies them as
    /// [`Self::apply_changes`] does and picks `then`, if given.
    fn refresh_changes(&mut self, rebuild: bool, then: Option<String>, cx: &mut Context<Self>) {
        let (root, base) = (self.root.clone(), self.base.clone());
        self.listing = Some(cx.spawn(async move |this, cx| {
            let changes = cx.background_executor().spawn(async move { diff::changes(&root, &base) }).await;
            this.update(cx, |reader, cx| {
                reader.apply_changes(changes, rebuild, true, cx);
                if let Some(path) = then.filter(|_| reader.mode == Mode::Diff) {
                    reader.pick(&path, cx);
                }
            })
            .ok();
        }));
    }

    /// Re-walks the workspace off the UI thread and refreshes the explorer.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.scanning = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let mut files = Vec::new();
                    walk(&root, &mut files);
                    files
                })
                .await;
            this.update(cx, |reader, cx| {
                reader.set_files(files, cx);
            })
            .ok();
        }));
    }

    /// Adopts a fresh walk of the workspace, if it changed, in the explorer and an open finder.
    fn set_files(&mut self, files: Vec<String>, cx: &mut Context<Self>) {
        if self.files == files {
            return;
        }
        self.files = files;
        self.refresh_tree(cx);
        if self.finder_open {
            let picked = self.finder_selected;
            self.refind(cx);
            self.finder_selected = picked.min(self.finder_matches.len().saturating_sub(1));
        }
        cx.notify();
    }

    /// Shows the diff `rows`, and works out their side-by-side pairs and counts once.
    fn set_rows(&mut self, rows: Vec<Row>) {
        self.counts = rows.iter().fold((0, 0), |(a, r), row| match row.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            _ => (a, r),
        });
        self.pairs = Rc::new(diff::split(&rows));
        self.rows = Rc::new(rows);
        self.relist_diff();
    }

    fn relist_diff(&mut self) {
        self.diff_list.reset(if self.split { self.pairs.len() } else { self.rows.len() });
    }

    /// Shows `path`'s diff in the viewer, once `git` has produced it off the UI thread.
    fn pick(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(change) = self.changes.iter().find(|c| c.path == path).cloned() else { return };
        if self.picked.as_deref() != Some(path) {
            self.picked = Some(path.to_string());
            // The last file's diff isn't shown under this one's name while this one loads.
            self.set_rows(vec![]);
            let id = SharedString::from(path.to_string());
            self.changes_tree.update(cx, |tree, cx| {
                if let Some(ix) = tree.index_of(&id) {
                    tree.set_selected_index(Some(ix), cx);
                }
            });
        }
        let (root, base, path) = (self.root.clone(), self.base.clone(), path.to_string());
        self.diffing = Some(cx.spawn(async move |this, cx| {
            let rows = cx.background_executor().spawn(async move { diff::file_diff(&root, &base, &change) }).await;
            this.update(cx, |reader, cx| {
                if reader.picked.as_deref() == Some(&path) {
                    reader.set_rows(rows);
                    reader.diffing = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Starts checking out the PR typed into the strip; the window switches to it when that finishes.
    fn open_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = self.pr_input.read(cx).value().trim().to_string();
        if spec.is_empty() || self.opening {
            return;
        }
        self.opening = true;
        self.pr_status = Some(format!("Checking out {spec}…"));
        cx.notify();
        let home = self.home.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { pr::Pr::open(&home, &spec) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.opening = false;
                        match result {
                            Ok(pr) => {
                                reader.pr_status = None;
                                reader.pr_input.update(cx, |input, cx| input.set_value("", window, cx));
                                let root = pr.worktree.clone();
                                reader.switch_root(root, Some(pr), window, cx);
                            }
                            Err(error) => {
                                log::warn!("open PR: {error}");
                                reader.pr_status = Some(error);
                                cx.notify();
                            }
                        }
                    })
                })
                .ok();
        })
        .detach();
    }

    /// Leaves the PR review and goes back to the directory `po` started in.
    fn close_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let home = self.home.clone();
        self.switch_root(home, None, window, cx);
    }

    /// Points the whole window (explorer, viewer, watcher, agent) at `root`, which is
    /// `pr`'s worktree when reviewing one. The previous PR's worktree is removed.
    fn switch_root(&mut self, root: PathBuf, pr: Option<pr::Pr>, window: &mut Window, cx: &mut Context<Self>) {
        // Stop watching the old tree before it can be deleted.
        self.watching = None;
        self.base = pr.as_ref().map_or_else(|| "HEAD".to_string(), |pr| pr.base.clone());
        self.root = root;
        drop(std::mem::replace(&mut self.pr, pr));
        self.files.clear();
        self.expanded.clear();
        self.collapsed.clear();
        self.current = None;
        self.stamp = None;
        self.code = None;
        self.notice = None;
        self.picked = None;
        self.diffing = None;
        self.set_rows(vec![]);
        self.changes.clear();
        self.marks = Rc::default();
        self.refresh_tree(cx);
        self.rescan(cx);
        // The agent works in the old directory, so it restarts in the new one.
        if let Some(agent) = self.agent.clone() {
            self.connect(&agent, false, window, cx);
        }
        self.watch(None, window, cx);
        if self.pr.is_some() {
            self.mode = Mode::Diff;
        }
        self.refresh_changes(true, None, cx);
        cx.notify();
    }

    fn toggle_left(&mut self, cx: &mut Context<Self>) {
        self.left_open = !self.left_open;
        cx.notify();
    }

    fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if self.split != split {
            self.split = split;
            self.relist_diff();
        }
        cx.notify();
    }

    fn toggle_right(&mut self, cx: &mut Context<Self>) {
        self.right_open = !self.right_open;
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Diff {
            // Show the last known changes at once; `git` confirms them in the background.
            self.apply_changes(self.changes.clone(), true, false, cx);
            self.refresh_changes(true, None, cx);
        }
        cx.notify();
    }

    /// Watches the workspace: refreshes the explorer when files come or go, and
    /// reloads the open file when it changes on disk.
    fn watch(&mut self, prepared: Option<futures::channel::oneshot::Receiver<Option<Watch>>>, window: &Window, cx: &mut Context<Self>) {
        use notify::Watcher;
        let root = self.root.clone();
        let handle = window.window_handle();
        // Kept so the watcher stops when the root changes.
        self.watching = Some(cx.spawn(async move |this, cx| {
            // Adding the watches takes a while on a big tree; the window doesn't wait for it.
            let watch = match prepared {
                Some(prepared) => prepared.await.ok().flatten(),
                None => {
                    let target = root.clone();
                    cx.background_executor().spawn(async move { start_watcher(&target) }).await
                }
            };
            // Moved in so the watcher lives as long as this task.
            let Some((mut watcher, mut rx)) = watch else { return };
            while let Some(first) = rx.next().await {
                // Let a burst of events settle, then handle them together.
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let mut events = vec![first];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                // Files coming, going or being renamed change the explorer; edits alone don't,
                // so only those re-walk the tree.
                let (mut changed, mut moved) = (false, false);
                let mut touched = HashSet::new();
                let (gitignore, _) = ignore::gitignore::Gitignore::new(root.join(".gitignore"));
                for event in events.iter().filter(|e| !e.kind.is_access()) {
                    for path in &event.paths {
                        let Ok(rel) = path.strip_prefix(&root) else { continue };
                        if rel.components().any(|c| ignored(&c.as_os_str().to_string_lossy()))
                            || gitignore.matched_path_or_any_parents(rel, path.is_dir()).is_ignore()
                        {
                            continue;
                        }
                        changed = true;
                        moved |= !matches!(
                            event.kind,
                            notify::EventKind::Modify(notify::event::ModifyKind::Data(_) | notify::event::ModifyKind::Metadata(_))
                        );
                        touched.insert(rel.to_string_lossy().replace('\\', "/"));
                        if path.is_dir() {
                            watch_dirs(&mut watcher, path);
                        }
                    }
                }
                if !changed {
                    continue;
                }
                let Ok(current) = this.read_with(cx, |reader, _| reader.current.clone()) else { break };
                let scan_root = root.clone();
                let Ok(base) = this.read_with(cx, |reader, _| reader.base.clone()) else { break };
                let (files, seen, changes) = cx
                    .background_executor()
                    .spawn(async move {
                        let files = moved.then(|| {
                            let mut files = Vec::new();
                            walk(&scan_root, &mut files);
                            files
                        });
                        let seen = current.as_ref().and_then(|path| stamp(&scan_root.join(path)));
                        (files, seen, diff::changes(&scan_root, &base))
                    })
                    .await;
                let alive = handle.update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        if let Some(files) = files {
                            reader.set_files(files, cx);
                        }
                        let rediff = reader.picked.as_ref().is_some_and(|path| touched.contains(path));
                        reader.apply_changes(changes, false, rediff, cx);
                        if let (Some(path), Some(seen)) = (reader.current.clone(), seen) {
                            if reader.stamp != Some(seen) {
                                log::info!("reloading {path}");
                                reader.open(&path, window, cx);
                            }
                        }
                    })
                });
                if !matches!(alive, Ok(Ok(()))) {
                    break;
                }
            }
            drop(watcher.unwatch(&root));
        }));
    }

    /// Replaces the running agent with `key`'s, with a fresh thread, or with the agent's session
    /// list to resume from when `resume`.
    fn connect(&mut self, key: &str, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.client = None;
        self.agent = Some(key.to_string());
        self.thread.clear();
        self.relist(0);
        self.selection.update(cx, |s, _| s.clear());
        self.busy = false;
        self.ready = false;
        self.loading = false;
        self.picking = resume;
        self.sessions = None;
        let command = self.agents.iter().find(|agent| agent.id == key).map_or(key.to_string(), |agent| agent.command.clone());
        self.status = if resume { "listing sessions…" } else { "starting…" }.into();
        match Client::spawn(&command, &self.root, resume) {
            Ok((client, mut events)) => {
                self.client = Some(client);
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    // An agent that never answers the handshake would leave the thread on "starting…".
                    let watchdog = this.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(20)).await;
                        watchdog
                            .update(cx, |reader, cx| {
                                if reader.generation == generation && !reader.ready && reader.client.is_some() {
                                    reader.status = "not responding".into();
                                    reader.fail("The agent did not answer the handshake. Check that its command speaks ACP.".into());
                                    cx.notify();
                                }
                            })
                            .ok();
                    })
                    .detach();
                    while let Some(event) = events.next().await {
                        let done = handle.update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                if reader.generation == generation {
                                    reader.on_event(event, window, cx);
                                }
                                reader.generation == generation
                            })
                        });
                        if !matches!(done, Ok(Ok(true))) {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                log::error!("agent: {error:#}");
                self.status = "not running".into();
                self.fail(format!("{error:#}"));
            }
        }
        cx.notify();
    }

    /// Starts a new thread with `key`'s agent, replacing any running one.
    fn new_thread(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("agent: new thread with {key}");
        self.connect(key, false, window, cx);
    }

    /// Replaces the running agent with `key`'s, offering its sessions to resume.
    fn resume_thread(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("agent: resuming with {key}");
        self.connect(key, true, window, cx);
    }

    /// Opens a listed session; its history arrives as updates before the agent is ready.
    fn resume_session(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(client) = &self.client else { return };
        if self.loading || !client.load(id) {
            return;
        }
        self.loading = true;
        self.sessions = None;
        self.status = "loading session…".into();
        cx.notify();
    }

    fn agent_label(&self) -> Option<&str> {
        let key = self.agent.as_ref()?;
        self.agents.iter().find(|agent| agent.id == *key).map(|agent| agent.name.as_str())
    }

    fn fail(&mut self, text: String) {
        self.busy = false;
        self.thread.push(Msg::Agent(vec![Block::Text(format!("**Error:** {text}"))]));
        self.relist(self.thread.len() - 1);
    }

    /// Tells the thread's list that messages from `from` on were added or changed, so it lays
    /// them out again.
    fn relist(&mut self, from: usize) {
        let listed = self.thread_list.item_count();
        let from = from.min(listed).min(self.thread.len());
        self.thread_list.splice(from..listed, self.thread.len() - from);
    }

    fn blocks(&mut self) -> &mut Vec<Block> {
        if !matches!(self.thread.last(), Some(Msg::Agent(_))) {
            self.thread.push(Msg::Agent(vec![]));
            self.relist(self.thread.len() - 1);
        }
        match self.thread.last_mut() {
            Some(Msg::Agent(blocks)) => blocks,
            _ => unreachable!("an agent message was just pushed"),
        }
    }

    fn on_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        // Most events change the last message; a tool call update may change an earlier one.
        let mut changed = self.thread.len().saturating_sub(1);
        match event {
            Event::Sessions(sessions) => {
                self.status = if sessions.is_empty() { "no sessions".into() } else { "pick a session".into() };
                self.sessions = Some(sessions);
            }
            Event::Ready { agent } => {
                self.ready = true;
                self.loading = false;
                self.picking = false;
                log::info!("agent: {agent} ready");
                self.status = "ready".into();
            }
            Event::Update(update) => {
                if let Some(ix) = self.tool_message(&update) {
                    changed = changed.min(ix);
                }
                // A replayed history would otherwise open every file it ever touched.
                if let Some((path, finished)) = self.update(&update).filter(|_| !self.loading) {
                    self.follow(&path, finished, window, cx);
                }
            }
            Event::Permission { request, title, options } => {
                self.blocks().push(Block::Permission(Ask { request, title, options, answered: None }));
            }
            Event::Stopped(reason) => {
                self.busy = false;
                log::info!("agent: stopped ({reason})");
            }
            Event::Failed(error) => {
                if !self.ready {
                    self.status = "failed to start".into();
                }
                self.fail(error);
            }
            Event::Exited(status) => {
                log::warn!("agent exited: {status}");
                self.ready = false;
                self.status = "exited".into();
                if self.busy {
                    self.fail(format!("the agent exited ({status})"));
                }
            }
        }
        self.relist(changed);
        self.thread_list.scroll_to_end();
        cx.notify();
    }

    /// The message holding the tool call an update is for, if it updates one.
    fn tool_message(&self, update: &Value) -> Option<usize> {
        if update["sessionUpdate"].as_str() != Some("tool_call_update") {
            return None;
        }
        let id = update["toolCallId"].as_str().unwrap_or_default();
        self.thread.iter().rposition(|msg| match msg {
            Msg::Agent(blocks) => blocks.iter().any(|b| matches!(b, Block::Tool(tool) if tool.id == id)),
            Msg::User(_) => false,
        })
    }

    fn update(&mut self, update: &Value) -> Option<(String, bool)> {
        let text = update["content"]["text"].as_str().unwrap_or_default();
        let mut known: Option<String> = None;
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => match self.blocks().last_mut() {
                Some(Block::Text(body)) => body.push_str(text),
                _ => self.blocks().push(Block::Text(text.to_string())),
            },
            Some("user_message_chunk") if self.loading || !self.busy => {
                let text = without_context(text);
                match self.thread.last_mut() {
                    Some(Msg::User(body)) => body.push_str(text),
                    _ => {
                        self.thread.push(Msg::User(text.to_string()));
                        self.relist(self.thread.len() - 1);
                    }
                }
            }
            Some("agent_thought_chunk") => match self.blocks().last_mut() {
                Some(Block::Thought(body)) => body.push_str(text),
                _ => self.blocks().push(Block::Thought(text.to_string())),
            },
            Some("plan") => {
                let entries = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| (e["content"].as_str().unwrap_or_default().to_string(), step(e["status"].as_str().unwrap_or("pending"))))
                    .collect();
                let blocks = self.blocks();
                match blocks.iter_mut().find(|b| matches!(b, Block::Plan(_))) {
                    Some(block) => *block = Block::Plan(entries),
                    None => blocks.push(Block::Plan(entries)),
                }
            }
            Some("tool_call") => self.blocks().push(Block::Tool(ToolBlock {
                id: update["toolCallId"].as_str().unwrap_or_default().to_string(),
                title: update["title"].as_str().unwrap_or("Tool").to_string(),
                status: step(update["status"].as_str().unwrap_or("pending")),
                open: false,
                arguments: pretty(&update["rawInput"]),
                result: tool_result(update),
                path: touched(update),
            })),
            Some("tool_call_update") => {
                let id = update["toolCallId"].as_str().unwrap_or_default();
                known = None;
                for msg in self.thread.iter_mut().rev() {
                    let Msg::Agent(blocks) = msg else { continue };
                    let found = blocks.iter_mut().rev().find_map(|b| match b {
                        Block::Tool(tool) if tool.id == id => Some(tool),
                        _ => None,
                    });
                    if let Some(tool) = found {
                        if let Some(title) = update["title"].as_str() {
                            tool.title = title.to_string();
                        }
                        if let Some(status) = update["status"].as_str() {
                            tool.status = step(status);
                        }
                        if !update["rawInput"].is_null() {
                            tool.arguments = pretty(&update["rawInput"]);
                        }
                        let result = tool_result(update);
                        if !result.is_empty() {
                            tool.result = result;
                        }
                        if let Some(path) = touched(update) {
                            tool.path = Some(path);
                        }
                        known = tool.path.clone();
                        break;
                    }
                }
            }
            _ => {}
        }
        let kind = update["sessionUpdate"].as_str()?;
        if kind != "tool_call" && kind != "tool_call_update" {
            return None;
        }
        let path = touched(update).or(known)?;
        Some((path, update["status"].as_str() == Some("completed")))
    }

    /// Shows the file a tool call touches; a finished call reloads it to show the change.
    fn follow(&mut self, path: &str, finished: bool, window: &mut Window, cx: &mut Context<Self>) {
        let path = Path::new(path);
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        if relative.is_absolute() || !self.root.join(relative).is_file() {
            return;
        }
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !self.files.contains(&relative) {
            self.rescan(cx);
        }
        if self.current.as_deref() != Some(&relative) || finished {
            log::info!("agent: showing {relative}");
            self.open(&relative, window, cx);
        }
        if self.mode == Mode::Diff {
            self.refresh_changes(false, Some(relative), cx);
        }
    }

    fn answer(&mut self, msg: usize, block: usize, option: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.get_mut(msg) else { return };
        let Some(Block::Permission(ask)) = blocks.get_mut(block) else { return };
        let (Some(choice), None) = (ask.options.get(option), &ask.answered) else { return };
        if let Some(client) = &self.client {
            client.answer(ask.request.clone(), Some(&choice.id));
        }
        ask.answered = Some(choice.name.clone());
        self.relist(msg);
        cx.notify();
    }

    /// Ctrl+P: shows the finder over the workspace's files, query and selection preserved.
    fn show_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finder.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search files…"));
            // Every keystroke refilters the matches; Enter picks the selected file.
            self.finder_events = Some(cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.refind(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.accept_picked(window, cx),
            }));
            self.finder = Some(input);
        }
        self.finder_open = true;
        self.refind(cx);
        self.finder.as_ref().unwrap().update(cx, |input, cx| {
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Refilters the finder's matches for its query and rewinds the selection to the first.
    fn refind(&mut self, cx: &mut Context<Self>) {
        let query = self.finder.as_ref().map_or_else(String::new, |input| input.read(cx).value().to_string());
        self.finder_matches = Rc::new(match_files(&self.files, &query));
        self.finder_selected = 0;
        self.finder_list.reset(self.finder_matches.len());
        self.finder_list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.) });
    }

    /// Moves the finder's selection by `delta` rows, wrapping, and keeps it in view.
    fn step_finder(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.finder_matches.is_empty() {
            return;
        }
        let len = self.finder_matches.len() as isize;
        self.finder_selected = (self.finder_selected as isize + delta).rem_euclid(len) as usize;
        self.finder_list.scroll_to_reveal_item(self.finder_selected);
        cx.notify();
    }

    /// Opens the file under the finder's selection and closes the finder.
    fn accept_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.finder_matches.get(self.finder_selected).cloned() else { return };
        self.accept_path(&path, window, cx);
    }

    /// Opens `path` in the viewer and closes the finder.
    fn accept_path(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open(path, window, cx);
        // A file that can't be shown leaves the finder up so the query isn't lost.
        if self.notice.is_none() {
            self.hide_finder(window, cx);
            self.set_mode(Mode::Source, cx);
        }
    }

    /// Hides the finder and gives the keys back to the reader.
    fn hide_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder_open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// The finder overlay, while Ctrl+P is open: a query field over the matching files.
    fn finder(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        let (popover, border, muted, radius) = (palette.popover, palette.border, palette.muted_foreground, palette.radius);
        let Some(input) = self.finder.clone() else { return div().into_any_element() };
        let matches = self.finder_matches.clone();
        let this = cx.entity();
        div()
            .absolute()
            .inset_0()
            .id("finder-backdrop")
            .occlude()
            .on_click(cx.listener(|reader, _: &gpui::ClickEvent, window, cx| reader.hide_finder(window, cx)))
            .flex()
            .justify_center()
            .pt_2()
            .child(
                div()
                    .id("finder")
                    .occlude()
                    .w(px(560.0))
                    .flex()
                    .flex_col()
                    .key_context("Finder")
                    .on_action(cx.listener(|reader, _: &CloseFinder, window, cx| reader.hide_finder(window, cx)))
                    .on_action(cx.listener(|reader, _: &FinderUp, _, cx| reader.step_finder(-1, cx)))
                    .on_action(cx.listener(|reader, _: &FinderDown, _, cx| reader.step_finder(1, cx)))
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .bg(popover)
                    .overflow_hidden()
                    .child(Input::new(&input).field())
                    .child(if matches.is_empty() {
                        div().h(px(380.0)).flex().justify_center().pt_3().text_color(muted).child("No matching files").into_any_element()
                    } else {
                        div().h(px(380.0)).border_t_1().border_color(border).child(list(
                            self.finder_list.clone(),
                            cx.processor(move |reader: &mut Reader, ix, _, _| {
                                let Some(path): Option<&String> = matches.get(ix) else { return div().into_any_element() };
                                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                                let dir = path.strip_suffix(name.as_str()).unwrap_or("").to_string();
                                let path = path.clone();
                                let this = this.clone();
                                ListItem::new(("file", ix))
                                    .selected(ix == reader.finder_selected)
                                    .on_click(move |_, window, cx| this.update(cx, |reader, cx| reader.accept_path(&path, window, cx)))
                                    .child(name)
                                    .when(!dir.is_empty(), |row| row.child(div().text_color(muted).child(dir)))
                                    .into_any_element()
                            }),
                        )
                        .size_full())
                        .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .px_3()
                            .py_1()
                            .border_t_1()
                            .border_color(border)
                            .text_color(muted)
                            .child(if self.files.len() >= MAX_FILES { format!("first {MAX_FILES} files only") } else { "↑↓ navigate".to_string() })
                            .child("↵ open · esc close"),
                    ),
            )
            .into_any_element()
    }

    fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let full = self.root.join(path);
        let too_big = std::fs::metadata(&full).map_or(false, |m| m.len() > 4 * 1024 * 1024);
        // A file too big to show isn't read at all.
        let read = if too_big { Ok(None) } else { std::fs::read_to_string(&full).map(Some) };
        match read {
            Ok(None) => self.notice = Some(format!("{path} is too large to show")),
            Ok(Some(text)) => {
                match &self.code {
                    // Same file: swap the text in place so the scroll position and caret survive.
                    Some(code) if self.current.as_deref() == Some(path) => {
                        code.update(cx, |state, cx| state.set_text(text, cx));
                    }
                    _ => self.code = Some(cx.new(|cx| viewer::CodeViewState::new(text, syntax(path), window, cx))),
                }
                self.notice = None;
            }
            Err(error) => {
                log::warn!("open {path}: {error}");
                self.code = None;
                self.notice = Some(format!("{path} is not a text file"));
            }
        }
        self.current = Some(path.to_string());
        self.stamp = stamp(&full);
        let id = SharedString::from(path.to_string());
        self.tree.update(cx, |tree, cx| {
            if let Some(ix) = tree.index_of(&id) {
                tree.set_selected_index(Some(ix), cx);
            }
        });
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.draft.read(cx).value().trim().to_string();
        if prompt.is_empty() || self.busy {
            return;
        }
        let Some(client) = self.client.as_ref().filter(|_| self.ready) else {
            let why = if self.agent.is_some() { "The agent is not ready yet." } else { "Pick an agent from New Thread first." };
            self.fail(why.into());
            cx.notify();
            return;
        };
        let context = match &self.current {
            Some(file) => format!("[The user has `{file}` open in the viewer.]\n\n{prompt}"),
            None => prompt.clone(),
        };
        if !client.prompt(&context) {
            self.fail("The agent has no session yet.".into());
            cx.notify();
            return;
        }
        self.draft.update(cx, |input, cx| input.set_value("", window, cx));
        self.busy = true;
        self.thread.push(Msg::User(prompt));
        self.thread.push(Msg::Agent(vec![]));
        self.relist(self.thread.len() - 2);
        self.thread_list.scroll_to_end();
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(client) = &self.client else { return };
        client.cancel();
        for msg in &mut self.thread {
            let Msg::Agent(blocks) = msg else { continue };
            for block in blocks {
                if let Block::Permission(ask) = block {
                    if ask.answered.is_none() {
                        client.answer(ask.request.clone(), None);
                        ask.answered = Some("Cancelled".into());
                    }
                }
            }
        }
        self.relist(0);
        cx.notify();
    }

    fn toggle_tool(&mut self, msg: usize, block: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.get_mut(msg) else { return };
        let Some(Block::Tool(tool)) = blocks.get_mut(block) else { return };
        tool.open = !tool.open;
        self.relist(msg);
        cx.notify();
    }

    /// The strip under the explorer header in Changes mode: an input to open a PR for
    /// review, or the open PR's summary with a way out.
    fn pr_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let (border, muted, danger) = (colors.border, colors.muted_foreground, colors.danger);
        let this = cx.entity();
        let body: AnyElement = match &self.pr {
            Some(pr) => div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("PR #{} · {} files", pr.number, self.changes.len())),
                )
                .child(
                    Button::new("pr-close")
                        .label("Close")
                        .ghost()
                        .small()
                        .on_click(move |_, window, cx| this.update(cx, |reader, cx| reader.close_pr(window, cx))),
                )
                .into_any_element(),
            None => {
                let open = Button::new("pr-open")
                    .label(if self.opening { "…" } else { "Open" })
                    .small()
                    .outline()
                    .on_click(move |_, window, cx| this.update(cx, |reader, cx| reader.open_pr(window, cx)));
                let status = self.pr_status.as_ref().map(|text| {
                    let failed = !self.opening;
                    div()
                        .text_xs()
                        .text_color(if failed { danger } else { muted })
                        .max_h(rems(5.))
                        .overflow_hidden()
                        .child(text.clone())
                });
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.pr_input)),
                            )
                            .child(open),
                    )
                    .children(status)
                    .into_any_element()
            }
        };
        div().flex_none().px_3().py_2().border_b_1().border_color(border).child(body)
    }

    fn explorer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let this = cx.entity();
        let mode = self.mode;
        let (source, changes) = (cx.entity(), cx.entity());
        let modes = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("mode-source")
                    .icon(IconName::Folder)
                    .ghost()
                    .small()
                    .tooltip("Source (Ctrl+1)")
                    .selected(mode == Mode::Source)
                    .on_click(move |_, _, cx| source.update(cx, |reader, cx| reader.set_mode(Mode::Source, cx))),
            )
            .child(
                Button::new("mode-diff")
                    .icon(IconName::Replace)
                    .ghost()
                    .small()
                    .tooltip("Changes (Ctrl+2)")
                    .selected(mode == Mode::Diff)
                    .on_click(move |_, _, cx| changes.update(cx, |reader, cx| reader.set_mode(Mode::Diff, cx))),
            );
        let title = match mode {
            Mode::Source => "Explorer",
            Mode::Diff => "Changes",
        };
        let state = if mode == Mode::Diff { &self.changes_tree } else { &self.tree };
        let marks = if mode == Mode::Diff { self.marks.clone() } else { Rc::default() };
        // Themes may lack a green (Rosé Pine's success is teal), so added files get their own.
        let (muted, danger) = (colors.muted_foreground, colors.danger);
        let kinds = [colors.primary, colors.warning, colors.info, gpui::hsla(0.37, 0.5, 0.5, 1.0)];
        let body: AnyElement = if mode == Mode::Diff && self.changes.is_empty() {
            centered("No changes".into(), cx)
        } else {
            tree(state, move |ix, entry, selected, right_clicked, _, _| {
                let path = entry.id.clone();
                let name = entry.label.to_string();
                let lead: AnyElement = if entry.folder {
                    div()
                        .w(IconSize::XSmall.rems())
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(
                            Icon::new(if entry.expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                                .size(IconSize::XSmall)
                                .color(muted),
                        )
                        .into_any_element()
                } else {
                    let tint = match file_kind(&name) {
                        FileKind::Code => kinds[0],
                        FileKind::Data => kinds[1],
                        FileKind::Doc => muted,
                        FileKind::Web => kinds[2],
                        FileKind::Image => kinds[3],
                        FileKind::Other => muted.opacity(0.6),
                    };
                    div().size(rems(0.5)).flex_none().rounded(px(2.0)).bg(tint).into_any_element()
                };
                let mark = marks.get(path.as_ref()).map(|status| {
                    let color = match status {
                        'A' | 'U' => kinds[3],
                        'D' => danger,
                        _ => kinds[1],
                    };
                    div().w(rems(0.75)).flex_none().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(color).child(status.to_string())
                });
                let label = div()
                    .flex()
                    .items_center()
                    // The chevron svg carries ~4px of padding on its right, so folders take
                    // a smaller gap to sit the name as far from the glyph as files do.
                    .when(entry.folder, |d| d.gap_1())
                    .when(!entry.folder, |d| d.gap_2())
                    .child(lead)
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .children(mark);
                let folder = entry.folder;
                let this = this.clone();
                ListItem::new(ix)
                    .selected(selected)
                    .secondary_selected(right_clicked)
                    .pl(rems(0.5 + entry.depth as f32))
                    .text_sm()
                    .child(label)
                    .on_click(move |_, window, cx| {
                        if folder {
                            return;
                        }
                        this.update(cx, |reader, cx| match mode {
                            Mode::Source => reader.open(&path, window, cx),
                            Mode::Diff => reader.pick(&path, cx),
                        });
                    })
                    .into_any_element()
            })
            .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .child(header_with(
                title,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_text(
                        match &self.pr {
                            Some(pr) => format!("PR #{} {}", pr.number, pr.title),
                            None => self.root.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string()),
                        },
                        cx,
                    ))
                    .child(modes),
                cx,
            ))
            .children((mode == Mode::Diff).then(|| self.pr_strip(cx)))
            .child(div().flex_1().min_h_0().p_1().child(body))
    }

    fn diff_viewer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let Some(path) = self.picked.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .bg(colors.background)
                .child(header("Changes", String::new(), cx))
                .child(div().flex_1().min_h_0().child(centered("Pick a changed file".into(), cx)));
        };
        let (rows, pairs) = (self.rows.clone(), self.pairs.clone());
        let (added, removed) = self.counts;
        // Themes may lack a green (Rosé Pine's success is teal), so added lines get their own.
        let (success, danger, muted, border) =
            (gpui::hsla(0.37, 0.5, 0.5, 1.0), colors.danger, colors.muted_foreground, colors.border);
        let font = cx.global::<Palette>().mono_font_family.clone();
        let split = self.split;
        // Only the rows in view are built, so a long diff costs no more than a short one.
        let body: AnyElement = if rows.is_empty() && self.diffing.is_some() {
            div().into_any_element()
        } else if rows.is_empty() {
            centered("No textual changes".into(), cx)
        } else {
            list(self.diff_list.clone(), move |ix, _, _| {
                let line = div().flex().w_full().min_h(rems(1.5)).items_start().font_family(font.clone()).text_sm();
                if split {
                    let cell = |cell: &Option<diff::Cell>, first: bool| {
                        let half = div().flex_1().min_w_0().self_stretch().flex().items_start().gap_2();
                        let half = if first { half.border_r_1().border_color(border) } else { half };
                        let Some(cell) = cell else { return half.bg(muted.opacity(0.06)) };
                        let tint = match cell.kind {
                            Kind::Added => Some(success.opacity(0.16)),
                            Kind::Removed => Some(danger.opacity(0.16)),
                            _ => None,
                        };
                        let half = match tint {
                            Some(tint) => half.bg(tint),
                            None => half,
                        };
                        half.child(div().w(rems(3.)).flex_none().text_right().text_color(muted).child(cell.number.to_string()))
                            .child(div().flex_1().min_w_0().child(cell.text.clone()))
                    };
                    return match &pairs[ix] {
                        Pair::Hunk(text) => line.px_2().bg(muted.opacity(0.12)).text_color(muted).child(text.clone()),
                        Pair::Line(old, new) => line.child(cell(old, true)).child(cell(new, false)),
                    }
                    .into_any_element();
                }
                let row = &rows[ix];
                let number = |n: Option<u32>| {
                    div().w(rems(3.)).flex_none().text_right().text_color(muted).child(n.map_or(String::new(), |n| n.to_string()))
                };
                let (tint, sign) = match row.kind {
                    Kind::Added => (Some(success.opacity(0.16)), "+"),
                    Kind::Removed => (Some(danger.opacity(0.16)), "-"),
                    Kind::Hunk => (Some(muted.opacity(0.12)), ""),
                    Kind::Context => (None, " "),
                };
                let line = line.gap_2();
                let line = match tint {
                    Some(tint) => line.bg(tint),
                    None => line,
                };
                if row.kind == Kind::Hunk {
                    return line.px_2().text_color(muted).child(row.text.clone()).into_any_element();
                }
                line.child(number(row.old))
                    .child(number(row.new))
                    .child(div().w(rems(0.75)).flex_none().text_color(muted).child(sign))
                    .child(div().flex_1().min_w_0().child(row.text.clone()))
                    .into_any_element()
            })
            .size_full()
            .into_any_element()
        };
        let right = div()
            .flex()
            .gap_2()
            .text_xs()
            .child(div().text_color(success).child(format!("+{added}")))
            .child(div().text_color(danger).child(format!("-{removed}")));
        let (inline_view, split_view) = (cx.entity(), cx.entity());
        let right = div()
            .flex()
            .items_center()
            .gap_3()
            .child(right)
            .child(
                div()
                    .flex()
                    .child(
                        Button::new("view-inline")
                            .label("Inline")
                            .tooltip("Toggle inline/split (Ctrl+\\)")
                            .ghost()
                            .xsmall()
                            .selected(!self.split)
                            .on_click(move |_, _, cx| inline_view.update(cx, |reader, cx| reader.set_split(false, cx))),
                    )
                    .child(
                        Button::new("view-split")
                            .label("Split")
                            .tooltip("Toggle inline/split (Ctrl+\\)")
                            .ghost()
                            .xsmall()
                            .selected(self.split)
                            .on_click(move |_, _, cx| split_view.update(cx, |reader, cx| reader.set_split(true, cx))),
                    ),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .child(header_with(&path, right, cx))
            .child(div().flex_1().min_h_0().border_color(border).child(body))
    }

    fn viewer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let title = self.current.clone().unwrap_or_else(|| "No file open".into());
        let body: AnyElement = match (&self.code, &self.notice) {
            (_, Some(notice)) => centered(notice.clone(), cx),
            // Cached: the viewer redraws when it changes, not with every agent update or caret blink elsewhere.
            (Some(code), None) => code.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (None, None) => centered("Pick a file in the explorer".into(), cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .child(header(
                &title,
                self.current.as_deref().map(language).unwrap_or("").to_string(),
                cx,
            ))
            .child(div().flex_1().min_h_0().child(body))
    }

    fn agent_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        self.selection.update(cx, |s, _| s.begin_frame());
        // Only the messages in view are built and laid out, however long the thread gets.
        let selection = self.selection.clone();
        let thread: AnyElement = if self.agent.is_none() {
            centered("Choose an agent from New Thread".into(), cx)
        } else if self.picking && self.thread.is_empty() {
            match &self.sessions {
                Some(sessions) if !self.loading && !sessions.is_empty() => session_picker(sessions, &this, cx),
                Some(sessions) if !self.loading => centered(format!("No sessions to resume in {}", self.root.display()), cx),
                _ => centered(self.status.clone(), cx),
            }
        } else {
            list(
                self.thread_list.clone(),
                cx.processor(move |reader: &mut Self, ix: usize, window, cx| {
                    let Some(msg) = reader.thread.get(ix) else { return div().into_any_element() };
                    let last = ix + 1 == reader.thread.len();
                    div()
                        .px_3()
                        .pt_3()
                        .when(last, |this| this.pb_3())
                        .child(render_msg(ix, msg, &this, &selection, window, cx))
                        .into_any_element()
                }),
            )
            .size_full()
            .into_any_element()
        };
        let colors = cx.global::<Palette>();
        let (send, stop, picker) = (cx.entity(), cx.entity(), cx.entity());
        let agents = self.agents.clone();
        let action = if self.busy {
            Button::new("stop")
                .icon(IconName::Square)
                .ghost()
                .rounded(gpui::px(999.0))
                .small()
                .tooltip("Stop")
                .on_click(move |_, _, cx| stop.update(cx, |reader, cx| reader.stop(cx)))
        } else {
            let send = Button::new("send")
                .icon(IconName::ArrowUp)
                .ghost()
                .rounded(gpui::px(999.0))
                .small()
                .tooltip("Send")
                .on_click(move |_, window, cx| send.update(cx, |reader, cx| reader.send(window, cx)));
            // Quiet until there is something to send.
            if self.draft.read(cx).value().trim().is_empty() { send } else { send.primary() }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .child(header_with(
                self.agent_label().unwrap_or("Agent"),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.agent.is_some().then(|| status_text(self.status.clone(), cx)))
                    .child(
                        dropdown(
                            "agent-select",
                            Button::new("agent-select")
                                .label("New Thread")
                                .tooltip("New thread (Ctrl+N)")
                                .small()
                                .outline()
                                .dropdown_caret(true),
                            move || {
                                let new = agents.iter().map(|agent| {
                                    let (picker, key) = (picker.clone(), agent.id.clone());
                                    MenuItem::new(agent.name.clone(), move |window, cx| {
                                        picker.update(cx, |reader, cx| reader.new_thread(&key, window, cx))
                                    })
                                });
                                let resume = agents.iter().map(|agent| {
                                    let (picker, key) = (picker.clone(), agent.id.clone());
                                    MenuItem::new(format!("Resume with {}…", agent.name), move |window, cx| {
                                        picker.update(cx, |reader, cx| reader.resume_thread(&key, window, cx))
                                    })
                                });
                                new.chain([MenuItem::separator()]).chain(resume).collect()
                            },
                        )
                        .menu(self.thread_menu.clone()),
                    ),
                cx,
            ))
            .child(
                div()
                    .id("thread")
                    .key_context("Markdown")
                    .track_focus(&self.thread_focus)
                    .on_action(cx.listener(|reader, _: &ui::markdown::Copy, _, cx| {
                        // Select All takes the replies scrolled out of view too, which aren't laid out.
                        let text = if reader.selection.read(cx).is_all() {
                            ui::markdown::all_text(reader.thread.iter().flat_map(|msg| match msg {
                                Msg::Agent(blocks) => blocks.as_slice(),
                                Msg::User(_) => &[],
                            }).filter_map(|block| match block {
                                Block::Text(text) | Block::Thought(text) => Some(text.as_str()),
                                _ => None,
                            }))
                        } else {
                            reader.selection.read(cx).selected_text()
                        };
                        if text.is_empty() {
                            return cx.propagate();
                        }
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    }))
                    .on_action(cx.listener(|reader, _: &ui::markdown::SelectAll, _, cx| {
                        reader.selection.update(cx, |s, _| s.select_all());
                        cx.notify();
                    }))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|reader, event: &gpui::MouseDownEvent, window, cx| {
                        reader.thread_focus.focus(window, cx);
                        reader.selection.update(cx, |s, _| s.mouse_down(event.position, event.click_count));
                        cx.notify();
                    }))
                    .on_mouse_move(cx.listener(|reader, event: &gpui::MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(gpui::MouseButton::Left) && reader.selection.read(cx).selecting {
                            reader.selection.update(cx, |s, _| s.drag(event.position));
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(gpui::MouseButton::Left, cx.listener(|reader, event: &gpui::MouseUpEvent, _, cx| {
                        if let Some(url) = reader.selection.update(cx, |s, _| s.mouse_up(event.position)) {
                            cx.open_url(&url);
                        }
                        cx.notify();
                    }))
                    .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|reader, event: &gpui::MouseUpEvent, _, cx| {
                        reader.selection.update(cx, |s, _| {
                            s.mouse_up(event.position);
                        });
                    }))
                    .flex_1()
                    .min_h_0()
                    .child(thread),
            )
            .child(
                div().flex_none().p_3().child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.background)
                        .child(Input::new(&self.draft))
                        .child(div().flex().justify_end().child(action)),
                ),
            )
    }
}

fn render_msg(
    ix: usize,
    msg: &Msg,
    this: &Entity<Reader>,
    selection: &Entity<ui::markdown::Selection>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.global::<Palette>();
    let blocks = match msg {
        Msg::User(text) => {
            return div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui::relative(0.9))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(colors.secondary)
                        .text_sm()
                        .child(text.clone()),
                )
                .into_any_element();
        }
        Msg::Agent(blocks) => blocks,
    };
    let mut column = div().flex().flex_col().gap_2().min_w_0();
    if blocks.is_empty() {
        column = column.child(div().text_sm().text_color(colors.muted_foreground).child("Thinking…"));
    }
    for (n, block) in blocks.iter().enumerate() {
        let id = ix * 10_000 + n;
        column = match block {
            Block::Text(text) => {
                column.child(ui::markdown::markdown(("answer", id), text, selection, window, cx))
            }
            Block::Thought(text) => column.child(div().italic().opacity(0.7).child(ui::markdown::markdown(
                ("thought", id),
                text,
                selection,
                window,
                cx,
            ))),
            Block::Tool(tool) => column.child(tool_card(ix, n, tool, this, cx)),
            Block::Permission(ask) => column.child(permission(ix, n, ask, this)),
            Block::Plan(entries) => column.child(plan(entries, cx)),
        };
    }
    column.into_any_element()
}

/// The sessions an agent offers to resume, newest first.
fn session_picker(sessions: &[SessionInfo], this: &Entity<Reader>, cx: &App) -> AnyElement {
    let colors = cx.global::<Palette>();
    div()
        .id("sessions")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .children(sessions.iter().enumerate().map(|(ix, session)| {
            let (this, id) = (this.clone(), session.id.clone());
            let title = if session.title.is_empty() { format!("Session {}", session.id.chars().take(8).collect::<String>()) } else { session.title.clone() };
            let when = session.updated.get(..16).unwrap_or_default().replace('T', " ");
            div()
                .id(("session", ix))
                .flex()
                .flex_col()
                .p_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(colors.list_hover))
                .on_click(move |_, _, cx| this.update(cx, |reader, cx| reader.resume_session(&id, cx)))
                .child(div().text_sm().truncate().child(title))
                .child(div().text_xs().text_color(colors.muted_foreground).child(when))
        }))
        .into_any_element()
}

fn step_mark(status: Step, colors: &Palette) -> (&'static str, gpui::Hsla) {
    match status {
        Step::Waiting => ("○", colors.muted_foreground),
        Step::Working => ("◐", colors.primary),
        Step::Done => ("✓", colors.success),
        Step::Failed => ("✕", colors.danger),
    }
}

fn plan(entries: &[(String, Step)], cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    div().flex().flex_col().gap_1().text_sm().children(entries.iter().map(|(text, status)| {
        let (mark, tint) = step_mark(*status, colors);
        div().flex().gap_2().child(div().flex_none().text_color(tint).child(mark)).child(div().flex_1().min_w_0().child(text.clone()))
    }))
}

fn tool_card(ix: usize, n: usize, tool: &ToolBlock, this: &Entity<Reader>, cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    let (mark, tint) = step_mark(tool.status, colors);
    let toggle = this.clone();
    let mut card = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(colors.border)
        .bg(colors.background)
        .child(
            div()
                .id(("tool", ix * 10_000 + n))
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_sm()
                .on_click(move |_, _, cx| toggle.update(cx, |reader, cx| reader.toggle_tool(ix, n, cx)))
                .child(div().text_color(tint).child(mark))
                .child(div().flex_1().min_w_0().truncate().child(tool.title.clone())),
        );
    if tool.open {
        for text in [&tool.arguments, &tool.result] {
            if !text.is_empty() {
                card = card.child(
                    div()
                        .max_h(rems(16.))
                        .overflow_hidden()
                        .font_family(cx.global::<Palette>().mono_font_family.clone())
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(text.clone()),
                );
            }
        }
    }
    card
}

fn permission(ix: usize, n: usize, ask: &Ask, this: &Entity<Reader>) -> impl IntoElement {
    let row = div().flex().flex_wrap().gap_2();
    let row = match &ask.answered {
        Some(name) => row.child(format!("→ {name}")),
        None => row.children(ask.options.iter().enumerate().map(|(o, option)| {
            let this = this.clone();
            let allow = option.name.to_lowercase().contains("allow");
            let button = Button::new(("permit", ix * 1_000_000 + n * 100 + o))
                .label(option.name.clone())
                .small()
                .on_click(move |_, _, cx| this.update(cx, |reader, cx| reader.answer(ix, n, o, cx)));
            if allow { button.primary() } else { button }
        })),
    };
    div().flex().flex_col().gap_2().text_sm().child(format!("Permission: {}", ask.title)).child(row)
}

fn header(title: &str, detail: String, cx: &App) -> impl IntoElement {
    header_with(title, status_text(detail, cx), cx)
}

fn status_text(text: String, cx: &App) -> impl IntoElement {
    div().flex_none().truncate().text_xs().text_color(cx.global::<Palette>().muted_foreground).child(text)
}

fn header_with(title: &str, right: impl IntoElement, cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    div()
        .h_10()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .border_b_1()
        .border_color(colors.border)
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.foreground)
                .child(title.to_string()),
        )
        .child(right)
}

fn centered(text: String, cx: &App) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.global::<Palette>().muted_foreground)
        .child(text)
        .into_any_element()
}

impl Render for Reader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.painted {
            self.painted = true;
            trace::mark("first render");
            if trace::exit_after_first_frame() {
                cx.defer(|_| std::process::exit(0));
            }
        }
        let colors = cx.global::<Palette>();
        let (background, foreground, border, sidebar) = (colors.background, colors.foreground, colors.border, colors.sidebar);
        let (left, right) = (cx.entity(), cx.entity());
        let (left_open, right_open) = (self.left_open, self.right_open);
        let bar = div()
            .h_8()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .border_t_1()
            .border_color(border)
            .bg(sidebar)
            .child(
                Button::new("toggle-left")
                    .icon(if left_open { IconName::PanelLeftClose } else { IconName::PanelLeftOpen })
                    .ghost()
                    .small()
                    .tooltip("Toggle left sidebar (Ctrl+B)")
                    .on_click(move |_, _, cx| left.update(cx, |reader, cx| reader.toggle_left(cx))),
            )
            .child(
                Button::new("toggle-right")
                    .icon(if right_open { IconName::PanelRightClose } else { IconName::PanelRightOpen })
                    .ghost()
                    .small()
                    .tooltip("Toggle right sidebar (Ctrl+Shift+B)")
                    .on_click(move |_, _, cx| right.update(cx, |reader, cx| reader.toggle_right(cx))),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .relative()
            .track_focus(&self.focus)
            .on_action(cx.listener(|reader, _: &ShowFinder, window, cx| reader.show_finder(window, cx)))
            .on_action(cx.listener(|reader, _: &ToggleLeft, _, cx| reader.toggle_left(cx)))
            .on_action(cx.listener(|reader, _: &ToggleRight, _, cx| reader.toggle_right(cx)))
            .on_action(cx.listener(|reader, _: &ShowSource, _, cx| reader.set_mode(Mode::Source, cx)))
            .on_action(cx.listener(|reader, _: &ShowChanges, _, cx| reader.set_mode(Mode::Diff, cx)))
            .on_action(cx.listener(|reader, _: &ToggleSplit, _, cx| reader.set_split(!reader.split, cx)))
            .on_action(cx.listener(|reader, _: &NewThread, window, cx| {
                // Opening the right sidebar first: the menu hangs off its header.
                if !reader.right_open {
                    reader.right_open = true;
                    cx.notify();
                }
                reader.thread_menu.update(cx, |menu, cx| menu.toggle(window, cx));
            }))
            .child(
                div().flex_1().min_h_0().child(
                    sidebars(&self.sidebars)
                        .left(left_open, px(160.0)..px(520.0), self.explorer(cx))
                        .center(match self.mode {
                            Mode::Source => self.viewer(cx).into_any_element(),
                            Mode::Diff => self.diff_viewer(cx).into_any_element(),
                        })
                        .right(right_open, px(300.0)..px(760.0), self.agent_panel(cx)),
                ),
            )
            .child(bar)
            .when(self.finder_open, |this| this.child(self.finder(cx)))
    }
}

/// Relaunch ourselves in its own process group with output going to a log file,
/// so the shell gets its prompt back (like `code .`).
fn detach(root: &Path, command: &Option<String>, pr: &Option<pr::Pr>) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let log_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("po");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("po.log"))?;
    let mut child = Command::new(std::env::current_exe()?);
    child.arg("--foreground");
    if let Some(command) = command {
        child.arg("--agent").arg(command);
    }
    if let Some(pr) = pr {
        // The parent did the checkout so its errors reach the terminal; the child owns the cleanup.
        child
            .arg("--pr-worktree")
            .arg(&pr.worktree)
            .arg("--pr-base")
            .arg(&pr.base)
            .arg("--pr-number")
            .arg(pr.number.to_string())
            .arg("--pr-title")
            .arg(&pr.title);
        if let Some(source) = pr.source() {
            child.arg("--pr-source").arg(source);
        }
    }
    child
        .arg(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// The window's root: sets the base font, size and colors everything inside inherits.
struct Shell {
    child: Entity<Reader>,
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        window.set_rem_size(palette.font_size);
        div()
            .id("root")
            .relative()
            .size_full()
            .font_family(palette.font_family.clone())
            .bg(palette.background)
            .text_color(palette.foreground)
            .child(self.child.clone())
    }
}

fn main() {
    trace::init();
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut foreground = false;
    let mut command = std::env::var("PO_AGENT").ok();
    let mut spec = None;
    // Set only by a parent that already checked the PR out.
    let (mut worktree, mut base, mut number, mut title, mut source) = (None, None, 0, String::new(), None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--foreground" | "-f" => foreground = true,
            "--agent" => command = Some(args.next().expect("--agent needs a command")),
            "--pr" => spec = Some(args.next().expect("--pr needs a PR number or URL")),
            "--pr-worktree" => worktree = args.next().map(PathBuf::from),
            "--pr-base" => base = args.next(),
            "--pr-number" => number = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--pr-title" => title = args.next().unwrap_or_default(),
            "--pr-source" => source = args.next().map(PathBuf::from),
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let root = path.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let root = root.canonicalize().expect("project path");
    let pr = match (worktree, base, spec) {
        (Some(worktree), Some(base), _) => Some(pr::Pr::adopt(worktree, base, number, title, source)),
        (_, _, Some(spec)) => {
            eprintln!("po: checking out PR {spec}…");
            match pr::Pr::open(&root, &spec) {
                Ok(pr) => Some(pr),
                Err(err) => {
                    eprintln!("po: could not open PR {spec}: {err}");
                    std::process::exit(1);
                }
            }
        }
        _ => None,
    };
    if !foreground {
        match detach(&root, &command, &pr) {
            Ok(()) => {
                // The detached process owns the worktree now.
                std::mem::forget(pr);
                return;
            }
            Err(err) => eprintln!("po: could not detach ({err}), running in foreground"),
        }
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,po=info"))
        .format_timestamp_millis()
        .init();
    trace::mark("args parsed");
    let prefetch = Prefetch::start(pr.as_ref().map_or_else(|| root.clone(), |pr| pr.worktree.clone()));
    gpui_platform::application().run(move |cx: &mut App| {
        trace::mark("app running");
        viewer::init(cx);
        ui::init(cx);
        cx.bind_keys([
            KeyBinding::new("ctrl-b", ToggleLeft, None),
            KeyBinding::new("ctrl-shift-b", ToggleRight, None),
            KeyBinding::new("ctrl-1", ShowSource, None),
            KeyBinding::new("ctrl-2", ShowChanges, None),
            KeyBinding::new("ctrl-\\", ToggleSplit, None),
            KeyBinding::new("ctrl-n", NewThread, None),
            KeyBinding::new("ctrl-p", ShowFinder, None),
            KeyBinding::new("escape", CloseFinder, Some("Finder")),
            // The Input binds up/down itself and swallows them when single-line, so these name it.
            KeyBinding::new("up", FinderUp, Some("Finder > Input")),
            KeyBinding::new("down", FinderDown, Some("Finder > Input")),
        ]);
        let bounds = Bounds::centered(None, size(px(1360.0), px(840.0)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| {
                // Applied here rather than before the window opens: the first text lookup waits
                // for the system fonts, and until the window exists the GPU setup can run alongside.
                trace::mark("window created");
                theme::apply(cx);
                trace::mark("theme applied");
                let reader = cx.new(|cx| Reader::new(root, pr, command, prefetch, window, cx));
                cx.new(|_| Shell { child: reader })
            },
        )
        .expect("window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::{language, match_files, syntax};

    #[test]
    fn file_matches_rank_names_before_paths() {
        let files = vec![
            "src/none.txt".to_string(),
            "needle/lib/a.rs".to_string(),
            "needle.rs".to_string(),
            "deep/needlemod.rs".to_string(),
            "src/Needle-ish.rs".to_string(),
        ];
        // Name hits first (shorter names first), then path-only hits, each in walk order.
        assert_eq!(
            match_files(&files, "needle"),
            vec!["needle.rs", "deep/needlemod.rs", "src/Needle-ish.rs", "needle/lib/a.rs"]
        );
        assert!(match_files(&files, "needs").is_empty());
        // An empty query lists everything in walk order.
        assert_eq!(match_files(&files, ""), files);
    }

    #[test]
    fn elixir_files_highlight() {
        assert_eq!(language("lib/app.ex"), "Elixir");
        assert_eq!(language("lib/app.exs"), "Elixir");
        // `.ex` resolves through the viewer's alias table, `.exs` through `syntax`.
        assert_eq!(syntax("lib/app.ex"), "ex");
        assert_eq!(syntax("mix.exs"), "elixir");
        assert!(super::viewer::highlights("defmodule A do\nend\n", "ex"));
        assert!(super::viewer::highlights("defmodule A do\nend\n", &syntax("mix.exs")));
    }
}
