#![recursion_limit = "1024"]
mod acp;
mod agent;
mod alert;
mod config;
mod dex;
mod diff;
mod pr;
mod schemes;
mod theme;
mod trace;
mod ui;
mod update;
mod viewer;
mod workspace;
mod worktree;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use acp::{Event, PermissionOption, SessionInfo};
use agent::Client;
use dex::Queue;
use diff::{Change, Kind, Pair, Row};
use theme::Palette;
use ui::button::Button;
use ui::icon::{Icon, IconName, IconSize};
use ui::input::{Input, InputEvent, InputState};
use ui::list_item::ListItem;
use ui::menu::{MenuItem, MenuState, dropdown};
use ui::resizable::{SidebarsState, VSplitState, sidebars, vsplit};
use ui::tree::{TreeEvent, TreeItem, TreeState, tree};
use futures::StreamExt;
use serde_json::Value;

use gpui::{
    AnyElement, App, Pixels, TitlebarOptions, WindowControlArea, point, Bounds, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState, ParentElement,
    Render, SharedString, StyleRefinement, Styled, Subscription, Task, Window, WindowBounds, WindowOptions, div, list,
    prelude::*, px, rems, size, KeyBinding,
};

gpui::actions!(po, [ToggleLeft, ToggleRight, ShowSource, ShowChanges, ShowChat, SwapPanes, ShowHistory, ToggleSplit, ToggleRendered, NewThread, OpenProject, ShowFinder, CloseWindow, CloseFinder, FinderUp, FinderDown]);

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
        if entry.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
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

/// A message typed while the agent was working.
struct Queued {
    content: String,
    /// Where the daemon holds it; `None` when it waits here for the turn to end.
    kind: Option<Queue>,
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

/// A markdown file as the viewer shows it rendered: cut into pieces so that only those in view
/// are laid out, with a selection and focus of its own.
struct Doc {
    chunks: Rc<Vec<String>>,
    list: ListState,
    selection: Entity<ui::markdown::Selection>,
    focus: gpui::FocusHandle,
}

fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

/// Everything about one checkout: its files and git state, and what the explorer, viewer and
/// diff viewer are showing of them.
struct Project {
    root: PathBuf,
    /// What Changes mode diffs against: `HEAD`, or a PR's merge-base when reviewing.
    base: String,
    pr: Option<pr::Pr>,
    files: Vec<String>,
    changes: Vec<Change>,
    /// Change marks for the explorer, by path.
    marks: Rc<HashMap<String, char>>,
    /// The latest `git diff` for the picked file and `git status`, run off the UI thread;
    /// replacing one drops the stale run.
    diffing: Option<Task<()>>,
    listing: Option<Task<()>>,
    scanning: Option<Task<()>>,
    watching: Option<gpui::Task<()>>,
    changes_tree: Entity<TreeState>,
    collapsed: HashSet<SharedString>,
    picked: Option<String>,
    /// The picked file's diff, its side-by-side pairs and how many lines it adds and removes.
    rows: Rc<Vec<Row>>,
    pairs: Rc<Vec<Pair>>,
    counts: (usize, usize),
    /// The diff's rows as laid out, inline or split; only those in view are drawn.
    diff_list: ListState,
    current: Option<String>,
    stamp: Option<Stamp>,
    code: Option<Entity<viewer::CodeViewState>>,
    /// The current file rendered, when it is markdown.
    doc: Option<Doc>,
    notice: Option<String>,
    tree: Entity<TreeState>,
    expanded: HashSet<SharedString>,
    /// Set when this is a thread's own checkout, made from another project's repository.
    worktree: Option<worktree::Worktree>,
    /// Keep the explorer and changes list wired to this project.
    _subscriptions: Vec<Subscription>,
}

impl Project {
    /// What the project is called in the list.
    fn name(&self) -> String {
        match &self.pr {
            Some(pr) => format!("PR #{} {}", pr.number, pr.title),
            None => {
                let name = self.root.file_name().map_or_else(|| self.root.display().to_string(), |n| n.to_string_lossy().to_string());
                match &self.worktree {
                    Some(wt) => format!("{} ⎇ {}", wt.spec.repo.file_name().map_or(name, |n| n.to_string_lossy().to_string()), wt.spec.branch),
                    None => name,
                }
            }
        }
    }
}

/// One conversation with an agent: its messages, the process behind it and what is waiting to
/// be sent to it.
struct Thread {
    /// Names the thread to the events of its agent while it isn't the one on screen.
    id: u64,
    /// It finished a turn while another thread was on screen.
    unread: bool,
    /// The agent's session, once it has one; what a thread without a running agent is resumed from.
    session: Option<String>,
    /// A session to load as soon as the agent has listed its sessions, instead of showing them.
    restore: Option<String>,
    /// What the thread was about before it was loaded back, since it has no messages yet.
    saved_title: Option<String>,
    /// When the thread last did anything; an idle one in the background is put to sleep.
    touched: Instant,
    /// A first message waiting for the agent to be ready, already shown in the thread.
    pending: Option<String>,
    /// Start the thread in a git worktree of its own. Only asked before it has started.
    isolate: bool,
    messages: Vec<Msg>,
    busy: bool,
    /// Messages typed during a turn, shown above the input until they are taken.
    queue: Vec<Queued>,
    ready: bool,
    /// A resumed session's history is being replayed.
    loading: bool,
    /// Choosing a session to resume: the agent's list once it arrives.
    picking: bool,
    sessions: Option<Vec<SessionInfo>>,
    status: String,
    client: Option<Client>,
    agent: Option<String>,
    generation: u64,
    draft: Entity<InputState>,
    /// The messages as laid out; only those in view are drawn.
    list: ListState,
    /// The selection across the agent's replies.
    selection: Entity<ui::markdown::Selection>,
    focus: gpui::FocusHandle,
    /// Sends the draft on Enter; goes with the thread.
    _enter: Subscription,
}

impl Thread {
    fn fail(&mut self, text: String) {
        self.busy = false;
        self.messages.push(Msg::Agent(vec![Block::Text(format!("**Error:** {text}"))]));
        self.relist(self.messages.len() - 1);
    }

    /// Tells the thread's list that messages from `from` on were added or changed, so it lays
    /// them out again.
    fn relist(&mut self, from: usize) {
        let listed = self.list.item_count();
        let from = from.min(listed).min(self.messages.len());
        self.list.splice(from..listed, self.messages.len() - from);
    }

    fn blocks(&mut self) -> &mut Vec<Block> {
        if !matches!(self.messages.last(), Some(Msg::Agent(_))) {
            self.messages.push(Msg::Agent(vec![]));
            self.relist(self.messages.len() - 1);
        }
        match self.messages.last_mut() {
            Some(Msg::Agent(blocks)) => blocks,
            _ => unreachable!("an agent message was just pushed"),
        }
    }

    /// The message holding the tool call an update is for, if it updates one.
    fn tool_message(&self, update: &Value) -> Option<usize> {
        if update["sessionUpdate"].as_str() != Some("tool_call_update") {
            return None;
        }
        let id = update["toolCallId"].as_str().unwrap_or_default();
        self.messages.iter().rposition(|msg| match msg {
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
                match self.messages.last_mut() {
                    Some(Msg::User(body)) => body.push_str(text),
                    _ => {
                        self.messages.push(Msg::User(text.to_string()));
                        self.relist(self.messages.len() - 1);
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
                for msg in self.messages.iter_mut().rev() {
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
}

/// What a thread is up to, for the thread list.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Activity {
    Idle,
    /// Finished a turn nobody has looked at.
    Unread,
    Running,
    /// Waiting for an answer to a permission request.
    Asking,
}

impl Activity {
    /// The mark the thread and project lists draw, and its colour.
    fn mark(self, colors: &Palette) -> (&'static str, gpui::Hsla) {
        match self {
            Activity::Idle => ("○", colors.muted_foreground),
            Activity::Running => ("●", colors.primary),
            Activity::Asking => ("!", colors.warning),
            // Themes may lack a green (Rosé Pine's success is teal), so this has its own.
            Activity::Unread => ("●", gpui::hsla(0.37, 0.5, 0.5, 1.0)),
        }
    }
}

impl Thread {
    fn activity(&self) -> Activity {
        let asking = match self.messages.last() {
            Some(Msg::Agent(blocks)) => blocks.iter().any(|b| matches!(b, Block::Permission(ask) if ask.answered.is_none())),
            _ => false,
        };
        if asking {
            Activity::Asking
        } else if self.busy {
            Activity::Running
        } else if self.unread {
            Activity::Unread
        } else {
            Activity::Idle
        }
    }

    /// The first thing asked, or a placeholder while there is none.
    fn title(&self) -> String {
        let first = self.messages.iter().find_map(|msg| match msg {
            Msg::User(text) => text.lines().find(|line| !line.trim().is_empty()),
            Msg::Agent(_) => None,
        });
        match first {
            Some(line) => {
                let line = line.trim();
                let short: String = line.chars().take(28).collect();
                if short.len() < line.len() { format!("{short}…") } else { short }
            }
            None => self.saved_title.clone().unwrap_or_else(|| "New thread".into()),
        }
    }

    /// An agent was started for it and then stopped (or never started after a restart), so it
    /// can be resumed from its session.
    fn asleep(&self) -> bool {
        self.client.is_none() && self.agent.is_some() && self.session.is_some()
    }

    /// It is showing an agent's sessions to pick one from, and nothing has been picked.
    fn is_listing(&self) -> bool {
        self.agent.is_some() && self.picking && !self.loading && self.messages.is_empty() && self.session.is_none()
    }

    /// Whether stopping its agent would lose nothing: no turn, no question and nothing queued.
    fn restful(&self) -> bool {
        self.client.is_some() && self.session.is_some() && self.activity() != Activity::Asking && !self.busy && self.queue.is_empty()
    }

    /// Stops the agent, keeping what has been said so it can be shown and resumed.
    fn sleep(&mut self) {
        self.client = None;
        self.ready = false;
        self.loading = false;
        self.picking = false;
        // The events of the stopped agent are no longer this thread's.
        self.generation += 1;
        self.status = "asleep".into();
    }
}

/// A project and its threads, wherever they are kept.
struct View<'a> {
    id: u64,
    mode: Mode,
    project: &'a Project,
    /// The thread that is, or would be, on screen.
    shown: u64,
    threads: Vec<&'a Thread>,
}

/// A project that isn't on screen, with its threads and view settings. They are swapped with
/// the same fields of [`Reader`] when it is shown.
struct Slot {
    id: u64,
    mode: Mode,
    project: Project,
    thread: Thread,
    parked: Vec<Thread>,
}

struct Reader {
    mode: Mode,
    /// The project on screen; its methods act on this one.
    project: Project,
    project_id: u64,
    /// The other projects, with their threads still running.
    slots: Vec<Slot>,
    next_project: u64,
    /// The thread on screen in this project; its methods act on this one.
    thread: Thread,
    /// This project's other threads, still running; swapped in with [`Self::with_thread`] to
    /// handle their events.
    parked: Vec<Thread>,
    next_thread: u64,
    /// The directory `po` was started in: where PRs are checked out from and where Close returns to.
    home: PathBuf,
    pr_input: Entity<InputState>,
    /// Progress or failure of the last attempt to open a PR.
    pr_status: Option<String>,
    opening: bool,
    /// The last thing that happened to a worktree (made, merged, closed), or why it failed.
    worktree_status: Option<String>,
    /// A merge or discard is running; asking for either again waits.
    finishing: bool,
    /// Discard was asked for once and waits for a second click, since it can't be undone.
    confirm_discard: bool,
    /// The Ctrl+P finder, when open: its query field, the matching files and which is picked.
    finder: Option<Entity<InputState>>,
    finder_matches: Rc<Vec<String>>,
    finder_selected: usize,
    finder_list: ListState,
    finder_open: bool,
    /// Keeps the finder input's event subscription alive for the reader's lifetime.
    finder_events: Option<Subscription>,
    painted: bool,
    agents: Vec<config::Agent>,
    /// Tell the desktop when a thread finishes or asks while it isn't being looked at.
    notifications: bool,
    /// How long a thread in the background can sit idle before its agent is stopped.
    idle_after: Option<Duration>,
    /// Agents that may run at once; beyond it the longest idle in the background are stopped.
    max_live: usize,
    /// Widths of the left side and the viewer pane.
    sidebars: Entity<SidebarsState>,
    left_open: bool,
    /// The boundary between the projects and the explorer on the left.
    left_split: Entity<VSplitState>,
    /// The viewer pane (a file or a diff) on the right is shown.
    side_open: bool,
    /// The viewer is in the middle and the conversation on the right, instead of the reverse.
    /// Only matters while the viewer is open.
    swapped: bool,
    /// The conversation pane is shown while it sits on the right, rather than collapsed. Only
    /// matters while it is there (`side_open` and `swapped`); in the middle it is always shown.
    agent_open: bool,
    /// The agent picker in the input box.
    agent_menu: Entity<MenuState>,
    /// The agent new threads start with: the one last chosen.
    chosen_agent: Option<String>,
    /// Whether diffs show old and new side by side instead of inline.
    split: bool,
    /// Markdown files show rendered rather than as source.
    rendered: bool,
    /// Keeps the shortcuts routed to the reader when no input is focused.
    focus: gpui::FocusHandle,
    /// The workspace is about to be saved, so asking again changes nothing.
    save_pending: bool,
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
        let left_split = cx.new(|_| VSplitState::new());
        let sidebars = cx.new(|_| SidebarsState::new(px(280.0), px(560.0)));
        let subscriptions = vec![
            // Switching modes unmounts whatever was focused (the explorer, the viewer), which
            // leaves nothing focused and the shortcuts unrouted, so take focus back.
            cx.on_focus_lost(window, |reader, window, cx| reader.focus.focus(window, cx)),
            // So are the column widths.
            cx.observe(&sidebars, |reader, sidebars, cx| {
                if !sidebars.read(cx).dragging() {
                    reader.request_save(cx);
                }
            }),
            // Where the split was dragged to is kept, once the drag is over.
            cx.observe(&left_split, |reader, split, cx| {
                if !split.read(cx).dragging() {
                    reader.request_save(cx);
                }
            }),
            cx.subscribe_in(&pr_input, window, |reader, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, alt: false }) {
                    reader.open_pr(window, cx);
                }
            }),
        ];
        let enter = Self::send_on_enter(&draft, window, cx);
        let settings = config::load();
        let mut agents = config::agents(&settings);
        let idle_minutes = settings["idle_minutes"].as_u64().unwrap_or(15);
        if let Some(command) = command.as_ref().filter(|c| !agents.iter().any(|agent| agent.id == **c)) {
            agents.push(config::Agent { id: command.clone(), name: "Custom".into(), command: command.clone(), daemon: None });
        }
        let project = Self::build_project(root, pr, files, window, cx);
        let thread = Thread {
            id: 0,
            unread: false,
            session: None,
            restore: None,
            saved_title: None,
            touched: Instant::now(),
            pending: None,
            isolate: false,
            _enter: enter,
            messages: vec![],
            busy: false,
            queue: vec![],
            ready: false,
            loading: false,
            picking: false,
            sessions: None,
            status: String::new(),
            client: None,
            agent: None,
            generation: 0,
            draft,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            selection: cx.new(|_| ui::markdown::Selection::default()),
            focus: cx.focus_handle(),
        };
        let mut reader = Self {
            mode: Mode::Source,
            project,
            project_id: 0,
            slots: vec![],
            next_project: 1,
            thread,
            parked: vec![],
            next_thread: 1,
            home,
            pr_input,
            pr_status: None,
            opening: false,
            worktree_status: None,
            finishing: false,
            confirm_discard: false,
            finder: None,
            finder_matches: Rc::new(vec![]),
            finder_selected: 0,
            finder_list: ListState::new(0, ListAlignment::Top, px(400.)),
            finder_open: false,
            finder_events: None,
            painted: false,
            agents,
            notifications: settings["notifications"].as_bool().unwrap_or(true),
            idle_after: (idle_minutes > 0).then(|| Duration::from_secs(idle_minutes * 60)),
            max_live: settings["max_live_agents"].as_u64().unwrap_or(8).max(1) as usize,
            sidebars,
            left_open: true,
            left_split,
            side_open: true,
            swapped: false,
            agent_open: true,
            agent_menu: cx.new(MenuState::new),
            chosen_agent: command.clone(),
            split: false,
            rendered: true,
            focus: cx.focus_handle(),
            save_pending: false,
            _subscriptions: subscriptions,
        };
        reader.focus.focus(window, cx);
        trace::mark("reader built");
        reader.refresh_tree(cx);
        trace::mark("tree");
        let resumed = workspace::Workspace::load(&reader.home).is_some_and(|saved| reader.restore(saved, window, cx));
        trace::mark("restore");
        if resumed {
            reader.wake_thread(window, cx);
        } else if let Some(command) = command {
            reader.connect(&command, false, window, cx);
            trace::mark("connect");
        }
        reader.watch(watch, window, cx);
        trace::mark("watch");
        // Agents nobody is talking to are stopped now and then; their threads wake on demand.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                if this.update(cx, |reader, cx| reader.sleep_idle(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        reader
    }

    /// A project over `root`, showing `files` in its explorer until the next walk.
    fn build_project(root: PathBuf, pr: Option<pr::Pr>, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Project {
        let tree = cx.new(TreeState::new);
        let changes_tree = cx.new(TreeState::new);
        // Only the project on screen has its trees touched, so these act on `reader.project`;
        // the check keeps a stray event from another project's tree off it.
        let subscriptions = vec![
            cx.subscribe(&tree, |reader, tree, event: &TreeEvent, _| {
                if reader.project.tree != tree {
                    return;
                }
                match event {
                    TreeEvent::Expanded(id) => {
                        reader.project.expanded.insert(id.clone());
                    }
                    TreeEvent::Collapsed(id) => {
                        reader.project.expanded.remove(id);
                    }
                }
            }),
            cx.subscribe(&changes_tree, |reader, tree, event: &TreeEvent, _| {
                if reader.project.changes_tree != tree {
                    return;
                }
                match event {
                    TreeEvent::Expanded(id) => {
                        reader.project.collapsed.remove(id);
                    }
                    TreeEvent::Collapsed(id) => {
                        reader.project.collapsed.insert(id.clone());
                    }
                }
            }),
            // Moving the selection with the keyboard opens the file too, like a click.
            cx.observe_in(&tree, window, |reader, tree, window, cx| {
                if reader.project.tree != tree {
                    return;
                }
                let path = tree.read(cx).selected_entry().filter(|e| !e.folder).map(|e| e.id.to_string());
                if let Some(path) = path.filter(|p| reader.mode == Mode::Source && reader.project.current.as_deref() != Some(p)) {
                    reader.open(&path, window, cx);
                }
            }),
            cx.observe(&changes_tree, |reader, tree, cx| {
                if reader.project.changes_tree != tree {
                    return;
                }
                let path = tree.read(cx).selected_entry().filter(|e| !e.folder).map(|e| e.id.to_string());
                if let Some(path) = path.filter(|p| reader.mode == Mode::Diff && reader.project.picked.as_deref() != Some(p)) {
                    reader.pick(&path, cx);
                }
            }),
        ];
        Project {
            root,
            base: pr.as_ref().map_or_else(|| "HEAD".to_string(), |pr| pr.base.clone()),
            pr,
            files,
            changes: vec![],
            marks: Rc::default(),
            diffing: None,
            listing: None,
            scanning: None,
            watching: None,
            changes_tree,
            collapsed: HashSet::new(),
            picked: None,
            rows: Rc::new(vec![]),
            pairs: Rc::new(vec![]),
            counts: (0, 0),
            diff_list: ListState::new(0, ListAlignment::Top, px(400.)),
            current: None,
            stamp: None,
            code: None,
            doc: None,
            notice: None,
            tree,
            expanded: HashSet::new(),
            worktree: None,
            _subscriptions: subscriptions,
        }
    }

    /// Rebuilds the explorer from `files`, keeping folders open and the current file selected.
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let items = explorer_items(&self.project.files, &self.project.expanded);
        let current = self.project.current.clone().map(SharedString::from);
        self.project.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            if let Some(ix) = current.and_then(|id| tree.index_of(&id)) {
                tree.set_selected_index(Some(ix), cx);
            }
        });
    }

    /// Takes a fresh list of changed files. In diff mode it picks a file if none is, and
    /// reloads the picked file's diff when `rediff` says it may have changed.
    fn apply_changes(&mut self, changes: Vec<Change>, rebuild: bool, rediff: bool, cx: &mut Context<Self>) {
        let listed = rebuild || self.project.changes != changes;
        self.project.changes = changes;
        if listed {
            self.project.marks = Rc::new(self.project.changes.iter().map(|c| (c.path.clone(), c.status)).collect());
        }
        if self.mode != Mode::Diff {
            return;
        }
        if listed {
            let paths: Vec<String> = self.project.changes.iter().map(|c| c.path.clone()).collect();
            let mut expanded = folders(&paths);
            expanded.retain(|id| !self.project.collapsed.contains(id));
            let items = explorer_items(&paths, &expanded);
            self.project.changes_tree.update(cx, |tree, cx| tree.set_items(items, cx));
        }
        let keep = self.project.picked.clone().filter(|path| self.project.changes.iter().any(|c| &c.path == path));
        match keep {
            Some(path) if rediff || listed => self.pick(&path, cx),
            Some(_) => {}
            None => match self.project.changes.first().map(|c| c.path.clone()) {
                Some(path) => self.pick(&path, cx),
                None => {
                    self.project.picked = None;
                    self.project.diffing = None;
                    self.set_rows(vec![]);
                }
            },
        }
        cx.notify();
    }

    /// Lists the changed files with `git` off the UI thread, then applies them as
    /// [`Self::apply_changes`] does and picks `then`, if given.
    fn refresh_changes(&mut self, rebuild: bool, then: Option<String>, cx: &mut Context<Self>) {
        let (root, base) = (self.project.root.clone(), self.project.base.clone());
        self.project.listing = Some(cx.spawn(async move |this, cx| {
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
        let root = self.project.root.clone();
        self.project.scanning = Some(cx.spawn(async move |this, cx| {
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
        if self.project.files == files {
            return;
        }
        self.project.files = files;
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
        self.project.counts = rows.iter().fold((0, 0), |(a, r), row| match row.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            _ => (a, r),
        });
        self.project.pairs = Rc::new(diff::split(&rows));
        self.project.rows = Rc::new(rows);
        self.relist_diff();
    }

    fn relist_diff(&mut self) {
        self.project.diff_list.reset(if self.split { self.project.pairs.len() } else { self.project.rows.len() });
    }

    /// Shows `path`'s diff in the viewer, once `git` has produced it off the UI thread.
    fn pick(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(change) = self.project.changes.iter().find(|c| c.path == path).cloned() else { return };
        if self.project.picked.as_deref() != Some(path) {
            self.project.picked = Some(path.to_string());
            // The last file's diff isn't shown under this one's name while this one loads.
            self.set_rows(vec![]);
            let id = SharedString::from(path.to_string());
            self.project.changes_tree.update(cx, |tree, cx| {
                if let Some(ix) = tree.index_of(&id) {
                    tree.set_selected_index(Some(ix), cx);
                }
            });
        }
        let (root, base, path) = (self.project.root.clone(), self.project.base.clone(), path.to_string());
        self.project.diffing = Some(cx.spawn(async move |this, cx| {
            let rows = cx.background_executor().spawn(async move { diff::file_diff(&root, &base, &change) }).await;
            this.update(cx, |reader, cx| {
                if reader.project.picked.as_deref() == Some(&path) {
                    reader.set_rows(rows);
                    reader.project.diffing = None;
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
        // Reviewed from the checkout on screen, or from where `po` started if that is a PR.
        let repo = if self.project.pr.is_none() { self.project.root.clone() } else { self.home.clone() };
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { pr::Pr::open(&repo, &spec) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.opening = false;
                        match result {
                            Ok(pr) => {
                                reader.pr_status = None;
                                reader.pr_input.update(cx, |input, cx| input.set_value("", window, cx));
                                let root = pr.worktree.clone();
                                reader.add_project(root, Some(pr), window, cx);
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

    /// Closes the PR review, going back to another project, or to the directory `po` started in
    /// if this was the only one.
    fn close_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.project_id;
        if self.slots.is_empty() {
            let home = self.home.clone();
            self.add_project(home, None, window, cx);
        }
        self.close_project(id, window, cx);
    }

    /// Swaps the project on screen, with its threads and mode, for `slots[at]`.
    fn swap_slot(&mut self, at: usize) {
        let slot = &mut self.slots[at];
        std::mem::swap(&mut self.project_id, &mut slot.id);
        std::mem::swap(&mut self.mode, &mut slot.mode);
        std::mem::swap(&mut self.project, &mut slot.project);
        std::mem::swap(&mut self.thread, &mut slot.thread);
        std::mem::swap(&mut self.parked, &mut slot.parked);
    }

    /// Stops what only the project on screen needs (its watcher, `git` runs and the finder), so
    /// that nothing keeps updating it once another is shown. Its threads are not touched.
    fn suspend_project(&mut self) {
        let project = &mut self.project;
        project.watching = None;
        project.listing = None;
        project.scanning = None;
        project.diffing = None;
        self.finder_open = false;
    }

    /// Brings a project that was swapped in up to date with the disk, and starts watching it.
    fn resume_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_tree(cx);
        self.rescan(cx);
        self.watch(None, window, cx);
        if self.mode == Mode::Diff {
            self.apply_changes(self.project.changes.clone(), true, false, cx);
            self.refresh_changes(true, None, cx);
        }
        if let Some(path) = self.project.current.clone() {
            self.open(&path, window, cx);
        }
        self.thread.unread = false;
        self.pr_status = None;
        self.worktree_status = None;
        self.confirm_discard = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Shows project `id`; its threads keep going wherever they were.
    fn show_project(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_project_with(id, None, window, cx);
    }

    /// [`Self::show_project`], with thread `thread` of it on screen if it has one.
    fn show_project_with(&mut self, id: u64, thread: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.slots.iter().any(|slot| slot.id == id) {
            return;
        }
        // Leaving a session listing unused drops it.
        if self.thread.is_listing() {
            self.close_thread(self.thread.id, window, cx);
        }
        let Some(at) = self.slots.iter().position(|slot| slot.id == id) else { return };
        self.suspend_project();
        self.swap_slot(at);
        if let Some(parked) = thread.and_then(|id| self.parked.iter().position(|t| t.id == id)) {
            std::mem::swap(&mut self.thread, &mut self.parked[parked]);
        }
        self.resume_project(window, cx);
        self.wake_thread(window, cx);
        self.request_save(cx);
    }

    /// Adds a project over `root` (a PR's worktree when `pr` is given) and shows it. If it is
    /// already open, that one is shown. An agent starts in it, the same one as in the project
    /// it was opened from.
    fn add_project(&mut self, root: PathBuf, pr: Option<pr::Pr>, window: &mut Window, cx: &mut Context<Self>) {
        if pr.is_none() {
            if self.project.pr.is_none() && self.project.root == root {
                return;
            }
            if let Some(id) = self.slots.iter().find(|slot| slot.project.pr.is_none() && slot.project.root == root).map(|slot| slot.id) {
                return self.show_project(id, window, cx);
            }
        }
        let agent = self.thread.agent.clone();
        let review = pr.is_some();
        let project = Self::build_project(root, pr, vec![], window, cx);
        self.open_project(project, if review { Mode::Diff } else { Mode::Source }, agent, window, cx);
    }

    /// Shows `project` in `mode`, with a thread of its own that runs `agent`, if any. The
    /// project on screen goes on running in the background.
    fn open_project(&mut self, project: Project, mode: Mode, agent: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.blank_thread(window, cx);
        self.suspend_project();
        let id = self.next_project;
        self.next_project += 1;
        // The project on screen goes into a slot, and the new one takes its fields.
        self.slots.push(Slot { id, mode, project, thread, parked: vec![] });
        let at = self.slots.len() - 1;
        self.swap_slot(at);
        self.resume_project(window, cx);
        if let Some(agent) = agent {
            self.connect(&agent, false, window, cx);
        }
        self.request_save(cx);
    }

    /// Starts a thread with `key`'s agent in a git worktree of its own, shown as a project:
    /// a branch whose changes can be reviewed here, then merged or discarded.
    /// With `first`, the message to start it with and the blank thread it was typed in, which
    /// goes away once the worktree is up.
    fn new_worktree_thread(&mut self, key: &str, first: Option<(String, (u64, u64))>, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        // A worktree is made from the repository, not from another worktree.
        let from = match &self.project.worktree {
            Some(wt) => wt.spec.repo.clone(),
            None => self.project.root.clone(),
        };
        self.finishing = true;
        self.worktree_status = Some("Creating worktree…".into());
        cx.notify();
        let (key, handle) = (key.to_string(), window.window_handle());
        cx.spawn(async move |this, cx| {
            let made = cx.background_executor().spawn(async move { worktree::Worktree::create(&from) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.finishing = false;
                        match made {
                            Ok(wt) => {
                                reader.worktree_status = None;
                                reader.open_worktree(wt, key, first, window, cx);
                            }
                            Err(error) => {
                                log::warn!("worktree: {error}");
                                reader.worktree_status = Some(error);
                                if let Some((_, (_, tid))) = first {
                                    reader.with_thread(tid, |reader, _| reader.thread.status.clear());
                                }
                                cx.notify();
                            }
                        }
                    })
                })
                .ok();
        })
        .detach();
    }

    fn open_worktree(&mut self, wt: worktree::Worktree, agent: String, first: Option<(String, (u64, u64))>, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("worktree: {} for {agent}", wt.spec.branch);
        let mut project = Self::build_project(wt.spec.path.clone(), None, vec![], window, cx);
        project.base = wt.spec.base.clone();
        project.worktree = Some(wt);
        self.confirm_discard = false;
        self.open_project(project, Mode::Diff, Some(agent), window, cx);
        if let Some((prompt, (pid, tid))) = first {
            self.begin_turn(&prompt);
            self.thread.pending = Some(prompt);
            // The thread it was typed in stays only if something has happened to it since.
            let blank = self.views().iter().any(|v| v.id == pid && v.threads.iter().any(|t| t.id == tid && t.agent.is_none()));
            if blank {
                self.close_any_thread(pid, tid, window, cx);
            }
        }
    }

    /// Merges the worktree on screen into the checkout it came from, or throws it away.
    fn finish_worktree(&mut self, merge: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.project.worktree.as_ref().map(|wt| wt.spec.clone()) else { return };
        if self.finishing {
            return;
        }
        if !merge && !self.confirm_discard {
            self.confirm_discard = true;
            cx.notify();
            return;
        }
        self.confirm_discard = false;
        self.finishing = true;
        self.worktree_status = Some(if merge { "Merging…" } else { "Discarding…" }.into());
        let (id, handle) = (self.project_id, window.window_handle());
        let message = format!("po: {}", self.thread.title());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if merge {
                        worktree::Worktree::merge(&spec, &message).map(Some)
                    } else {
                        worktree::Worktree::discard(&spec);
                        Ok(None)
                    }
                })
                .await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.finishing = false;
                        match result {
                            Ok(into) => {
                                reader.close_worktree_project(id, window, cx);
                                reader.worktree_status = Some(match into {
                                    Some(into) => format!("Merged into {into}"),
                                    None => "Discarded".into(),
                                });
                            }
                            Err(error) => {
                                log::warn!("worktree: {error}");
                                reader.worktree_status = Some(error);
                            }
                        }
                        cx.notify();
                    })
                })
                .ok();
        })
        .detach();
    }

    /// Project `id`, on screen or not.
    fn project_by_id(&mut self, id: u64) -> Option<&mut Project> {
        if self.project_id == id {
            return Some(&mut self.project);
        }
        self.slots.iter_mut().find(|slot| slot.id == id).map(|slot| &mut slot.project)
    }

    /// Closes the project of a worktree that has been merged or discarded, going back to the
    /// project it came from if that is open.
    fn close_worktree_project(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let origin = self.project_by_id(id).and_then(|project| project.worktree.as_ref()).map(|wt| wt.spec.repo.clone());
        if let Some(wt) = self.project_by_id(id).and_then(|project| project.worktree.as_mut()) {
            wt.finished();
        }
        let parent = origin.and_then(|repo| {
            std::iter::once((self.project_id, &self.project)).chain(self.slots.iter().map(|s| (s.id, &s.project)))
                .find(|(pid, project)| *pid != id && project.worktree.is_none() && project.pr.is_none() && project.root == repo)
                .map(|(pid, _)| pid)
        });
        if let Some(parent) = parent {
            self.show_project(parent, window, cx);
        }
        self.close_project(id, window, cx);
    }

    /// Asks for a folder and opens it as a project.
    fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open project".into()),
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(root) = paths.into_iter().next().and_then(|path| path.canonicalize().ok()) else { return };
            handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.add_project(root, None, window, cx))).ok();
        })
        .detach();
    }

    /// Closes project `id`: its agents stop, and a PR's worktree is removed. Never the last one.
    fn close_project(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.slots.is_empty() {
            return;
        }
        if self.project_id == id {
            // Show another first; the closed project is then the one in the slot.
            let Some(other) = self.slots.last().map(|slot| slot.id) else { return };
            self.show_project(other, window, cx);
        }
        let Some(at) = self.slots.iter().position(|slot| slot.id == id) else { return };
        // Its agents stop with the slot; a worktree is then closed off the UI thread without
        // losing what was done in it, which the user is told.
        let mut slot = self.slots.remove(at);
        let worktree = slot.project.worktree.take();
        drop(slot);
        if let Some(mut wt) = worktree {
            cx.spawn(async move |this, cx| {
                let kept = cx.background_executor().spawn(async move { wt.retire() }).await;
                if let Some(branch) = kept {
                    this.update(cx, |reader, cx| {
                        reader.worktree_status = Some(format!("Closed. Its work is kept on branch {branch}"));
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }
        self.request_save(cx);
        cx.notify();
    }

    fn toggle_left(&mut self, cx: &mut Context<Self>) {
        self.left_open = !self.left_open;
        cx.notify();
    }

    /// Browse (files) or Review (changes) in the viewer pane. If the pane was closed it opens wide,
    /// since looking at code or a diff wants room; if it is open its width is left as it was.
    fn show_viewer(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.set_mode(mode, cx);
        self.open_viewer_wide(cx);
    }

    /// Opens the viewer if it is closed, wide since looking at code or a diff wants room; if it is
    /// open its width is left as it was.
    fn open_viewer_wide(&mut self, cx: &mut Context<Self>) {
        if !self.side_open {
            self.side_open = true;
            // It may have been opened from the middle, where the conversation always shows.
            self.agent_open = true;
            self.sidebars.update(cx, |sidebars, _| sidebars.set_right_share(0.55));
            self.request_save(cx);
        }
        cx.notify();
    }

    /// Chat: the viewer pane closed, so the conversation has the middle to itself.
    fn show_chat(&mut self, cx: &mut Context<Self>) {
        if self.side_open {
            self.side_open = false;
            self.request_save(cx);
        }
        cx.notify();
    }

    /// Shows the viewer pane, which a file was just opened for, at the width it last had.
    fn reveal_side(&mut self, cx: &mut Context<Self>) {
        if !self.side_open {
            self.side_open = true;
            // It may have been opened from the middle, where the conversation always shows.
            self.agent_open = true;
            self.request_save(cx);
            cx.notify();
        }
    }

    /// Shows `path` where it is most useful: its diff if it has changes, the file otherwise.
    fn reveal(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let full = Path::new(path);
        let relative = full.strip_prefix(&self.project.root).unwrap_or(full).to_string_lossy().replace('\\', "/");
        if self.project.changes.iter().any(|change| change.path == relative) {
            self.set_mode(Mode::Diff, cx);
            self.pick(&relative, cx);
        } else if self.project.root.join(&relative).is_file() {
            self.set_mode(Mode::Source, cx);
            self.open(&relative, window, cx);
        }
        self.reveal_side(cx);
    }

    fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if self.split != split {
            self.split = split;
            self.relist_diff();
        }
        cx.notify();
    }

    /// Puts the viewer in the middle and the conversation on the right, or back. The
    /// conversation always shows after a swap: it lands in the middle, where it can't be
    /// collapsed, and keeps its place when it goes back to the right.
    fn swap_panes(&mut self, cx: &mut Context<Self>) {
        self.swapped = !self.swapped;
        self.agent_open = true;
        self.request_save(cx);
        cx.notify();
    }

    fn toggle_right(&mut self, cx: &mut Context<Self>) {
        // The right toggle works on the pane that sits on the right: the conversation while the
        // viewer is open and swapped, the viewer otherwise.
        if self.side_open && self.swapped {
            self.agent_open = !self.agent_open;
            self.request_save(cx);
            cx.notify();
        } else if self.side_open {
            self.show_chat(cx);
        } else {
            self.open_viewer_wide(cx);
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Diff {
            // Show the last known changes at once; `git` confirms them in the background.
            self.apply_changes(self.project.changes.clone(), true, false, cx);
            self.refresh_changes(true, None, cx);
        }
        cx.notify();
    }

    /// Watches the workspace: refreshes the explorer when files come or go, and
    /// reloads the open file when it changes on disk.
    fn watch(&mut self, prepared: Option<futures::channel::oneshot::Receiver<Option<Watch>>>, window: &Window, cx: &mut Context<Self>) {
        use notify::Watcher;
        let root = self.project.root.clone();
        let handle = window.window_handle();
        // Kept so the watcher stops when the root changes.
        self.project.watching = Some(cx.spawn(async move |this, cx| {
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
                let Ok(current) = this.read_with(cx, |reader, _| reader.project.current.clone()) else { break };
                let scan_root = root.clone();
                let Ok(base) = this.read_with(cx, |reader, _| reader.project.base.clone()) else { break };
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
                        let rediff = reader.project.picked.as_ref().is_some_and(|path| touched.contains(path));
                        reader.apply_changes(changes, false, rediff, cx);
                        if let (Some(path), Some(seen)) = (reader.project.current.clone(), seen)
                            && reader.project.stamp != Some(seen) {
                                log::info!("reloading {path}");
                                reader.open(&path, window, cx);
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

    /// Starts `key`'s agent in the thread on screen, with a fresh conversation, or with the
    /// agent's session list to resume from when `resume`.
    fn connect(&mut self, key: &str, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.generation += 1;
        let generation = self.thread.generation;
        let id = self.thread.id;
        self.thread.client = None;
        self.thread.agent = Some(key.to_string());
        self.thread.messages.clear();
        self.thread.relist(0);
        self.thread.selection.update(cx, |s, _| s.clear());
        self.thread.busy = false;
        self.thread.queue.clear();
        self.thread.ready = false;
        self.thread.loading = false;
        self.thread.picking = resume;
        self.thread.sessions = None;
        let agent = self.agents.iter().find(|agent| agent.id == key).cloned().unwrap_or_else(|| config::Agent {
            id: key.to_string(),
            name: key.to_string(),
            command: key.to_string(),
            daemon: None,
        });
        self.thread.status = if resume { "listing sessions…" } else { "starting…" }.into();
        match Client::spawn(&agent, &self.project.root, resume) {
            Ok((client, mut events)) => {
                self.thread.client = Some(client);
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    // An agent that never answers the handshake would leave the thread on "starting…".
                    let watchdog = this.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(20)).await;
                        watchdog
                            .update(cx, |reader, cx| {
                                reader.with_thread(id, |reader, _| {
                                    let thread = &mut reader.thread;
                                    if thread.generation == generation && !thread.ready && thread.client.is_some() {
                                        thread.status = "not responding".into();
                                        thread.fail("The agent did not answer the handshake. Check that its command speaks ACP.".into());
                                    }
                                });
                                cx.notify();
                            })
                            .ok();
                    })
                    .detach();
                    while let Some(event) = events.next().await {
                        let done = handle.update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                // The thread may have been closed, or its agent replaced, since.
                                reader
                                    .with_thread(id, |reader, visible| {
                                        let current = reader.thread.generation == generation;
                                        if current {
                                            reader.on_event(event, visible, window, cx);
                                        }
                                        current
                                    })
                                    .unwrap_or(false)
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
                self.thread.status = "not running".into();
                self.thread.fail(format!("{error:#}"));
            }
        }
        self.enforce_cap();
        cx.notify();
    }

    /// Opens a thread that has no agent yet, ready for a message. The thread on screen keeps
    /// running in the background; if it is itself still blank it is used instead. The agent
    /// starts when the first message is sent.
    fn new_blank_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.agent.is_some() {
            self.add_thread(window, cx);
        }
        self.focus_draft(window, cx);
        cx.notify();
    }

    fn focus_draft(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.draft.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The agent a new thread would start with.
    fn chosen(&self) -> Option<&config::Agent> {
        self.chosen_agent.as_ref().and_then(|key| self.agents.iter().find(|agent| agent.id == *key)).or(self.agents.first())
    }

    /// Lists an agent's sessions in a new thread, to resume one: those of the agent the thread on
    /// screen is running, or of the chosen one if it has not started.
    fn resume_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else { return };
        log::info!("agent: resuming with {key}");
        self.add_thread(window, cx);
        self.connect(&key, true, window, cx);
    }

    /// Starts the blank thread on screen: the chosen agent runs `prompt` as its first message,
    /// which is shown at once and sent when the agent is ready. In a worktree of its own if
    /// that was asked for.
    fn start_thread(&mut self, prompt: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.chosen().map(|agent| agent.id.clone()) else {
            self.thread.fail("No agents are configured.".into());
            cx.notify();
            return;
        };
        if self.finishing {
            return;
        }
        self.chosen_agent = Some(key.clone());
        if self.thread.isolate && worktree::in_repo(&self.project.root) {
            // The draft stays until the worktree exists, so a failure loses nothing.
            let blank = (self.project_id, self.thread.id);
            self.thread.status = "creating worktree…".into();
            self.new_worktree_thread(&key, Some((prompt, blank)), window, cx);
            return;
        }
        self.thread.draft.update(cx, |input, cx| input.set_value("", window, cx));
        log::info!("agent: new thread with {key}");
        self.connect(&key, false, window, cx);
        self.begin_turn(&prompt);
        self.thread.pending = Some(prompt);
        self.request_save(cx);
        cx.notify();
    }

    /// Puts a first message back in the draft after the agent failed before it could be sent.
    fn return_pending(&mut self) {
        let Some(prompt) = self.thread.pending.take() else { return };
        let shown = matches!(self.thread.messages.as_slice(), [.., Msg::User(text), Msg::Agent(blocks)] if *text == prompt && blocks.is_empty());
        if shown {
            let keep = self.thread.messages.len() - 2;
            self.thread.messages.truncate(keep);
            self.thread.relist(keep);
        }
        self.thread.queue.insert(0, Queued { content: prompt, kind: None });
    }

    /// Sends the draft on Enter, while its thread is the one on screen.
    fn send_on_enter(draft: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(draft, window, |reader, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, alt } = event
                && reader.thread.draft == *input
            {
                reader.send(*alt, window, cx);
            }
        })
    }

    /// Makes a thread with no agent yet.
    fn blank_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Thread {
        let draft = cx.new(|cx| {
            InputState::new(window, cx).auto_grow(1, 6).submit_on_enter(true).placeholder("Message the agent")
        });
        let id = self.next_thread;
        self.next_thread += 1;
        Thread {
            id,
            unread: false,
            session: None,
            restore: None,
            saved_title: None,
            touched: Instant::now(),
            pending: None,
            isolate: false,
            _enter: Self::send_on_enter(&draft, window, cx),
            messages: vec![],
            busy: false,
            queue: vec![],
            ready: false,
            loading: false,
            picking: false,
            sessions: None,
            status: String::new(),
            client: None,
            agent: None,
            generation: 0,
            draft,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            selection: cx.new(|_| ui::markdown::Selection::default()),
            focus: cx.focus_handle(),
        }
    }

    /// Puts a blank thread on screen, parking the one that was, unless that is still blank.
    fn add_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = self.blank_thread(window, cx);
        let old = std::mem::replace(&mut self.thread, fresh);
        // A blank thread, or a session listing nothing was picked from, isn't kept.
        if old.agent.is_some() && !old.is_listing() {
            self.parked.push(old);
        }
    }

    /// Runs `work` with thread `id` as [`Self::thread`] (in whichever project it belongs to), and
    /// whether it is the one on screen. `None` if there is no such thread.
    fn with_thread<R>(&mut self, id: u64, work: impl FnOnce(&mut Self, bool) -> R) -> Option<R> {
        if self.thread.id == id || self.parked.iter().any(|thread| thread.id == id) {
            return self.with_thread_here(id, work);
        }
        let at = self.slots.iter().position(|slot| slot.thread.id == id || slot.parked.iter().any(|thread| thread.id == id))?;
        self.swap_slot(at);
        let out = self.with_thread_here(id, |reader, _| work(reader, false));
        self.swap_slot(at);
        out
    }

    /// [`Self::with_thread`] for a thread of the project on screen.
    fn with_thread_here<R>(&mut self, id: u64, work: impl FnOnce(&mut Self, bool) -> R) -> Option<R> {
        if self.thread.id == id {
            return Some(work(self, true));
        }
        let at = self.parked.iter().position(|thread| thread.id == id)?;
        std::mem::swap(&mut self.thread, &mut self.parked[at]);
        let out = work(self, false);
        std::mem::swap(&mut self.thread, &mut self.parked[at]);
        Some(out)
    }

    /// Brings thread `id` on screen.
    fn show_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(at) = self.parked.iter().position(|thread| thread.id == id) {
            std::mem::swap(&mut self.thread, &mut self.parked[at]);
            // A session listing that was left without picking anything isn't kept.
            self.parked.retain(|thread| !thread.is_listing());
            self.thread.unread = false;
            self.wake_thread(window, cx);
            self.request_save(cx);
            cx.notify();
        }
    }

    /// Starts the agent of the thread on screen again if it was stopped, resuming its session.
    fn wake_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread.asleep() {
            return;
        }
        let (Some(key), Some(session)) = (self.thread.agent.clone(), self.thread.session.clone()) else { return };
        log::info!("agent: waking {key} session {session}");
        self.connect(&key, true, window, cx);
        self.thread.restore = Some(session);
        self.thread.status = "waking…".into();
    }

    /// The threads not on screen, in every project.
    fn background_threads(&mut self) -> impl Iterator<Item = &mut Thread> {
        self.parked.iter_mut().chain(self.slots.iter_mut().flat_map(|slot| std::iter::once(&mut slot.thread).chain(slot.parked.iter_mut())))
    }

    /// A thread with no agent running, to be resumed from `saved`'s session when it is shown.
    fn sleeping_thread(&mut self, saved: workspace::ThreadState, window: &mut Window, cx: &mut Context<Self>) -> Thread {
        let mut thread = self.blank_thread(window, cx);
        thread.agent = Some(saved.agent);
        thread.session = Some(saved.session);
        thread.saved_title = Some(saved.title).filter(|title| !title.is_empty());
        thread.status = "asleep".into();
        thread
    }

    /// Brings back the projects and threads the last window started here left. The project on
    /// screen is the one `po` was started on; the others wait in the background and the
    /// threads sleep until shown. True if the project on screen got its threads back.
    fn restore(&mut self, saved: workspace::Workspace, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut resumed = false;
        let mut seen = HashSet::new();
        self.side_open = saved.viewer_open;
        self.swapped = saved.swapped;
        self.agent_open = saved.agent_open;
        self.sidebars.update(cx, |sidebars, _| sidebars.set_widths(saved.left_width.map(px), saved.right_width.map(px)));
        self.chosen_agent = saved.agent.clone().or(self.chosen_agent.take());
        if let Some(height) = saved.split {
            self.left_split.update(cx, |split, _| split.set_top(px(height)));
        }
        self.left_split.update(cx, |split, _| split.set_collapsed(saved.explorer_collapsed));
        for state in saved.projects {
            let worktree = match state.worktree {
                Some(spec) => {
                    let branch = spec.branch.clone();
                    match worktree::Worktree::adopt(spec) {
                        Some(wt) => Some(wt),
                        None => {
                            log::warn!("worktree {} could not be restored; branch {branch} is left in its repository", state.root.display());
                            self.worktree_status = Some(format!("A worktree could not be restored; its branch {branch} is left in the repository"));
                            continue;
                        }
                    }
                }
                None => None,
            };
            let start = worktree.is_none() && self.project.pr.is_none() && self.project.worktree.is_none() && state.root == self.project.root;
            if !start && (!state.root.is_dir() || !seen.insert(state.root.clone()) || state.root == self.project.root) {
                continue;
            }
            let mut threads: Vec<Thread> = state.threads.into_iter().map(|t| self.sleeping_thread(t, window, cx)).collect();
            let shown = (!threads.is_empty()).then(|| threads.remove(state.active.min(threads.len() - 1)));
            if start {
                if let Some(shown) = shown {
                    self.thread = shown;
                    self.parked = threads;
                    resumed = true;
                }
                continue;
            }
            let mut project = Self::build_project(state.root, None, vec![], window, cx);
            if let Some(wt) = worktree {
                project.base = wt.spec.base.clone();
                project.worktree = Some(wt);
            }
            let thread = match shown {
                Some(thread) => thread,
                None => self.blank_thread(window, cx),
            };
            let id = self.next_project;
            self.next_project += 1;
            let mode = if state.review { Mode::Diff } else { Mode::Source };
            self.slots.push(Slot { id, mode, project, thread, parked: threads });
        }
        resumed
    }

    /// Every open project with its threads, oldest first.
    fn views(&self) -> Vec<View<'_>> {
        let mut views = vec![View {
            id: self.project_id,
            mode: self.mode,
            project: &self.project,
            shown: self.thread.id,
            threads: std::iter::once(&self.thread).chain(&self.parked).collect(),
        }];
        views.extend(self.slots.iter().map(|slot| View {
            id: slot.id,
            mode: slot.mode,
            project: &slot.project,
            shown: slot.thread.id,
            threads: std::iter::once(&slot.thread).chain(&slot.parked).collect(),
        }));
        views.sort_by_key(|view| view.id);
        // The thread on screen is first among them; lists keep the order they were made in.
        for view in &mut views {
            view.threads.sort_by_key(|thread| thread.id);
        }
        views
    }

    /// What is worth keeping of the projects and threads: those with an agent session.
    fn workspace(&self, cx: &App) -> workspace::Workspace {
        // A PR is a throwaway checkout; it is not brought back.
        let projects = self
            .views()
            .into_iter()
            .filter(|view| view.project.pr.is_none())
            .map(|view| {
                let mut threads: Vec<&Thread> = view.threads.into_iter().filter(|t| t.agent.is_some() && t.session.is_some()).collect();
                threads.sort_by_key(|t| t.id);
                workspace::ProjectState {
                    root: view.project.root.clone(),
                    review: view.mode == Mode::Diff,
                    worktree: view.project.worktree.as_ref().map(|wt| wt.spec.clone()),
                    active: threads.iter().position(|t| t.id == view.shown).unwrap_or(0),
                    threads: threads
                        .into_iter()
                        .filter_map(|t| {
                            Some(workspace::ThreadState { agent: t.agent.clone()?, session: t.session.clone()?, title: t.title() })
                        })
                        .collect(),
                }
            })
            .collect();
        workspace::Workspace { projects, viewer_open: self.side_open, agent_open: self.agent_open, swapped: self.swapped, agent: self.chosen_agent.clone(), split: self.left_split.read(cx).top().map(f32::from), explorer_collapsed: self.left_split.read(cx).collapsed(), left_width: Some(self.sidebars.read(cx).widths().0.into()), right_width: self.sidebars.read(cx).widths().1.map(f32::from) }
    }

    /// Saves the workspace once the current change has settled, so that a thread that is
    /// swapped in to handle an event isn't saved as the one on screen.
    fn request_save(&mut self, cx: &mut Context<Self>) {
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        let this = cx.entity();
        cx.defer(move |cx| {
            this.update(cx, |reader, cx| {
                reader.save_pending = false;
                reader.workspace(cx).save(&reader.home);
            })
        });
    }

    /// Stops the agents of threads that have been idle in the background for too long.
    fn sleep_idle(&mut self, cx: &mut Context<Self>) {
        let Some(after) = self.idle_after else { return };
        let mut slept = false;
        for thread in self.background_threads().filter(|t| t.restful() && t.touched.elapsed() > after) {
            log::info!("agent: stopping idle thread {}", thread.id);
            thread.sleep();
            slept = true;
        }
        if slept {
            cx.notify();
        }
    }

    /// Stops the longest idle agents in the background while more than `max_live` run.
    fn enforce_cap(&mut self) {
        let mut live = usize::from(self.thread.client.is_some());
        live += self.background_threads().filter(|t| t.client.is_some()).count();
        let Some(excess) = live.checked_sub(self.max_live).filter(|n| *n > 0) else { return };
        let mut idle: Vec<&mut Thread> = self.background_threads().filter(|t| t.restful()).collect();
        idle.sort_by_key(|t| t.touched);
        for thread in idle.into_iter().take(excess) {
            log::info!("agent: stopping thread {} to stay under the limit", thread.id);
            thread.sleep();
        }
    }

    /// Tells the desktop about `what` the thread on screen did, unless that is already in view.
    fn alert(&self, window: &Window, visible: bool, what: &str) {
        if self.notifications && !(visible && window.is_window_active()) {
            alert::send(&format!("{} · {}", self.project.name(), self.thread.title()), what);
        }
    }

    /// Brings up thread `tid` of project `pid`, whichever is on screen.
    fn open_thread(&mut self, pid: u64, tid: u64, window: &mut Window, cx: &mut Context<Self>) {
        if pid == self.project_id {
            self.show_thread(tid, window, cx);
        } else {
            self.show_project_with(pid, Some(tid), window, cx);
        }
    }

    /// Ends thread `tid` of project `pid`, whichever is on screen.
    fn close_any_thread(&mut self, pid: u64, tid: u64, window: &mut Window, cx: &mut Context<Self>) {
        if pid == self.project_id {
            return self.close_thread(tid, window, cx);
        }
        let last = self.slots.iter().find(|slot| slot.id == pid).is_some_and(|slot| slot.thread.id == tid && slot.parked.is_empty());
        let blank = last.then(|| self.blank_thread(window, cx));
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == pid) else { return };
        if let Some(at) = slot.parked.iter().position(|thread| thread.id == tid) {
            slot.parked.remove(at);
        } else if slot.thread.id == tid
            && let Some(next) = slot.parked.pop().or(blank)
        {
            slot.thread = next;
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Opens a blank thread in project `pid`.
    fn start_thread_in(&mut self, pid: u64, window: &mut Window, cx: &mut Context<Self>) {
        if pid != self.project_id {
            self.show_project(pid, window, cx);
        }
        self.new_blank_thread(window, cx);
    }

    /// Ends thread `id`, stopping its agent. Closing the one on screen brings up another.
    fn close_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(at) = self.parked.iter().position(|thread| thread.id == id) {
            self.parked.remove(at);
        } else if self.thread.id == id {
            let next = match self.parked.pop() {
                Some(next) => next,
                None => self.blank_thread(window, cx),
            };
            self.thread = next;
            self.thread.unread = false;
            self.wake_thread(window, cx);
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Opens a listed session; its history arrives as updates before the agent is ready.
    fn resume_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A session already open in a thread is shown rather than opened a second time, and the
        // listing it was picked from goes away.
        let open = self
            .views()
            .iter()
            .find_map(|view| view.threads.iter().find(|t| t.session.as_deref() == Some(id)).map(|t| (view.id, t.id)));
        if let Some((pid, tid)) = open {
            let listing = (self.project_id, self.thread.id);
            self.open_thread(pid, tid, window, cx);
            if self.views().iter().any(|v| v.id == listing.0 && v.threads.iter().any(|t| t.id == listing.1)) {
                self.close_any_thread(listing.0, listing.1, window, cx);
            }
            return;
        }
        let Some(client) = &self.thread.client else { return };
        if self.thread.loading || !client.load(id) {
            return;
        }
        self.thread.loading = true;
        self.thread.sessions = None;
        self.thread.status = "loading session…".into();
        cx.notify();
    }

    fn agent_label(&self) -> Option<&str> {
        let key = self.thread.agent.as_ref()?;
        self.agents.iter().find(|agent| agent.id == *key).map(|agent| agent.name.as_str())
    }




    /// `visible`: this thread is the one on screen, so its edits may move the viewer.
    fn on_event(&mut self, event: Event, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Most events change the last message; a tool call update may change an earlier one.
        let mut changed = self.thread.messages.len().saturating_sub(1);
        self.thread.touched = Instant::now();
        match event {
            Event::Sessions(sessions) => match self.thread.restore.take() {
                // A thread being woken goes straight to its own session.
                Some(id) => match &self.thread.client {
                    Some(client) if client.load(&id) => {
                        self.thread.loading = true;
                        self.thread.status = "loading session…".into();
                    }
                    _ => {
                        self.thread.status = "not running".into();
                        self.thread.fail("The session could not be resumed.".into());
                    }
                },
                None => {
                    self.thread.status = if sessions.is_empty() { "no sessions".into() } else { "pick a session".into() };
                    self.thread.sessions = Some(sessions);
                }
            },
            Event::Ready { agent } => {
                self.thread.ready = true;
                self.thread.loading = false;
                self.thread.picking = false;
                log::info!("agent: {agent} ready");
                self.thread.status = "ready".into();
                self.thread.session = self.thread.client.as_ref().and_then(|client| client.session());
                self.request_save(cx);
                if let Some(prompt) = self.thread.pending.take()
                    && !self.send_to_agent(&prompt)
                {
                    self.thread.pending = Some(prompt);
                    self.return_pending();
                    self.thread.fail("The agent has no session yet.".into());
                    self.restore_queue(window, cx);
                }
            }
            Event::Update(update) => {
                if let Some(ix) = self.thread.tool_message(&update) {
                    changed = changed.min(ix);
                }
                // A replayed history would otherwise open every file it ever touched.
                if let Some((path, finished)) = self.thread.update(&update).filter(|_| !self.thread.loading && visible) {
                    self.follow(&path, finished, window, cx);
                }
            }
            Event::Permission { request, title, options } => {
                self.alert(window, visible, &format!("Needs your approval: {title}"));
                self.thread.blocks().push(Block::Permission(Ask { request, title, options, answered: None }));
            }
            Event::Stopped(reason) => {
                self.thread.busy = false;
                self.thread.unread = !visible;
                self.alert(window, visible, "Finished");
                log::info!("agent: stopped ({reason})");
                // What the daemon didn't take into the turn is no longer in its queue.
                for item in &mut self.thread.queue {
                    item.kind = None;
                }
                self.send_next(window, cx);
            }
            Event::Accepted { content, queue } => {
                if let Some(at) = self.thread.queue.iter().position(|q| q.kind == Some(queue) && q.content == content) {
                    self.thread.queue.remove(at);
                }
                self.thread.messages.push(Msg::User(content));
            }
            Event::Rejected { content, queue } => {
                // No turn to join: it waits for the turn to end like any other queued message.
                if let Some(item) = self.thread.queue.iter_mut().find(|q| q.kind == Some(queue) && q.content == content) {
                    item.kind = None;
                }
                self.send_next(window, cx);
            }
            Event::Failed(error) => {
                if !self.thread.ready {
                    self.thread.status = "failed to start".into();
                }
                self.return_pending();
                self.thread.fail(error);
                self.restore_queue(window, cx);
            }
            Event::Exited(status) => {
                log::warn!("agent exited: {status}");
                self.thread.ready = false;
                self.thread.status = "exited".into();
                if self.thread.busy {
                    self.return_pending();
                    self.thread.fail(format!("the agent exited ({status})"));
                    self.restore_queue(window, cx);
                }
            }
        }
        self.thread.relist(changed);
        self.thread.list.scroll_to_end();
        cx.notify();
    }



    /// Shows the file a tool call touches; a finished call reloads it to show the change.
    fn follow(&mut self, path: &str, finished: bool, window: &mut Window, cx: &mut Context<Self>) {
        let path = Path::new(path);
        let relative = path.strip_prefix(&self.project.root).unwrap_or(path);
        if relative.is_absolute() || !self.project.root.join(relative).is_file() {
            return;
        }
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !self.project.files.contains(&relative) {
            self.rescan(cx);
        }
        if self.project.current.as_deref() != Some(&relative) || finished {
            log::info!("agent: showing {relative}");
            self.open(&relative, window, cx);
        }
        if self.mode == Mode::Diff {
            self.refresh_changes(false, Some(relative), cx);
        }
    }

    fn answer(&mut self, msg: usize, block: usize, option: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Permission(ask)) = blocks.get_mut(block) else { return };
        let (Some(choice), None) = (ask.options.get(option), &ask.answered) else { return };
        if let Some(client) = &self.thread.client {
            client.answer(ask.request.clone(), Some(&choice.id));
        }
        ask.answered = Some(choice.name.clone());
        self.thread.relist(msg);
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
                InputEvent::PressEnter { alt: false, .. } => this.accept_picked(window, cx),
                InputEvent::PressEnter { .. } => {}
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
        self.finder_matches = Rc::new(match_files(&self.project.files, &query));
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
        if self.project.notice.is_none() {
            self.hide_finder(window, cx);
            self.set_mode(Mode::Source, cx);
            self.reveal_side(cx);
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
                            .child(if self.project.files.len() >= MAX_FILES { format!("first {MAX_FILES} files only") } else { "↑↓ navigate".to_string() })
                            .child("↵ open · esc close"),
                    ),
            )
            .into_any_element()
    }

    /// The rendered form of `text` if `path` is markdown, keeping the scroll position and
    /// selection when it's the file already open being read again.
    fn markdown_doc(&mut self, path: &str, text: &str, cx: &mut Context<Self>) -> Option<Doc> {
        if !is_markdown(path) {
            return None;
        }
        let chunks = Rc::new(ui::markdown::chunks(text));
        match self.project.doc.take().filter(|_| self.project.current.as_deref() == Some(path)) {
            Some(doc) if *doc.chunks == *chunks => Some(doc),
            Some(doc) => {
                // Splicing resets the scroll to the top, so put it back, and drop a selection
                // that may now point at different text.
                let mut top = doc.list.logical_scroll_top();
                doc.list.splice(0..doc.list.item_count(), chunks.len());
                if top.item_ix >= chunks.len() {
                    top = gpui::ListOffset { item_ix: chunks.len().saturating_sub(1), offset_in_item: px(0.) };
                }
                doc.list.scroll_to(top);
                doc.selection.update(cx, |s, _| s.clear());
                Some(Doc { chunks, ..doc })
            }
            None => Some(Doc {
                list: ListState::new(chunks.len(), ListAlignment::Top, px(400.)),
                chunks,
                selection: cx.new(|_| ui::markdown::Selection::default()),
                focus: cx.focus_handle(),
            }),
        }
    }

    fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let full = self.project.root.join(path);
        let too_big = std::fs::metadata(&full).is_ok_and(|m| m.len() > 4 * 1024 * 1024);
        // A file too big to show isn't read at all.
        let read = if too_big { Ok(None) } else { std::fs::read_to_string(&full).map(Some) };
        match read {
            Ok(None) => {
                self.project.doc = None;
                self.project.notice = Some(format!("{path} is too large to show"));
            }
            Ok(Some(text)) => {
                self.project.doc = self.markdown_doc(path, &text, cx);
                match &self.project.code {
                    // Same file: swap the text in place so the scroll position and caret survive.
                    Some(code) if self.project.current.as_deref() == Some(path) => {
                        code.update(cx, |state, cx| state.set_text(text, cx));
                    }
                    _ => self.project.code = Some(cx.new(|cx| viewer::CodeViewState::new(text, syntax(path), window, cx))),
                }
                self.project.notice = None;
            }
            Err(error) => {
                log::warn!("open {path}: {error}");
                self.project.code = None;
                self.project.doc = None;
                self.project.notice = Some(format!("{path} is not a text file"));
            }
        }
        self.project.current = Some(path.to_string());
        self.project.stamp = stamp(&full);
        let id = SharedString::from(path.to_string());
        self.project.tree.update(cx, |tree, cx| {
            if let Some(ix) = tree.index_of(&id) {
                tree.set_selected_index(Some(ix), cx);
            }
        });
        cx.notify();
    }

    /// Sends the draft: as a new turn, or, while the agent is working, into the queue
    /// (`later` for after the turn rather than into it).
    fn send(&mut self, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.thread.draft.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        if self.thread.agent.is_none() {
            return self.start_thread(prompt, window, cx);
        }
        let starting = self.thread.client.is_some() && !self.thread.ready && !self.thread.loading && !self.thread.picking && self.thread.restore.is_none();
        if starting && !self.thread.busy {
            // The agent is still starting: the message waits for it, shown as sent.
            self.thread.draft.update(cx, |input, cx| input.set_value("", window, cx));
            self.begin_turn(&prompt);
            self.thread.pending = Some(prompt);
            cx.notify();
            return;
        }
        if !self.thread.busy {
            if self.dispatch(prompt, cx) {
                self.thread.draft.update(cx, |input, cx| input.set_value("", window, cx));
            }
            return;
        }
        self.thread.draft.update(cx, |input, cx| input.set_value("", window, cx));
        // An agent that can't take messages mid-turn gets them when the turn is over; neither
        // can one that hasn't been given the first message yet.
        let kind = match self.thread.client.as_ref().filter(|client| client.queues() && self.thread.pending.is_none()) {
            Some(client) => {
                let kind = if later { Queue::Followup } else { Queue::Steer };
                client.enqueue(&prompt, kind);
                Some(kind)
            }
            None => None,
        };
        self.thread.queue.push(Queued { content: prompt, kind });
        cx.notify();
    }

    /// Takes a queued message back into the draft to edit.
    fn recall(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.thread.queue.len() {
            return;
        }
        let item = self.thread.queue.remove(ix);
        if let (Some(kind), Some(client)) = (item.kind, &self.thread.client) {
            client.recall(&item.content, kind);
        }
        let draft = self.thread.draft.read(cx).value().to_string();
        let text = if draft.trim().is_empty() { item.content } else { format!("{}\n{draft}", item.content) };
        self.thread.draft.update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    /// If no turn is running, sends the oldest queued message that waits here rather than in
    /// the daemon. A message that can't be sent goes back to the draft with the rest.
    fn send_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.busy {
            return;
        }
        let Some(at) = self.thread.queue.iter().position(|q| q.kind.is_none()) else { return };
        let next = self.thread.queue.remove(at);
        if !self.dispatch(next.content.clone(), cx) {
            self.thread.queue.insert(0, next);
            self.restore_queue(window, cx);
        }
    }

    /// Moves everything queued back into the draft, taking back what the daemon still holds.
    fn restore_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.queue.is_empty() {
            return;
        }
        let items: Vec<Queued> = std::mem::take(&mut self.thread.queue);
        let mut text = Vec::new();
        for item in items {
            if let (Some(kind), Some(client)) = (item.kind, &self.thread.client) {
                client.recall(&item.content, kind);
            }
            text.push(item.content);
        }
        let draft = self.thread.draft.read(cx).value().to_string();
        if !draft.trim().is_empty() {
            text.push(draft);
        }
        self.thread.draft.update(cx, |input, cx| input.set_value(text.join("\n\n"), window, cx));
        cx.notify();
    }

    /// Sends `prompt` as a new turn; false if it could not be sent (the error is in the thread).
    fn dispatch(&mut self, prompt: String, cx: &mut Context<Self>) -> bool {
        if !self.thread.ready || !self.send_to_agent(&prompt) {
            let why = if self.thread.client.is_some() && self.thread.ready { "The agent has no session yet." } else { "The agent is not ready yet." };
            self.thread.fail(why.into());
            cx.notify();
            return false;
        }
        self.begin_turn(&prompt);
        self.request_save(cx);
        cx.notify();
        true
    }

    /// Hands `prompt` to the agent, with the file in view as context. False if it can't take it.
    fn send_to_agent(&self, prompt: &str) -> bool {
        let Some(client) = &self.thread.client else { return false };
        let context = match &self.project.current {
            Some(file) => format!("[The user has `{file}` open in the viewer.]\n\n{prompt}"),
            None => prompt.to_string(),
        };
        client.prompt(&context)
    }

    /// Shows `prompt` as the start of a turn the agent is working on.
    fn begin_turn(&mut self, prompt: &str) {
        self.thread.busy = true;
        self.thread.touched = Instant::now();
        self.thread.messages.push(Msg::User(prompt.to_string()));
        self.thread.messages.push(Msg::Agent(vec![]));
        self.thread.relist(self.thread.messages.len() - 2);
        self.thread.list.scroll_to_end();
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = &self.thread.client else { return };
        client.cancel();
        for msg in &mut self.thread.messages {
            let Msg::Agent(blocks) = msg else { continue };
            for block in blocks {
                if let Block::Permission(ask) = block
                    && ask.answered.is_none() {
                        client.answer(ask.request.clone(), None);
                        ask.answered = Some("Cancelled".into());
                    }
            }
        }
        self.thread.relist(0);
        // What was waiting goes back to the draft rather than being sent into a stopped turn.
        self.restore_queue(window, cx);
        cx.notify();
    }

    fn toggle_tool(&mut self, msg: usize, block: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Tool(tool)) = blocks.get_mut(block) else { return };
        tool.open = !tool.open;
        self.thread.relist(msg);
        cx.notify();
    }

    /// The strip under the explorer header in Changes mode: an input to open a PR for
    /// review, or the open PR's summary with a way out.
    fn pr_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let (border, muted, danger) = (colors.border, colors.muted_foreground, colors.danger);
        let this = cx.entity();
        let body: AnyElement = match &self.project.pr {
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
                        .child(format!("PR #{} · {} files", pr.number, self.project.changes.len())),
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

    /// For a worktree project: its branch, and the ways to finish it.
    fn worktree_strip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let wt = self.project.worktree.as_ref()?;
        let colors = cx.global::<Palette>();
        let (border, muted) = (colors.border, colors.muted_foreground);
        let (merge, discard) = (cx.entity(), cx.entity());
        let busy = self.finishing;
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(border)
                .child(div().truncate().text_xs().text_color(muted).child(format!("⎇ {}", wt.spec.branch)))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("worktree-merge")
                                .label("Merge")
                                .tooltip("Commit this branch's work and merge it into the checkout it came from")
                                .small()
                                .outline()
                                .on_click(move |_, window, cx| {
                                    if !busy {
                                        merge.update(cx, |reader, cx| reader.finish_worktree(true, window, cx))
                                    }
                                }),
                        )
                        .child(
                            Button::new("worktree-discard")
                                .label(if self.confirm_discard { "Discard everything?" } else { "Discard" })
                                .tooltip("Delete this branch and everything done in it")
                                .small()
                                .ghost()
                                .on_click(move |_, window, cx| {
                                    if !busy {
                                        discard.update(cx, |reader, cx| reader.finish_worktree(false, window, cx))
                                    }
                                }),
                        ),
                ),
        )
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
        let state = if mode == Mode::Diff { &self.project.changes_tree } else { &self.project.tree };
        let marks = if mode == Mode::Diff { self.project.marks.clone() } else { Rc::default() };
        // Themes may lack a green (Rosé Pine's success is teal), so added files get their own.
        let (muted, danger) = (colors.muted_foreground, colors.danger);
        let kinds = [colors.primary, colors.warning, colors.info, gpui::hsla(0.37, 0.5, 0.5, 1.0)];
        let body: AnyElement = if mode == Mode::Diff && self.project.changes.is_empty() {
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
                        this.update(cx, |reader, cx| {
                            match mode {
                                Mode::Source => reader.open(&path, window, cx),
                                Mode::Diff => reader.pick(&path, cx),
                            }
                            reader.reveal_side(cx);
                        });
                    })
                    .into_any_element()
            })
            .into_any_element()
        };
        // The explorer can be folded down to this header, so it has a chevron.
        let title: AnyElement = {
            let folded = self.left_split.read(cx).collapsed();
            let fold = cx.entity();
            div()
                .id("explorer-fold")
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .cursor_pointer()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .on_click(move |_, _, cx| fold.update(cx, |reader, cx| reader.toggle_explorer(cx)))
                .child(
                    Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown })
                        .size(IconSize::XSmall)
                        .color(colors.muted_foreground),
                )
                .child(div().min_w_0().truncate().child(title.to_string()))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .child(header_row(
                title,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_text(
                        match &self.project.pr {
                            Some(pr) => format!("PR #{} {}", pr.number, pr.title),
                            None => self.project.root.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string()),
                        },
                        cx,
                    ))
                    .child(modes),
                cx,
            ))
            .children(self.worktree_strip(cx))
            .children((mode == Mode::Diff).then(|| self.pr_strip(cx)))
            .child(div().flex_1().min_h_0().p_1().child(body))
    }

    /// The left side: every project with its threads, and the active project's files below.
    fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let explorer = self.explorer(cx).into_any_element();
        let colors = cx.global::<Palette>();
        let (hover, selected, muted, foreground, primary) =
            (colors.list_hover, colors.list_selected, colors.muted_foreground, colors.foreground, colors.primary);
        let views = self.views();
        let (active_project, active_thread) = (self.project_id, self.thread.id);
        let project_count = views.len();
        let many = project_count > 1;
        let add = cx.entity();
        // Groups are spaced apart by more than anything inside one, so the eye reads them as units.
        let mut tree = div().id("agents-tree").size_full().overflow_y_scroll().flex().flex_col().gap_2().p_1();
        for view in &views {
            let pid = view.id;
            let (select, close, start) = (cx.entity(), cx.entity(), cx.entity());
            // A project and its threads are one card. The one on screen has a soft tint and its name at
            // full strength; the others have neither, but the same padding, so nothing shifts. Names
            // and thread titles share one left edge, after a column for the folder or status mark.
            let current = pid == active_project;
            let mut group = div()
                .flex()
                .flex_col()
                .gap_px()
                .p_1()
                .rounded_lg()
                .when(current, |card| card.bg(hover));
            // Inside the selected card the row under the pointer needs a different shade to show.
            let over = if current { selected.opacity(0.6) } else { hover };
            group = group.child(
                div()
                    .id(("agent-project", pid as usize))
                    .group(row_group("agent-project", pid))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1p5()
                    .h(rems(1.625))
                    // A little air between the project and its threads.
                    .mb_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |row| row.bg(over))
                    .on_click(move |_, window, cx| select.update(cx, |reader, cx| reader.show_project(pid, window, cx)))
                    .child(
                        div()
                            .w(rems(1.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(Icon::new(IconName::Folder).size(IconSize::XSmall).color(project_tone(primary, current))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(rems(0.9375))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(project_tone(primary, current))
                            .child(view.project.name()),
                    )
                    // Starting a thread is on the project's own row, shown when it is hovered.
                    .child(show_on_hover(
                        row_group("agent-project", pid),
                        Button::new(("agent-new-thread", pid as usize))
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .tooltip("New thread in this project")
                            .on_click(move |_, window, cx| start.update(cx, |reader, cx| reader.start_thread_in(pid, window, cx))),
                    ))
                    .when(many, |row| {
                        row.child(show_on_hover(
                            row_group("agent-project", pid),
                            Button::new(("agent-project-close", pid as usize))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip("Close project")
                                .on_click(move |_, window, cx| close.update(cx, |reader, cx| reader.close_project(pid, window, cx))),
                        ))
                    }),
            );
            for thread in view.threads.iter().filter(|t| t.agent.is_some() || t.id == view.shown) {
                let tid = thread.id;
                let (glyph, tint) = thread.activity().mark(colors);
                let (select, close) = (cx.entity(), cx.entity());
                group = group.child(
                    div()
                        .id(("agent-thread", tid as usize))
                        .group(row_group("agent-thread", tid))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1p5()
                        .h(rems(1.625))
                        .rounded_md()
                        .cursor_pointer()
                        .when((pid, tid) == (active_project, active_thread), |row| row.bg(selected))
                        .hover(move |row| row.bg(over))
                        .on_click(move |_, window, cx| select.update(cx, |reader, cx| reader.open_thread(pid, tid, window, cx)))
                        .child(div().w(rems(1.)).flex_none().flex().justify_center().text_size(rems(0.875)).text_color(tint).child(glyph))
                        .child(
                            // Threads sit a step below their project: smaller and regular, bright only
                            // for the one on screen.
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(rems(0.875))
                                .text_color(if (pid, tid) == (active_project, active_thread) { foreground } else { muted })
                                .child(thread.title()),
                        )
                        .child(show_on_hover(
                            row_group("agent-thread", tid),
                            Button::new(("agent-thread-close", tid as usize))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip("Close thread")
                                .on_click(move |_, window, cx| close.update(cx, |reader, cx| reader.close_any_thread(pid, tid, window, cx))),
                        )),
                );
            }
            tree = tree.child(group);
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            // The same kind of header as the explorer's below: a title, and what you can do here.
            .child(header_row(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Projects"))
                    .child(div().text_xs().text_color(muted).child(project_count.to_string())),
                Button::new("agents-add")
                    .icon(IconName::Plus)
                    .tooltip("Add a project: open a folder (Ctrl+O)")
                    .ghost()
                    .small()
                    .on_click(move |_, window, cx| add.update(cx, |reader, cx| reader.pick_project(window, cx))),
                cx,
            ))
            .children(self.worktree_status.clone().map(|text| {
                div().px_3().py_1().text_xs().text_color(muted).max_h(rems(5.)).overflow_hidden().child(text)
            }))
            .child(div().flex_1().min_h_0().child(vsplit(&self.left_split).top(tree).bottom(div().size_full().child(explorer))))
    }

    /// Folds the explorer on the agents view down to its header, or unfolds it to the height it
    /// had.
    fn toggle_explorer(&mut self, cx: &mut Context<Self>) {
        self.left_split.update(cx, |split, cx| {
            let folded = split.collapsed();
            split.set_collapsed(!folded);
            cx.notify();
        });
    }

    /// The viewer or the diff viewer, as the project's mode says.
    fn pane(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        match self.mode {
            Mode::Source => self.viewer(lead, trail, cx).into_any_element(),
            Mode::Diff => self.diff_viewer(lead, trail, cx).into_any_element(),
        }
    }

    fn diff_viewer(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let Some(path) = self.project.picked.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .bg(colors.background)
                .child(pane_header("Changes", status_text(String::new(), cx), lead, trail, cx))
                .child(div().flex_1().min_h_0().child(centered("Pick a changed file".into(), cx)));
        };
        let (rows, pairs) = (self.project.rows.clone(), self.project.pairs.clone());
        let (added, removed) = self.project.counts;
        // Themes may lack a green (Rosé Pine's success is teal), so added lines get their own.
        let (success, danger, muted, border) =
            (gpui::hsla(0.37, 0.5, 0.5, 1.0), colors.danger, colors.muted_foreground, colors.border);
        let font = cx.global::<Palette>().mono_font_family.clone();
        let split = self.split;
        // Only the rows in view are built, so a long diff costs no more than a short one.
        let body: AnyElement = if rows.is_empty() && self.project.diffing.is_some() {
            div().into_any_element()
        } else if rows.is_empty() {
            centered("No textual changes".into(), cx)
        } else {
            list(self.project.diff_list.clone(), move |ix, _, _| {
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
            .child(pane_header(&path, right, lead, trail, cx))
            .child(div().flex_1().min_h_0().border_color(border).child(body))
    }

    fn viewer(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.global::<Palette>().background;
        let title = self.project.current.clone().unwrap_or_else(|| "No file open".into());
        let doc = self.project.doc.as_ref().filter(|_| self.rendered);
        let body: AnyElement = match (&self.project.code, &self.project.notice) {
            (_, Some(notice)) => centered(notice.clone(), cx),
            (_, None) if let Some(doc) = doc => self.markdown_view(doc, cx),
            // Cached: the viewer redraws when it changes, not with every agent update or caret blink elsewhere.
            (Some(code), None) => code.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (None, None) => centered("Pick a file in the explorer".into(), cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(pane_header(
                &title,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_text(self.project.current.as_deref().map(language).unwrap_or("").to_string(), cx))
                    .children(self.project.doc.as_ref().map(|_| {
                        let this = cx.entity();
                        Button::new("toggle-rendered")
                            .label(if self.rendered { "Source" } else { "Preview" })
                            .tooltip(if self.rendered { "Show the markdown source (Ctrl+Shift+M)" } else { "Show the rendered markdown (Ctrl+Shift+M)" })
                            .ghost()
                            .small()
                            .on_click(move |_, _, cx| this.update(cx, |reader, cx| reader.toggle_rendered(cx)))
                    })),
                lead,
                trail,
                cx,
            ))
            .child(div().flex_1().min_h_0().child(body))
    }

    fn toggle_rendered(&mut self, cx: &mut Context<Self>) {
        self.rendered = !self.rendered;
        cx.notify();
    }

    /// A markdown file rendered, scrolling a piece at a time like the thread.
    fn markdown_view(&self, doc: &Doc, cx: &mut Context<Self>) -> AnyElement {
        doc.selection.update(cx, |s, _| s.begin_frame());
        let (chunks, selection, focus) = (doc.chunks.clone(), doc.selection.clone(), doc.focus.clone());
        let background = cx.global::<Palette>().background;
        div()
            .id("markdown-file")
            .key_context("Markdown")
            .track_focus(&focus)
            .size_full()
            .bg(background)
            .on_action(cx.listener({
                let (chunks, selection) = (chunks.clone(), selection.clone());
                move |_, _: &ui::markdown::Copy, _, cx| {
                    // Select All takes the pieces scrolled out of view too, which aren't laid out.
                    let text = if selection.read(cx).is_all() {
                        ui::markdown::all_text(chunks.iter().map(String::as_str))
                    } else {
                        selection.read(cx).selected_text()
                    };
                    if text.is_empty() {
                        return cx.propagate();
                    }
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener({
                let selection = selection.clone();
                move |_, _: &ui::markdown::SelectAll, _, cx| {
                    selection.update(cx, |s, _| s.select_all());
                    cx.notify();
                }
            }))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener({
                let (selection, focus) = (selection.clone(), focus.clone());
                move |_, event: &gpui::MouseDownEvent, window, cx| {
                    focus.focus(window, cx);
                    selection.update(cx, |s, _| s.mouse_down(event.position, event.click_count));
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener({
                let selection = selection.clone();
                move |_, event: &gpui::MouseMoveEvent, _, cx| {
                    if event.pressed_button == Some(gpui::MouseButton::Left) && selection.read(cx).selecting {
                        selection.update(cx, |s, _| s.drag(event.position));
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener({
                let selection = selection.clone();
                move |_, event: &gpui::MouseUpEvent, _, cx| {
                    if let Some(url) = selection.update(cx, |s, _| s.mouse_up(event.position)) {
                        cx.open_url(&url);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener({
                let selection = selection.clone();
                move |_, event: &gpui::MouseUpEvent, _, cx| {
                    selection.update(cx, |s, _| {
                        s.mouse_up(event.position);
                    });
                }
            }))
            .child(
                list(
                    doc.list.clone(),
                    cx.processor(move |_, ix: usize, window, cx| {
                        let Some(chunk) = chunks.get(ix) else { return div().into_any_element() };
                        // A readable column rather than the pane's whole width.
                        div()
                            .w_full()
                            .flex()
                            .justify_center()
                            .px_6()
                            .when(ix == 0, |this| this.pt_5())
                            .when(ix + 1 == chunks.len(), |this| this.pb_5())
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(820.))
                                    .pb(rems(1.))
                                    .child(ui::markdown::markdown(("file", ix), chunk, &selection, window, cx)),
                            )
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn agent_panel(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        self.thread.selection.update(cx, |s, _| s.begin_frame());
        // Only the messages in view are built and laid out, however long the thread gets.
        let selection = self.thread.selection.clone();
        let thread: AnyElement = if self.thread.agent.is_none() {
            centered("Pick an agent below, then send a message to start a thread".into(), cx)
        } else if self.thread.picking && self.thread.messages.is_empty() {
            match &self.thread.sessions {
                Some(sessions) if !self.thread.loading && !sessions.is_empty() => session_picker(sessions, &this, cx),
                Some(sessions) if !self.thread.loading => centered(format!("No sessions to resume in {}", self.project.root.display()), cx),
                _ => centered(self.thread.status.clone(), cx),
            }
        } else {
            list(
                self.thread.list.clone(),
                cx.processor(move |reader: &mut Self, ix: usize, window, cx| {
                    let Some(msg) = reader.thread.messages.get(ix) else { return div().into_any_element() };
                    let last = ix + 1 == reader.thread.messages.len();
                    // Boxes share the composer box's edges (the column's own padding); the text
                    // inside them is inset to the composer's text edge (see `text_inset`).
                    div()
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
        let (send, stop, history, fresh) = (cx.entity(), cx.entity(), cx.entity(), cx.entity());
        let agents = self.agents.clone();
        let in_repo = worktree::in_repo(&self.project.root);
        let (pick, isolate) = (cx.entity(), cx.entity());
        let typed = !self.thread.draft.read(cx).value().trim().is_empty();
        let stop_button = |stop: Entity<Reader>| {
            Button::new("stop")
                .icon(IconName::Square)
                .ghost()
                .rounded(gpui::px(999.0))
                .small()
                .tooltip("Stop")
                .on_click(move |_, window, cx| stop.update(cx, |reader, cx| reader.stop(window, cx)))
        };
        let send_button = |send: Entity<Reader>, tooltip: &'static str| {
            let button = Button::new("send")
                .icon(IconName::ArrowUp)
                .ghost()
                .rounded(gpui::px(999.0))
                .small()
                .tooltip(tooltip)
                .on_click(move |_, window, cx| send.update(cx, |reader, cx| reader.send(false, window, cx)));
            // Quiet until there is something to send.
            if typed { button.primary() } else { button }
        };
        let steer_tip = if self.thread.client.as_ref().is_some_and(|client| client.queues()) {
            "Steer: the agent reads it before its next step (Alt+Enter: after this turn)"
        } else {
            "Queue: sent when this turn is over"
        };
        let action = if self.thread.busy && typed {
            // Typing during a turn queues it; Stop stays one click away.
            div()
                .flex()
                .gap_1()
                .child(stop_button(stop))
                .child(send_button(send, steer_tip))
                .into_any_element()
        } else if self.thread.busy {
            stop_button(stop).into_any_element()
        } else {
            send_button(send, "Send").into_any_element()
        };
        let muted = colors.muted_foreground;
        let (border, queue_this) = (colors.border, cx.entity());
        let queued: Vec<AnyElement> = self
            .thread
            .queue
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let label = match item.kind {
                    Some(Queue::Steer) => "Steer",
                    Some(Queue::Followup) => "Follow-up",
                    None => "Queued",
                };
                let this = queue_this.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_none().child(label))
                    .child(div().flex_1().min_w_0().truncate().child(item.content.replace('\n', " ")))
                    .child(
                        Button::new(("recall", ix))
                            .icon(IconName::Close)
                            .ghost()
                            .small()
                            .tooltip("Take back to edit")
                            .on_click(move |_, window, cx| this.update(cx, |reader, cx| reader.recall(ix, window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        // Before the thread starts, what it starts with is chosen here; after, it is only named.
        let chips: AnyElement = if self.thread.agent.is_none() {
            let name = self.chosen().map_or_else(|| "No agent".to_string(), |agent| agent.name.clone());
            let isolated = self.thread.isolate;
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    dropdown(
                        "agent-chip",
                        Button::new("agent-chip").label(name).tooltip("The agent this thread starts with").ghost().small().dropdown_caret(true),
                        move || {
                            agents
                                .iter()
                                .map(|agent| {
                                    let (pick, key) = (pick.clone(), agent.id.clone());
                                    MenuItem::new(agent.name.clone(), move |_, cx| {
                                        pick.update(cx, |reader, cx| {
                                            reader.chosen_agent = Some(key.clone());
                                            cx.notify();
                                        })
                                    })
                                })
                                .collect()
                        },
                    )
                    .menu(self.agent_menu.clone()),
                )
                .children(in_repo.then(|| {
                    Button::new("worktree-chip")
                        .label("⎇ Worktree")
                        .tooltip("Work on a git branch of its own, to review and merge or discard")
                        .ghost()
                        .small()
                        .selected(isolated)
                        .on_click(move |_, _, cx| {
                            isolate.update(cx, |reader, cx| {
                                reader.thread.isolate = !reader.thread.isolate;
                                cx.notify();
                            })
                        })
                }))
                .into_any_element()
        } else {
            div().pl_2().text_sm().text_color(muted).child(self.agent_label().unwrap_or("Agent").to_string()).into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .child(pane_header(
                self.agent_label().unwrap_or("Agent"),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children((!self.thread.status.is_empty()).then(|| status_text(self.thread.status.clone(), cx)))
                    .child(
                        Button::new("history")
                            .label("History")
                            .tooltip("Resume an earlier session (Ctrl+Shift+H)")
                            .ghost()
                            .small()
                            .on_click(move |_, window, cx| history.update(cx, |reader, cx| reader.resume_thread(window, cx))),
                    )
                    .child(
                        Button::new("new-thread")
                            .label("New Thread")
                            .tooltip("New thread (Ctrl+N)")
                            .small()
                            .outline()
                            .on_click(move |_, window, cx| fresh.update(cx, |reader, cx| reader.new_blank_thread(window, cx))),
                    ),
                lead,
                trail,
                cx,
            ))
            .child(
                div()
                    .id("thread")
                    .key_context("Markdown")
                    .track_focus(&self.thread.focus)
                    .on_action(cx.listener(|reader, _: &ui::markdown::Copy, _, cx| {
                        // Select All takes the replies scrolled out of view too, which aren't laid out.
                        let text = if reader.thread.selection.read(cx).is_all() {
                            let mut sources: Vec<(&str, bool)> = Vec::new();
                            for msg in &reader.thread.messages {
                                match msg {
                                    Msg::User(text) => sources.push((text, true)),
                                    Msg::Agent(blocks) => {
                                        for block in blocks {
                                            match block {
                                                Block::Text(text) | Block::Thought(text) => sources.push((text, false)),
                                                Block::Tool(tool) if tool.open => {
                                                    sources.extend([&tool.arguments, &tool.result].into_iter().filter(|t| !t.is_empty()).map(|t| (t.as_str(), true)));
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                            ui::markdown::all_text_of(sources)
                        } else {
                            reader.thread.selection.read(cx).selected_text()
                        };
                        if text.is_empty() {
                            return cx.propagate();
                        }
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    }))
                    .on_action(cx.listener(|reader, _: &ui::markdown::SelectAll, _, cx| {
                        reader.thread.selection.update(cx, |s, _| s.select_all());
                        cx.notify();
                    }))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|reader, event: &gpui::MouseDownEvent, window, cx| {
                        reader.thread.focus.focus(window, cx);
                        reader.thread.selection.update(cx, |s, _| s.mouse_down(event.position, event.click_count));
                        cx.notify();
                    }))
                    .on_mouse_move(cx.listener(|reader, event: &gpui::MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(gpui::MouseButton::Left) && reader.thread.selection.read(cx).selecting {
                            reader.thread.selection.update(cx, |s, _| s.drag(event.position));
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(gpui::MouseButton::Left, cx.listener(|reader, event: &gpui::MouseUpEvent, _, cx| {
                        if let Some(url) = reader.thread.selection.update(cx, |s, _| s.mouse_up(event.position)) {
                            cx.open_url(&url);
                        }
                        cx.notify();
                    }))
                    .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|reader, event: &gpui::MouseUpEvent, _, cx| {
                        reader.thread.selection.update(cx, |s, _| {
                            s.mouse_up(event.position);
                        });
                    }))
                    .flex_1()
                    .min_h_0()
                    // A long line of text is hard to read, so the conversation keeps to a column.
                    // It has the composer's outer padding too, so both columns are the same width.
                    .child(div().size_full().flex().justify_center().px_3().child(div().h_full().w_full().max_w(px(860.0)).child(thread))),
            )
            .child(
                div().flex_none().p_3().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(860.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p(rems(COMPOSER_PAD_REM))
                        .rounded_lg()
                        .border(px(BOX_BORDER))
                        .border_color(colors.border)
                        .bg(colors.sidebar)
                        .when(!queued.is_empty(), |this| {
                            this.child(div().flex().flex_col().gap_1().pb_1().border_b_1().border_color(border).children(queued))
                        })
                        .child(Input::new(&self.thread.draft).large())
                        .child(
                            // The input's text sits 0.75rem in; the chips' own padding (0.5rem) plus this
                            // inset puts their text, and the label after the thread has started, on its edge.
                            div().flex().items_center().justify_between().gap_2().pl_1().child(chips).child(action),
                        ),
                ),
            )
    }
}

/// The composer's box: its border, its padding, and the padding of the input inside it. The
/// transcript derives its edges from these, so the two cannot drift apart.
const BOX_BORDER: f32 = 1.;
const COMPOSER_PAD_REM: f32 = 0.5;
const INPUT_PAD_REM: f32 = 0.75;

/// From a box's edge to its text in the composer; the transcript insets its text by the same.
fn text_inset(window: &Window) -> gpui::Pixels {
    rems(COMPOSER_PAD_REM + INPUT_PAD_REM).to_pixels(window.rem_size()) + px(BOX_BORDER)
}

fn render_msg(
    ix: usize,
    msg: &Msg,
    this: &Entity<Reader>,
    selection: &Entity<ui::markdown::Selection>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let blocks = match msg {
        Msg::User(text) => {
            let colors = cx.global::<Palette>();
            let (bg, text_el) = (colors.secondary, ui::markdown::plain(("user", ix), text, selection, |_| {}, window, cx));
            return div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui::relative(0.9))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(bg)
                        .text_sm()
                        .child(text_el),
                )
                .into_any_element();
        }
        Msg::Agent(blocks) => blocks,
    };
    let colors = cx.global::<Palette>();
    let mut column = div().flex().flex_col().gap_2().min_w_0();
    // Text blocks sit where the composer's text does; boxes (tool cards) span the composer's width.
    let text_inset = text_inset(window);
    if blocks.is_empty() {
        let thinking = div().text_sm().text_color(colors.muted_foreground).child("Thinking…");
        column = column.child(div().px(text_inset).child(thinking));
    }
    for (n, block) in blocks.iter().enumerate() {
        let id = ix * 10_000 + n;
        let (el, boxed) = match block {
            Block::Text(text) => (ui::markdown::markdown(("answer", id), text, selection, window, cx).into_any_element(), false),
            Block::Thought(text) => (
                div().opacity(0.7).child(ui::markdown::italic(("thought", id), text, selection, window, cx)).into_any_element(),
                false,
            ),
            Block::Tool(tool) => (tool_card(ix, n, tool, this, text_inset, selection, window, cx).into_any_element(), true),
            Block::Permission(ask) => (permission(ix, n, ask, this).into_any_element(), false),
            Block::Plan(entries) => (plan(entries, cx).into_any_element(), false),
        };
        column = column.child(if boxed { el } else { div().px(text_inset).child(el).into_any_element() });
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
                .on_click(move |_, window, cx| this.update(cx, |reader, cx| reader.resume_session(&id, window, cx)))
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

fn tool_card(
    ix: usize,
    n: usize,
    tool: &ToolBlock,
    this: &Entity<Reader>,
    text_inset: gpui::Pixels,
    selection: &Entity<ui::markdown::Selection>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    let (mark, tint) = step_mark(tool.status, colors);
    let (muted, border, background) = (colors.muted_foreground, colors.border, colors.background);
    let toggle = this.clone();
    let reveal = this.clone();
    let path = tool.path.clone();
    let mut card = div()
        .flex()
        .flex_col()
        .gap_1()
        .py_2()
        // The border plus this padding put the card's text on the composer's text edge.
        .px(text_inset - px(BOX_BORDER))
        .rounded_md()
        .border(px(BOX_BORDER))
        .border_color(border)
        .bg(background)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id(("tool", ix * 10_000 + n))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_sm()
                        .on_click(move |_, _, cx| toggle.update(cx, |reader, cx| reader.toggle_tool(ix, n, cx)))
                        .child(div().text_color(tint).child(mark))
                        .child(div().flex_1().min_w_0().truncate().child(tool.title.clone())),
                )
                .children(path.map(|path| {
                    Button::new(("tool-open", ix * 10_000 + n))
                        .label("Open")
                        .tooltip("Show this file, or its changes")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, window, cx| reveal.update(cx, |reader, cx| reader.reveal(&path, window, cx)))
                })),
        );
    if tool.open {
        for (which, text) in [&tool.arguments, &tool.result].into_iter().enumerate() {
            if !text.is_empty() {
                let mono = cx.global::<Palette>().mono_font_family.clone();
                let body = ui::markdown::plain(
                    ("tool-text", (ix * 10_000 + n) * 2 + which),
                    text,
                    selection,
                    |style| {
                        style.font_family = mono;
                        style.color = muted;
                    },
                    window,
                    cx,
                );
                card = card.child(
                    div()
                        .max_h(rems(16.))
                        .overflow_hidden()
                        .font_family(cx.global::<Palette>().mono_font_family.clone())
                        .text_xs()
                        .text_color(muted)
                        .child(body),
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

fn status_text(text: String, cx: &App) -> impl IntoElement {
    div().flex_none().truncate().text_xs().text_color(cx.global::<Palette>().muted_foreground).child(text)
}

/// The colour of a project's name and folder: the theme's primary, in full for the project on
/// screen and softer for the rest, so projects read apart from the threads under them.
fn project_tone(primary: gpui::Hsla, current: bool) -> gpui::Hsla {
    if current { primary } else { primary.opacity(0.78) }
}

/// The name that ties a row to the controls that appear only while it is hovered.
fn row_group(kind: &str, id: u64) -> SharedString {
    format!("{kind}-row-{id}").into()
}

/// `control`, kept in the row's layout but drawn only while the row named `group` is hovered.
fn show_on_hover(group: SharedString, control: impl IntoElement) -> gpui::Div {
    div().invisible().group_hover(group, |style| style.visible()).child(control)
}

/// A pane's header. The window's own controls sit in the headers of the panes at its edges:
/// `lead` before the title in the leftmost pane, `trail` after what is on the right in the
/// rightmost one.
fn pane_header(title: &str, right: impl IntoElement, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    header_row(
        div().flex().items_center().gap_1().min_w_0().children(lead).child(
            div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(colors.foreground).child(title.to_string()),
        ),
        div().flex().items_center().gap_2().child(right).children(trail),
        cx,
    )
}

/// A pane header: `left` and `right` on a bar with a line under it.
fn header_row(left: impl IntoElement, right: impl IntoElement, cx: &App) -> impl IntoElement {
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
        .text_color(colors.foreground)
        .child(left)
        .child(right)
}

fn centered(text: String, cx: &App) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .px_4()
        .text_center()
        .text_sm()
        .text_color(cx.global::<Palette>().muted_foreground)
        .child(div().max_w_full().child(text))
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
        let (background, foreground) = (colors.background, colors.foreground);
        let (left_open, side_open, swapped, agent_open) = (self.left_open, self.side_open, self.swapped, self.agent_open);
        // The window's controls live in the headers of the panes at its edges: the sidebar toggle
        // at the left of the leftmost pane, the viewer toggle (and the swap, while the viewer is
        // open) at the right of the rightmost one.
        let (left, right, swap) = (cx.entity(), cx.entity(), cx.entity());
        let sidebar_toggle = Button::new("toggle-left")
            .icon(if left_open { IconName::PanelLeftClose } else { IconName::PanelLeftOpen })
            .ghost()
            .small()
            .tooltip("Toggle the sidebar (Ctrl+B)")
            .on_click(move |_, _, cx| left.update(cx, |reader, cx| reader.toggle_left(cx)))
            .into_any_element();
        let swap_button = Button::new("swap-panes")
            .icon(IconName::ArrowLeftRight)
            .ghost()
            .small()
            .tooltip("Swap the viewer and the conversation (Ctrl+Shift+S)")
            .on_click(move |_, _, cx| swap.update(cx, |reader, cx| reader.swap_panes(cx)));
        // The right toggle works on the pane that sits on the right: the conversation while the
        // viewer is open and swapped, the viewer otherwise. Its icon says whether that pane shows.
        let (toggles_agent, right_open) = if side_open && swapped { (true, agent_open) } else { (false, side_open) };
        let viewer_toggle = Button::new("toggle-right")
            .icon(if right_open { IconName::PanelRightClose } else { IconName::PanelRightOpen })
            .ghost()
            .small()
            .tooltip(if toggles_agent { "Toggle the conversation (Ctrl+Shift+B)" } else { "Toggle the viewer (Ctrl+Shift+B)" })
            .on_click(move |_, _, cx| right.update(cx, |reader, cx| reader.toggle_right(cx)));
        let trailing = div().flex().items_center().gap_1().children(side_open.then_some(swap_button)).child(viewer_toggle).into_any_element();
        // With the viewer open they can trade places; with it closed the conversation is the middle
        // either way, and has both controls.
        let (middle, side) = if !side_open {
            (self.agent_panel(Some(sidebar_toggle), Some(trailing), cx).into_any_element(), self.pane(None, None, cx))
        } else if swapped {
            // The conversation sits on the right; with it collapsed the viewer keeps the middle
            // and carries both controls.
            if agent_open {
                (self.pane(Some(sidebar_toggle), None, cx), self.agent_panel(None, Some(trailing), cx).into_any_element())
            } else {
                (self.pane(Some(sidebar_toggle), Some(trailing), cx), self.pane(None, None, cx))
            }
        } else {
            (self.agent_panel(Some(sidebar_toggle), None, cx).into_any_element(), self.pane(None, Some(trailing), cx))
        };
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
            .on_action(cx.listener(|reader, _: &ShowSource, _, cx| reader.show_viewer(Mode::Source, cx)))
            .on_action(cx.listener(|reader, _: &ShowChanges, _, cx| reader.show_viewer(Mode::Diff, cx)))
            .on_action(cx.listener(|reader, _: &ShowChat, _, cx| reader.show_chat(cx)))
            .on_action(cx.listener(|reader, _: &SwapPanes, _, cx| reader.swap_panes(cx)))
            .on_action(cx.listener(|reader, _: &ShowHistory, window, cx| reader.resume_thread(window, cx)))
            .on_action(cx.listener(|reader, _: &ToggleSplit, _, cx| reader.set_split(!reader.split, cx)))
            .on_action(cx.listener(|reader, _: &ToggleRendered, _, cx| reader.toggle_rendered(cx)))
            .on_action(cx.listener(|reader, _: &OpenProject, window, cx| reader.pick_project(window, cx)))
            .on_action(cx.listener(|reader, _: &NewThread, window, cx| {
                reader.new_blank_thread(window, cx);
            }))
            // macOS draws no titlebar of its own (see `main`): this strip clears the traffic lights
            // and, like the titlebar it replaces, drags the window.
            .when(cfg!(target_os = "macos"), |this| {
                this.child(div().h(TITLEBAR_HEIGHT).flex_none().window_control_area(WindowControlArea::Drag))
            })
            .child(
                div().flex_1().min_h_0().child(
                    sidebars(&self.sidebars)
                        .left(left_open, px(220.0)..px(520.0), self.left_sidebar(cx))
                        .center(middle)
                        .right(right_open, px(320.0)..px(900.0), side),
                ),
            )
            .when(self.finder_open, |this| this.child(self.finder(cx)))
    }
}

/// Relaunch ourselves in its own process group with output going to a log file,
/// so the shell gets its prompt back (like `code .`).
fn detach(root: &Path, command: &Option<String>, pr: &Option<pr::Pr>) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let log_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                config::home().map(|home| home.join(".cache"))
            }
        })
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
    child.arg(root).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut child, 0);
    #[cfg(windows)]
    {
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        std::os::windows::process::CommandExt::creation_flags(&mut child, 0x0000_0008 | 0x0000_0200);
    }
    child.spawn()?;
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

/// Height of the strip standing in for macOS's titlebar, which holds the traffic lights.
const TITLEBAR_HEIGHT: Pixels = px(32.0);

fn main() {
    // `po update` is a terminal command, not a window: it runs to completion and
    // exits before any GUI is set up (like `dex update`).
    let cli: Vec<String> = std::env::args().skip(1).collect();
    if cli.len() == 1 && cli[0] == "update" {
        if std::path::Path::new("update").exists() {
            eprintln!("po-update: running self-update (use `po ./update` to open the local path 'update')");
        }
        match update::update() {
            Ok(message) => println!("{message}"),
            Err(message) => {
                eprintln!("po-update: {message}");
                std::process::exit(1);
            }
        }
        return;
    }
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
            KeyBinding::new("secondary-b", ToggleLeft, None),
            KeyBinding::new("secondary-shift-b", ToggleRight, None),
            KeyBinding::new("secondary-1", ShowSource, None),
            KeyBinding::new("secondary-2", ShowChanges, None),
            KeyBinding::new("secondary-3", ShowChat, None),
            KeyBinding::new("secondary-shift-s", SwapPanes, None),
            KeyBinding::new("secondary-shift-h", ShowHistory, None),
            KeyBinding::new("secondary-\\", ToggleSplit, None),
            KeyBinding::new("secondary-shift-m", ToggleRendered, None),
            KeyBinding::new("secondary-n", NewThread, None),
            KeyBinding::new("secondary-o", OpenProject, None),
            KeyBinding::new("secondary-p", ShowFinder, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
            KeyBinding::new("escape", CloseFinder, Some("Finder")),
            // The Input binds up/down itself and swallows them when single-line, so these name it.
            KeyBinding::new("up", FinderUp, Some("Finder > Input")),
            KeyBinding::new("down", FinderDown, Some("Finder > Input")),
        ]);
        cx.on_action(|_: &CloseWindow, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, _| window.remove_window()).ok();
            }
        });
        // macOS keeps an app running after its last window closes; this one has nothing left to show.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1360.0), px(840.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // On macOS, hide the titlebar but keep the traffic lights, centred in our own strip.
                titlebar: cfg!(target_os = "macos").then(|| TitlebarOptions {
                    title: Some("po".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.0), (TITLEBAR_HEIGHT - px(14.0)) / 2.0)),
                }),
                ..Default::default()
            },
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
