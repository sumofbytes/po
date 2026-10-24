#![recursion_limit = "1024"]
mod acp;
mod agent;
mod alert;
mod command;
mod config;
mod control;
mod dex;
mod diff;
mod fonts;
mod mcp;
mod pr;
mod schemes;
mod theme;
mod trace;
mod ui;
mod update;
mod viewer;
mod workspace;
mod worktree;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use acp::{ConfigOption, Event, PermissionOption, SessionInfo};
use agent::Client;
use dex::Queue;
use diff::{Change, Kind, Pair, Row};
use theme::Palette;
use ui::button::Button;
use ui::icon::{Icon, IconName, IconSize};
use ui::input::{Input, InputEvent, InputState};
use ui::list_item::ListItem;
use ui::menu::{CHECKED, MenuItem, MenuState, dropdown};
use ui::resizable::{SidebarsState, StackState, sidebars, stack};
use ui::file_tree::{FileKind, FileTree, file_kind};
use ui::tree::{TreeEvent, tree};
use futures::StreamExt;
use serde_json::Value;

use gpui::{
    AnyElement, App, Pixels, TitlebarOptions, WindowControlArea, point, Bounds, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState, ParentElement,
    Render, SharedString, StyleRefinement, Styled, StyledText, Subscription, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, div, list,
    prelude::*, px, rems, size, KeyBinding,
};

gpui::actions!(po, [ToggleTools, ToggleThoughts, ToggleLeft, ToggleRight, ShowSource, ShowChanges, ShowChat, SwapPanes, ShowHistory, ToggleSplit, ToggleRendered, NewThread, OpenProject, ShowFinder, CloseWindow, CloseFinder, FinderUp, FinderDown]);

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
    /// On the first call of a run of three or more: whether the run is folded to one line. `None`
    /// leaves it to `run_folded`'s default, until the user clicks the run's header.
    fold: Option<bool>,
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
    /// The agent's reasoning, and whether it is shown; it starts folded.
    Thought(String, bool),
    Tool(ToolBlock),
    Permission(Ask),
    /// Po wants to run an agent command that isn't one of its own, and waits to be told it may.
    /// `new_agent` is whether the agent's own command is the question; `mcp` are the config's MCP
    /// servers it would be handed.
    Approve { agent: config::Agent, new_agent: bool, mcp: Vec<config::McpServer>, resume: bool, answered: Option<bool> },
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

/// The sections of the left side, top to bottom.
const LIVE: usize = 0;
const PROJECTS: usize = 1;

/// The width, in pixels, the viewer's file tree starts with, and the range it is dragged within.
const TREE_WIDTH: f32 = 240.0;
const TREE_RANGE: std::ops::Range<gpui::Pixels> = px(160.)..px(340.);
/// A viewer pane narrower than this, in pixels, has no room for the file tree beside the file:
/// the widest tree leaves the file this much less than that.
const TREE_MIN_PANE: f32 = 560.0;

/// Whether a viewer pane `width` pixels wide has room for the file tree beside the file.
fn tree_fits(width: f32) -> bool {
    width >= TREE_MIN_PANE
}

/// What an empty viewer says to do: pick from the tree, or how to get it back.
fn pick_hint(what: &str, tree_visible: bool, tree_hidden: bool) -> String {
    if tree_visible {
        format!("Pick {what} in the tree")
    } else if tree_hidden {
        format!("Pick {what} with Ctrl+P, or show the file tree")
    } else {
        format!("Pick {what} with Ctrl+P, or widen the pane to show the file tree")
    }
}

/// The height, in pixels, the live agents list starts with.
const LIVE_HEIGHT: f32 = 136.0;
/// The height of the live pane while it has no agents to list: its header and a line.
const LIVE_EMPTY_REMS: f32 = 5.0;

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
    /// The changed files, folders open until the user closes them.
    changes_tree: FileTree,
    picked: Option<String>,
    /// The picked file's diff, its side-by-side pairs and how many lines it adds and removes.
    rows: Rc<Vec<Row>>,
    pairs: Rc<Vec<Pair>>,
    /// Syntax colors for the picked file's old and new versions.
    colors: Rc<diff::Colors>,
    counts: (usize, usize),
    /// The diff's rows as laid out, inline or split; only those in view are drawn.
    diff_list: ListState,
    current: Option<String>,
    stamp: Option<Stamp>,
    code: Option<Entity<viewer::CodeViewState>>,
    /// The current file rendered, when it is markdown.
    doc: Option<Doc>,
    notice: Option<String>,
    /// Every file, folders closed until the user opens them.
    files_tree: FileTree,
    /// Set when this is a thread's own checkout, made from another project's repository.
    worktree: Option<worktree::Worktree>,
    /// Keep the explorer and changes list wired to this project.
    _subscriptions: Vec<Subscription>,
}

impl Project {
    /// Whether the list shows the branch this project has checked out. A PR is a checkout of
    /// its own and a thread's worktree already names its branch.
    fn shows_branch(&self) -> bool {
        self.pr.is_none() && self.worktree.is_none()
    }

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
    /// Documents the agent put on screen through Po's MCP server.
    views: command::Views,
    /// The rendered form of each view.
    docs: HashMap<String, Doc>,
    /// The view the viewer shows in place of a file or the changes.
    shown: Option<String>,
    /// A view the agent added while the user was busy, waiting for a click.
    offered: Option<String>,
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
    /// When the thread last did anything, in milliseconds since the Unix epoch. Unlike
    /// `touched` it is saved, so the thread list keeps its order across restarts.
    active_at: u64,
    /// A first message waiting for the agent to be ready, already shown in the thread.
    pending: Option<String>,
    /// Start the thread in a git worktree of its own. Only asked before it has started.
    isolate: bool,
    /// Nothing has been sent in it and no session was picked for it. Its agent runs only to
    /// offer the session's settings, so its session is never kept or resumed.
    blank: bool,
    /// The settings being carried over from earlier threads, while the agent takes them one at
    /// a time: a model can change what the other settings offer. A first message waits for them.
    applying: Option<Applying>,
    /// Settings the agent was asked to carry over and gave up on answering; a late answer is
    /// not a choice of the user's.
    unanswered: HashSet<String>,
    messages: Vec<Msg>,
    busy: bool,
    /// Messages typed during a turn, shown above the input until they are taken.
    queue: Vec<Queued>,
    ready: bool,
    /// The agent's settings for the session, like its model, once it has said what they are.
    options: Vec<ConfigOption>,
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
    /// What Select All copies, in reading order: each text with whether it is plain (drawn by
    /// `markdown::plain`) rather than markdown. Includes replies scrolled out of view.
    fn selectable_sources(&self) -> Vec<(&str, bool)> {
        let mut sources = Vec::new();
        for msg in &self.messages {
            match msg {
                Msg::User(text) => sources.push((text.as_str(), true)),
                Msg::Agent(blocks) => {
                    for block in blocks {
                        match block {
                            Block::Text(text) | Block::Thought(text, true) => sources.push((text.as_str(), false)),
                            Block::Tool(tool) if tool.open => {
                                sources.extend([&tool.arguments, &tool.result].into_iter().filter(|t| !t.is_empty()).map(|t| (t.as_str(), true)));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        sources
    }

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
                Some(Block::Thought(body, _)) => body.push_str(text),
                _ => self.blocks().push(Block::Thought(text.to_string(), false)),
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
                fold: None,
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
            Activity::Unread => ("●", colors.added),
        }
    }

    /// Something the user may want to look at: not idle (an idle thread with a live agent still
    /// shows, last, at `IDLE_RANK`). Lower ranks come first in the live list,
    /// so a question comes before a finished turn, and both before work still going.
    fn live_rank(self) -> Option<u8> {
        match self {
            Activity::Asking => Some(0),
            Activity::Unread => Some(1),
            Activity::Running => Some(2),
            Activity::Idle => None,
        }
    }
}

/// The time now in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// How many of a project's threads the project list shows before the rest fold under "more".
const THREADS_SHOWN: usize = 3;

/// Where a running agent with nothing to show sorts in the live list: after all that need a look.
const IDLE_RANK: u8 = 3;

impl Thread {
    /// What it is doing, in a line for the live list.
    fn preview(&self) -> String {
        preview_of(&self.messages, self.busy)
    }

    fn activity(&self) -> Activity {
        let asking = match self.messages.last() {
            Some(Msg::Agent(blocks)) => blocks
                .iter()
                .any(|b| matches!(b, Block::Permission(Ask { answered: None, .. }) | Block::Approve { answered: None, .. })),
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

    /// Session settings in the order they are shown and carried over: the model first, as it
    /// can change what the others offer, then the others in the agent's order.
    fn ordered(mut options: Vec<&ConfigOption>) -> Vec<&ConfigOption> {
        options.sort_by_key(|option| option.category != "model");
        options
    }

    /// It was started with a first message, but its agent stopped before opening a session.
    fn unstarted(&self) -> bool {
        !self.blank && self.client.is_none() && self.session.is_none() && !self.picking
    }

    /// Whether to keep it once another thread is on screen: a blank thread, or a session listing
    /// nothing was picked from, has nothing to come back to.
    fn worth_keeping(&self) -> bool {
        !self.blank && !self.is_listing()
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
        self.drop_agent();
        self.status = if self.blank { String::new() } else { "asleep".into() };
    }

    /// Stops the agent and forgets what it was doing. A blank thread's session was never used,
    /// so it goes too, and the thread starts a new one when it is shown.
    fn drop_agent(&mut self) {
        self.client = None;
        self.ready = false;
        self.options.clear();
        self.applying = None;
        self.unanswered.clear();
        self.loading = false;
        self.picking = false;
        if self.blank {
            self.session = None;
        }
        // The events of the stopped agent are no longer this thread's.
        self.generation += 1;
    }
}

/// How far a new session has got in taking the settings last chosen for its agent.
#[derive(Default)]
struct Applying {
    /// The settings dealt with: carried over, already as chosen, or chosen in this thread since.
    settled: HashSet<String>,
    /// The setting the agent was asked to carry over and hasn't answered for yet.
    asked: Option<String>,
    /// Counts the questions put to the agent, so a timer can tell whether its own is still the one waiting.
    asks: u64,
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

/// How long the user must have been still before an agent may put a view in front of them.
const QUIET: Duration = Duration::from_secs(4);

/// A command that moves the viewer, held back until the user is still.
struct PendingMove {
    thread: u64,
    command: command::Command,
    /// What was on screen when it was asked for. If that is not the same when the pause comes,
    /// the user moved the viewer themselves, and theirs stands.
    from: Screen,
}

/// Which panes are open, as the workspace saves it.
#[derive(Clone, Copy, PartialEq)]
struct Panes {
    viewer: bool,
    agent: bool,
}

/// A layout an agent changed: what it was before, which is the user's, and what the agent left.
/// The workspace saves the user's while the agent's is still on screen, so an agent's move lasts
/// only until `po` is closed; once the user changes it again, what they leave is theirs.
#[derive(Clone, Copy)]
struct AgentChange<T> {
    before: T,
    left: T,
}

impl<T: Copy + PartialEq> AgentChange<T> {
    /// The record after an agent moved `from` to `to`: an earlier move it builds on keeps the
    /// user's layout from before that one.
    fn after(earlier: Option<Self>, from: T, to: T) -> Self {
        let before = earlier.filter(|change| change.left == from).map_or(from, |change| change.before);
        Self { before, left: to }
    }

    /// The record if the layout is still what the agent left; once the user has moved on it is
    /// gone, and coming back to the agent's layout by hand is theirs.
    fn settled(change: Option<Self>, now: T) -> Option<Self> {
        change.filter(|change| change.left == now)
    }

    /// What to save of `now`.
    fn saved(change: Option<&Self>, now: T) -> T {
        change.filter(|change| change.left == now).map_or(now, |change| change.before)
    }
}

impl PendingMove {
    /// The agent itself changed the layout, not the user: what it touched (`shown`, the viewer
    /// being open) is noted, so its move is not dropped for its own doing. A move of another
    /// thread stays as it was; it is dropped when that thread goes off screen.
    fn follows_agent(&mut self, on_screen: u64, now: &Screen) {
        if self.thread == on_screen {
            self.from.shown.clone_from(&now.shown);
            self.from.side_open = now.side_open;
            self.from.agent_open = now.agent_open;
        }
    }
}

/// What the user sees of the viewer: whether it is open, in which mode, and on what.
#[derive(Clone, PartialEq)]
struct Screen {
    side_open: bool,
    agent_open: bool,
    mode: Mode,
    current: Option<String>,
    picked: Option<String>,
    shown: Option<String>,
}

struct Reader {
    /// How agents started from this window reach it; none where there is no socket.
    control: Option<control::Control>,
    /// When the user last pressed a key or scrolled; an agent waits for a pause to show something.
    last_input: Instant,
    /// A viewer move the thread on screen asked for while the user was busy, waiting for a pause.
    pending_move: Option<PendingMove>,
    /// The panes an agent last opened or closed, and each project's mode an agent last changed,
    /// by project id; see [`AgentChange`].
    agent_panes: Option<AgentChange<Panes>>,
    agent_modes: HashMap<u64, AgentChange<Mode>>,
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
    /// The left side's sections: live agents and projects.
    sections: Entity<StackState>,
    /// Projects whose thread list is shown in full rather than cut to [`THREADS_SHOWN`].
    threads_expanded: HashSet<u64>,
    /// Each project's checked-out branch, read off the UI thread now and then.
    heads: HashMap<PathBuf, worktree::Head>,
    /// The width of the file tree inside the viewer pane.
    tree_cols: Entity<SidebarsState>,
    /// The file tree in the viewer is hidden.
    tree_hidden: bool,
    /// The viewer pane is too narrow for the file tree, as of the last frame it was drawn.
    viewer_narrow: Rc<Cell<bool>>,
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
    /// What was chosen for each agent's settings, such as its model.
    choices: config::Choices,
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
        let sections = cx.new(|_| StackState::new(vec![Some(px(LIVE_HEIGHT)), None]));
        let sidebars = cx.new(|_| SidebarsState::new(px(280.0), px(560.0)));
        let tree_cols = cx.new(|_| SidebarsState::new(px(TREE_WIDTH), px(0.0)));
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
            // Where a section was dragged to, or folded, is kept, once the drag is over.
            cx.observe(&sections, |reader, sections, cx| {
                if !sections.read(cx).dragging() {
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
            agents.push(config::Agent { id: command.clone(), name: "Custom".into(), command: command.clone(), daemon: None, vouched: true });
        }
        let project = Self::build_project(root, pr, files, window, cx);
        let thread = Thread {
            views: Default::default(),
            docs: HashMap::new(),
            shown: None,
            offered: None,
            id: 0,
            unread: false,
            session: None,
            restore: None,
            saved_title: None,
            touched: Instant::now(),
            active_at: now_ms(),
            pending: None,
            isolate: false,
            blank: true,
            applying: None,
            unanswered: HashSet::new(),
            _enter: enter,
            messages: vec![],
            busy: false,
            queue: vec![],
            ready: false,
            options: Vec::new(),
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
        let (control, requests) = control::start().unzip();
        let mut reader = Self {
            control,
            last_input: Instant::now().checked_sub(QUIET).unwrap_or_else(Instant::now),
            pending_move: None,
            agent_panes: None,
            agent_modes: HashMap::new(),
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
            sections,
            threads_expanded: HashSet::new(),
            heads: HashMap::new(),
            tree_cols,
            tree_hidden: false,
            viewer_narrow: Rc::default(),
            side_open: true,
            swapped: false,
            agent_open: true,
            agent_menu: cx.new(MenuState::new),
            choices: config::load_choices(),
            chosen_agent: command.clone(),
            split: false,
            rendered: true,
            focus: cx.focus_handle(),
            save_pending: false,
            _subscriptions: subscriptions,
        };
        if let Some(mut requests) = requests {
            let handle = window.window_handle();
            cx.spawn(async move |this, cx| {
                while let Some(request) = requests.next().await {
                    let answered = handle.update(cx, |_, window, cx| {
                        this.update(cx, |reader, cx| {
                            let reply = reader.run_command(request.caller, request.command, window, cx);
                            request.reply.send(reply).ok();
                        })
                    });
                    if answered.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        reader.focus.focus(window, cx);
        trace::mark("reader built");
        reader.refresh_tree(cx);
        trace::mark("tree");
        let resumed = workspace::Workspace::load(&reader.home).is_some_and(|saved| reader.restore(saved, window, cx));
        trace::mark("restore");
        match command.filter(|_| !resumed) {
            Some(command) => reader.connect(&command, false, window, cx),
            None => reader.wake_thread(window, cx),
        }
        trace::mark("connect");
        reader.watch(watch, window, cx);
        trace::mark("watch");
        // The branch beside each project's name follows checkouts made outside po too.
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(roots) = this.update(cx, |reader, _| {
                    reader.views().iter().filter(|view| view.project.shows_branch()).map(|view| view.project.root.clone()).collect::<Vec<_>>()
                }) else {
                    break;
                };
                // Each in its own run, so one slow repository doesn't hold up the rest. One
                // that takes too long keeps what it showed before.
                let runs = roots.into_iter().map(|root| {
                    let run = cx.background_executor().spawn({
                        let root = root.clone();
                        async move { worktree::head_of(&root) }
                    });
                    let limit = cx.background_executor().timer(Duration::from_secs(10));
                    async move {
                        match futures::future::select(run, limit).await {
                            futures::future::Either::Left((head, _)) => (root, Some(head)),
                            futures::future::Either::Right(_) => (root, None),
                        }
                    }
                });
                let found = futures::future::join_all(runs).await;
                if this
                    .update(cx, |reader, cx| {
                        let mut heads = HashMap::new();
                        for (root, head) in found {
                            if let Some(head) = head.unwrap_or_else(|| reader.heads.get(&root).cloned()) {
                                heads.insert(root, head);
                            }
                        }
                        if reader.heads != heads {
                            reader.heads = heads;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        })
        .detach();
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
        let files_tree = FileTree::new(false, cx);
        let changes_tree = FileTree::new(true, cx);
        // Only the project on screen has its trees touched, so these act on `reader.project`;
        // the check keeps a stray event from another project's tree off it.
        let subscriptions = vec![
            cx.subscribe(&files_tree.state, |reader, tree, event: &TreeEvent, _| {
                if reader.project.files_tree.state == tree {
                    reader.project.files_tree.note(event);
                }
            }),
            cx.subscribe(&changes_tree.state, |reader, tree, event: &TreeEvent, _| {
                if reader.project.changes_tree.state == tree {
                    reader.project.changes_tree.note(event);
                }
            }),
            // Moving the selection with the keyboard opens the file too, like a click.
            cx.observe_in(&files_tree.state, window, |reader, tree, window, cx| {
                if reader.project.files_tree.state != tree {
                    return;
                }
                let path = tree.read(cx).selected_entry().filter(|e| !e.folder).map(|e| e.id.to_string());
                // Only when the user just pressed something: the agent opening a file selects it too.
                if path.is_some() && reader.last_input.elapsed() < Duration::from_secs(1) {
                    reader.thread.shown = None;
                }
                if let Some(path) = path.filter(|p| reader.mode == Mode::Source && reader.project.current.as_deref() != Some(p)) {
                    reader.open(&path, window, cx);
                }
            }),
            cx.observe(&changes_tree.state, |reader, tree, cx| {
                if reader.project.changes_tree.state != tree {
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
            picked: None,
            rows: Rc::new(vec![]),
            pairs: Rc::new(vec![]),
            colors: Rc::default(),
            counts: (0, 0),
            diff_list: ListState::new(0, ListAlignment::Top, px(400.)),
            current: None,
            stamp: None,
            code: None,
            doc: None,
            notice: None,
            files_tree,
            worktree: None,
            _subscriptions: subscriptions,
        }
    }

    /// Rebuilds the explorer from `files`, keeping folders open and the current file selected.
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        self.project.files_tree.set_paths(&self.project.files, self.project.current.as_deref(), cx);
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
            self.project.changes_tree.set_paths(&paths, None, cx);
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
                    self.set_rows(vec![], diff::Colors::default());
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

    /// Shows the diff `rows` colored by `colors`, and works out their side-by-side pairs and counts once.
    fn set_rows(&mut self, rows: Vec<Row>, colors: diff::Colors) {
        self.project.counts = rows.iter().fold((0, 0), |(a, r), row| match row.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            _ => (a, r),
        });
        self.project.pairs = Rc::new(diff::split(&rows));
        self.project.rows = Rc::new(rows);
        self.project.colors = Rc::new(colors);
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
            self.set_rows(vec![], diff::Colors::default());
            self.project.changes_tree.select(path, cx);
        }
        let (root, base, path) = (self.project.root.clone(), self.project.base.clone(), path.to_string());
        self.project.diffing = Some(cx.spawn(async move |this, cx| {
            let language = syntax(&path);
            let (rows, colors) = cx
                .background_executor()
                .spawn(async move {
                    let rows = diff::file_diff(&root, &base, &change);
                    // Only files with changed lines are worth parsing.
                    let colors = if rows.is_empty() { diff::Colors::default() } else { diff::colors(&root, &base, &change, &language) };
                    (rows, colors)
                })
                .await;
            this.update(cx, |reader, cx| {
                if reader.project.picked.as_deref() == Some(&path) {
                    reader.set_rows(rows, colors);
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
    /// that nothing keeps updating it once another is shown. Its threads keep going, except a
    /// blank one, whose agent starts again when it is back on screen.
    fn suspend_project(&mut self) {
        if self.thread.blank && self.thread.client.is_some() {
            self.thread.sleep();
        }
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
            self.parked.retain(Thread::worth_keeping);
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
        match agent {
            Some(agent) => self.connect(&agent, false, window, cx),
            None => self.wake_thread(window, cx),
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
            let blank = self.views().iter().any(|v| v.id == pid && v.threads.iter().any(|t| t.id == tid && t.blank));
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

    /// The user's answer to a command Po asked about: remember it and start the agent, or leave it stopped.
    fn answer_approval(&mut self, ix: usize, n: usize, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(ix) else { return };
        let Some(Block::Approve { agent, new_agent, mcp, resume, answered }) = blocks.get_mut(n) else { return };
        if answered.is_some() {
            return;
        }
        *answered = Some(yes);
        let (agent, new_agent, mcp, resume) = (agent.clone(), *new_agent, mcp.clone(), *resume);
        self.thread.relist(ix);
        if yes {
            if new_agent {
                config::approve(&agent);
            }
            config::approve_mcp(&mcp);
            self.connect(&agent.id, resume, window, cx);
        } else if new_agent {
            self.thread.status = "not running".into();
        } else {
            // Only the servers were turned down: the agent runs without them.
            config::decline_mcp(&mcp);
            self.connect(&agent.id, resume, window, cx);
        }
        cx.notify();
    }

    /// The one way a request from outside the window changes it. Views are the caller's own; the
    /// viewer is the user's, so only the thread on screen may move it, and not while they are busy.
    fn run_command(&mut self, caller: command::Caller, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        use command::Command;
        let command::Caller::Thread(id) = caller;
        let reply = if command.moves_viewer() {
            self.with_thread(id, |reader, visible| {
                if !visible {
                    return Err("only the thread on screen can move the viewer".to_string());
                }
                let command = reader.check_move(command)?;
                if reader.last_input.elapsed() < QUIET {
                    reader.defer_move(id, command, window, cx);
                    return Ok(serde_json::json!({ "deferred": true }));
                }
                reader.pending_move = None;
                reader.as_agent(cx, |reader, cx| reader.apply_move(command, window, cx));
                Ok(serde_json::json!({}))
            })
            .unwrap_or_else(|| Err("that thread is gone".into()))
        } else {
            let shows = matches!(command, Command::ViewShow { .. });
            self.with_thread(id, |reader, visible| {
                let reply = reader.thread.views.apply(command)?;
                reader.sync_view_docs(cx);
                if let (true, Some(view)) = (shows, reply["id"].as_str()) {
                    let view = view.to_string();
                    if visible && reader.last_input.elapsed() >= QUIET {
                        reader.as_agent(cx, |reader, cx| {
                            reader.show_agent_view(&view, cx);
                            reader.follow_agent();
                        });
                    } else if reader.thread.shown.as_deref() != Some(&view) {
                        // The user is typing, or this is not the thread they are in: ask first.
                        reader.thread.offered = Some(view);
                    }
                }
                Ok(reply)
            })
            .unwrap_or_else(|| Err("that thread is gone".into()))
        };
        cx.notify();
        reply
    }

    /// Renders the thread's views again after they changed, and drops what belongs to views that are gone.
    fn sync_view_docs(&mut self, cx: &mut Context<Self>) {
        let live: HashSet<String> = self.thread.views.list().iter().map(|view| view.id.clone()).collect();
        self.thread.docs.retain(|id, _| live.contains(id));
        for view in self.thread.views.list().to_vec() {
            let chunks = Rc::new(ui::markdown::chunks(&view.body));
            let doc = Self::doc_for(self.thread.docs.remove(&view.id), chunks, cx);
            self.thread.docs.insert(view.id, doc);
        }
        for slot in [&mut self.thread.shown, &mut self.thread.offered] {
            if slot.as_ref().is_some_and(|id| !live.contains(id)) {
                *slot = None;
            }
        }
    }

    /// Puts the agent's view `id` in the viewer, opening it if it was closed.
    fn show_agent_view(&mut self, id: &str, cx: &mut Context<Self>) {
        self.thread.shown = Some(id.to_string());
        self.thread.offered = None;
        self.open_viewer_wide(cx);
        cx.notify();
    }

    /// The agent itself changed the layout, not the user: its pending move is not dropped for it.
    fn follow_agent(&mut self) {
        let (thread, now) = (self.thread.id, self.screen());
        if let Some(pending) = &mut self.pending_move {
            pending.follows_agent(thread, &now);
        }
    }

    /// `command` with its path made one inside the project, or why there is nothing to show.
    fn check_move(&self, command: command::Command) -> Result<command::Command, String> {
        use command::Command;
        let root = &self.project.root;
        Ok(match command {
            Command::FileOpen { path, line } => {
                let path = command::project_path(root, &path)?;
                let file = root.join(&path);
                if !file.is_file() {
                    return Err(format!("no file `{path}` in the project"));
                }
                // A link inside the project may lead out of it.
                let inside = file.canonicalize().and_then(|file| root.canonicalize().map(|root| file.starts_with(root))).unwrap_or(false);
                if !inside {
                    return Err(format!("`{path}` is not in the project"));
                }
                Command::FileOpen { path, line }
            }
            Command::DiffShow { path: Some(path) } => {
                let path = command::project_path(root, &path)?;
                if !self.project.changes.iter().any(|change| change.path == path) {
                    return Err(format!("`{path}` has no changes"));
                }
                Command::DiffShow { path: Some(path) }
            }
            other => other,
        })
    }

    fn apply_move(&mut self, command: command::Command, window: &mut Window, cx: &mut Context<Self>) {
        use command::Command;
        match command {
            Command::FileOpen { path, line } => {
                self.show_viewer(Mode::Source, cx);
                self.open(&path, window, cx);
                if let (Some(line), Some(code)) = (line, &self.project.code) {
                    code.update(cx, |code, cx| code.go_to_line(line - 1, cx));
                }
            }
            Command::DiffShow { path } => {
                self.show_viewer(Mode::Diff, cx);
                if let Some(path) = path {
                    self.pick(&path, cx);
                }
            }
            Command::ViewerClose => self.show_chat(cx),
            _ => {}
        }
    }

    fn panes(&self) -> Panes {
        Panes { viewer: self.side_open, agent: self.agent_open }
    }

    /// Runs `change`, which an agent asked for, noting what it did to the layout so the
    /// workspace goes on saving the user's.
    fn as_agent(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Self, &mut Context<Self>)) {
        let (panes, project, mode) = (self.panes(), self.project_id, self.mode);
        change(self, cx);
        if self.panes() != panes {
            self.agent_panes = Some(AgentChange::after(self.agent_panes, panes, self.panes()));
        }
        if self.mode != mode {
            let change = AgentChange::after(self.agent_modes.get(&project).copied(), mode, self.mode);
            self.agent_modes.insert(project, change);
        }
    }

    /// Forgets an agent's change once the layout is no longer what it left: the user moved on, and
    /// coming back to it by hand is their choice, not the agent's.
    fn settle_agent_changes(&mut self) {
        self.agent_panes = AgentChange::settled(self.agent_panes, self.panes());
        let mode = AgentChange::settled(self.agent_modes.get(&self.project_id).copied(), self.mode);
        match mode {
            Some(change) => self.agent_modes.insert(self.project_id, change),
            None => self.agent_modes.remove(&self.project_id),
        };
    }

    fn screen(&self) -> Screen {
        Screen {
            side_open: self.side_open,
            agent_open: self.agent_open,
            mode: self.mode,
            current: self.project.current.clone(),
            picked: self.project.picked.clone(),
            shown: self.thread.shown.clone(),
        }
    }

    /// Holds `command` from thread `thread` until the user has been still for [`QUIET`]. A later
    /// request replaces it; it is dropped if another thread comes on screen or the user moves the
    /// viewer first.
    fn defer_move(&mut self, thread: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) {
        let waiting = self.pending_move.is_some();
        self.pending_move = Some(PendingMove { thread, command, from: self.screen() });
        if waiting {
            return;
        }
        let mut wait = QUIET.saturating_sub(self.last_input.elapsed());
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(wait).await;
                let next = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.try_pending_move(window, cx)));
                match next {
                    Ok(Ok(Some(more))) => wait = more,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Applies the held move if the user has gone still, or says how much longer to wait.
    fn try_pending_move(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Duration> {
        let pending = self.pending_move.as_ref()?;
        if pending.thread != self.thread.id || pending.from != self.screen() {
            self.pending_move = None;
            return None;
        }
        let still = self.last_input.elapsed();
        if still < QUIET {
            return Some(QUIET - still);
        }
        let pending = self.pending_move.take()?;
        self.as_agent(cx, |reader, cx| reader.apply_move(pending.command, window, cx));
        cx.notify();
        None
    }

    /// Browse (files) or Review (changes) in the viewer pane. If the pane was closed it opens wide,
    /// since looking at code or a diff wants room; if it is open its width is left as it was.
    fn show_viewer(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.thread.shown = None;
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
        self.thread.shown = None;
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
        self.thread.options.clear();
        self.thread.applying = None;
        self.thread.loading = false;
        self.thread.picking = resume;
        // Listing sessions to resume is not a blank thread, even before one is picked.
        self.thread.blank &= !resume;
        self.thread.sessions = None;
        let agent = self.agents.iter().find(|agent| agent.id == key).cloned().unwrap_or_else(|| config::Agent {
            id: key.to_string(),
            name: key.to_string(),
            command: key.to_string(),
            daemon: None,
            vouched: false,
        });
        let new_agent = !config::trusted(&agent);
        // A dex daemon is not handed MCP servers.
        let mcp = if agent.daemon.is_none() { config::unapproved_mcp(&config::load()) } else { vec![] };
        if new_agent || !mcp.is_empty() {
            // Nothing is started until the user has seen the commands and said yes.
            self.thread.status = "waiting for approval".into();
            self.thread.messages.push(Msg::Agent(vec![Block::Approve { agent, new_agent, mcp, resume, answered: None }]));
            self.thread.relist(0);
            cx.notify();
            return;
        }
        self.thread.status = if resume { "listing sessions…" } else { "starting…" }.into();
        let control = self.control.as_ref().map(|control| control.register(id)).unwrap_or_default();
        match Client::spawn(&agent, &self.project.root, resume, &control) {
            Ok((client, mut events)) => {
                self.thread.client = Some(client);
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    // An agent that never answers the handshake would leave the thread on "starting…".
                    let watchdog = this.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(20)).await;
                        handle
                            .update(cx, |_, window, cx| {
                                watchdog.update(cx, |reader, cx| {
                                    reader.with_thread(id, |reader, _| {
                                        let thread = &reader.thread;
                                        if thread.generation == generation && !thread.ready && thread.client.is_some() {
                                            // What was typed goes back to the draft rather than wait for an agent that is gone.
                                            reader.return_pending();
                                            reader.restore_queue(window, cx);
                                            reader.thread.status = "not responding".into();
                                            reader.thread.fail("The agent did not answer the handshake. Check that its command speaks ACP.".into());
                                            reader.unstart();
                                        }
                                    });
                                    cx.notify();
                                })
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

    /// Opens a blank thread, ready for a message, with its agent starting so that its settings
    /// can be chosen first. The thread on screen keeps running in the background; if it is
    /// itself still blank it is used instead.
    fn new_blank_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread.blank {
            self.add_thread(window, cx);
        }
        self.start_blank(window, cx);
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

    /// Sets a new session's settings to what was last chosen for its agent, in the order of
    /// [`Thread::ordered`]. A first message waits until they are taken.
    fn apply_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.applying = Some(Applying::default());
        self.apply_next(window, cx);
    }

    /// Asks the agent for the next setting to carry over that it doesn't already have, judged
    /// by what it offers now, so that one it only offers after a change is carried over too.
    /// Once none are left, the first message goes.
    fn apply_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = &mut self.thread;
        let (Some(applying), Some(key), Some(client)) = (thread.applying.as_mut(), &thread.agent, &thread.client) else {
            thread.applying = None;
            return self.send_pending(window, cx);
        };
        let chosen = self.choices.get(key);
        let options: Vec<&ConfigOption> = thread.options.iter().filter(|option| !applying.settled.contains(&option.id)).collect();
        for option in Thread::ordered(options) {
            applying.settled.insert(option.id.clone());
            let Some(value) = chosen.and_then(|chosen| chosen.get(&option.id)) else { continue };
            if *value != option.current && option.choices.iter().any(|(choice, _)| choice == value) && client.set_option(&option.id, value) {
                applying.asked = Some(option.id.clone());
                applying.asks += 1;
                // An agent that never answers would hold the first message back for good.
                let (id, generation, asked, ask, handle) =
                    (thread.id, thread.generation, option.id.clone(), applying.asks, window.window_handle());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(20)).await;
                    handle
                        .update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                reader.with_thread(id, |reader, _| {
                                    let thread = &mut reader.thread;
                                    let waiting = thread.generation == generation
                                        && thread.applying.as_ref().is_some_and(|applying| applying.asks == ask && applying.asked.is_some());
                                    if waiting {
                                        log::warn!("agent: no answer to the setting {asked}");
                                        thread.unanswered.insert(asked);
                                        reader.apply_next(window, cx);
                                    }
                                });
                            })
                            .ok();
                        })
                        .ok();
                })
                .detach();
                return;
            }
        }
        thread.applying = None;
        self.send_pending(window, cx);
    }

    /// Sends the first message, once the agent is ready and has the settings it starts with.
    fn send_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread.ready || self.thread.applying.is_some() {
            return;
        }
        if let Some(prompt) = self.thread.pending.take()
            && !self.send_to_agent(&prompt)
        {
            self.thread.pending = Some(prompt);
            self.return_pending();
            self.thread.fail("The agent has no session yet.".into());
            self.restore_queue(window, cx);
        }
    }

    /// The agent answered a change of setting `id`. If it was the one being carried over, the
    /// next follows; true then, as the choice it carried over is already remembered.
    fn took_setting(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let carried = self.thread.applying.as_ref().is_some_and(|applying| applying.asked.as_deref() == Some(id));
        if carried {
            self.apply_next(window, cx);
        }
        // One given up on was asked for too, so it isn't the user's choice either.
        carried || self.thread.unanswered.remove(id)
    }

    /// Chooses the agent new threads start with. A blank thread on screen switches to it.
    fn pick_agent(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chosen_agent = Some(key.clone());
        if self.thread.blank && self.thread.agent.as_ref() != Some(&key) {
            log::info!("agent: blank thread switches to {key}");
            self.connect(&key, false, window, cx);
        }
        cx.notify();
    }

    /// Asks the agent to change a setting of the thread's session. Once it accepts, the choice is
    /// remembered for the agent's next threads.
    fn choose_option(&mut self, id: &str, value: &str) {
        let thread = &mut self.thread;
        if let Some(client) = &thread.client
            && client.set_option(id, value)
            && let Some(applying) = &mut thread.applying
        {
            // What is chosen here isn't then undone by carrying over an earlier choice.
            applying.settled.insert(id.to_string());
        }
    }

    /// Lists an agent's sessions in a new thread, to resume one: those of the agent the thread on
    /// screen is running, or of the chosen one if it has not started.
    fn resume_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else { return };
        log::info!("agent: resuming with {key}");
        self.add_thread(window, cx);
        self.connect(&key, true, window, cx);
    }

    /// Starts the blank thread on screen with `prompt`, the draft, as its first message: in
    /// the session its agent already has open, or in a worktree of its own if that was asked
    /// for. `later` is as for [`Self::send`].
    fn start_thread(&mut self, prompt: String, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else {
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
        log::info!("agent: new thread with {key}");
        // An agent that failed or was stopped starts again, without the error it left.
        if self.thread.client.is_none() {
            self.connect(&key, false, window, cx);
        }
        // One that couldn't be started has said why, and the next message tries again.
        if self.thread.client.is_none() {
            return;
        }
        self.thread.blank = false;
        self.send(later, window, cx);
        self.request_save(cx);
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
            active_at: now_ms(),
            pending: None,
            isolate: false,
            blank: true,
            applying: None,
            unanswered: HashSet::new(),
            _enter: Self::send_on_enter(&draft, window, cx),
            messages: vec![],
            busy: false,
            queue: vec![],
            ready: false,
            options: Vec::new(),
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
            views: Default::default(),
            docs: HashMap::new(),
            shown: None,
            offered: None,
        }
    }

    /// Puts a blank thread on screen, parking the one that was, unless that is still blank.
    fn add_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = self.blank_thread(window, cx);
        let old = std::mem::replace(&mut self.thread, fresh);
        // A blank thread, or a session listing nothing was picked from, isn't kept.
        if old.worth_keeping() {
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
            self.parked.retain(Thread::worth_keeping);
            self.thread.unread = false;
            self.wake_thread(window, cx);
            self.request_save(cx);
            cx.notify();
        }
    }

    /// Starts the agent of the thread on screen again if it was stopped, resuming its session.
    /// A blank thread's agent starts afresh, unless it failed: that shows why until a message
    /// or a new thread asks for it again.
    fn wake_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.blank {
            if self.thread.messages.is_empty() {
                self.start_blank(window, cx);
            }
            return;
        }
        if !self.thread.asleep() {
            return;
        }
        let (Some(key), Some(session)) = (self.thread.agent.clone(), self.thread.session.clone()) else { return };
        log::info!("agent: waking {key} session {session}");
        self.connect(&key, true, window, cx);
        self.thread.restore = Some(session);
        // An agent waiting for approval hasn't started, and says so.
        if self.thread.client.is_some() {
            self.thread.status = "waking…".into();
        }
    }

    /// Starts the agent of the blank thread on screen if it has none running: its own agent, or
    /// else the chosen one.
    fn start_blank(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.blank
            && self.thread.client.is_none()
            && let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone()))
        {
            self.connect(&key, false, window, cx);
        }
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
        thread.blank = false;
        thread.active_at = saved.active_at;
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
        self.tree_hidden = saved.explorer_collapsed;
        self.tree_cols.update(cx, |cols, _| cols.set_widths(saved.explorer_width.map(px), None));
        self.chosen_agent = saved.agent.clone().or(self.chosen_agent.take());
        self.sections.update(cx, |sections, _| {
            for (section, height) in [(LIVE, saved.live_split), (PROJECTS, saved.split)] {
                if let Some(height) = height {
                    sections.set_size(section, px(height));
                }
            }
            // A section is never folded if it is the last open one, so a layout that folds both,
            // which is never saved, keeps the last open.
            for (section, folded) in [(LIVE, saved.live_folded), (PROJECTS, saved.projects_folded)] {
                sections.set_folded(section, folded);
            }
        });
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
            // Files saved before the time was kept list the threads oldest first.
            let mut threads: Vec<Thread> = state
                .threads
                .into_iter()
                .enumerate()
                .map(|(ix, mut t)| {
                    if t.active_at == 0 {
                        t.active_at = ix as u64 + 1;
                    }
                    self.sleeping_thread(t, window, cx)
                })
                .collect();
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
                let mut threads: Vec<&Thread> = view.threads.into_iter().filter(|t| !t.blank && t.agent.is_some() && t.session.is_some()).collect();
                threads.sort_by_key(|t| t.id);
                workspace::ProjectState {
                    root: view.project.root.clone(),
                    review: AgentChange::saved(self.agent_modes.get(&view.id), view.mode) == Mode::Diff,
                    worktree: view.project.worktree.as_ref().map(|wt| wt.spec.clone()),
                    active: threads.iter().position(|t| t.id == view.shown).unwrap_or(0),
                    threads: threads
                        .into_iter()
                        .filter_map(|t| {
                            Some(workspace::ThreadState { agent: t.agent.clone()?, session: t.session.clone()?, title: t.title(), active_at: t.active_at })
                        })
                        .collect(),
                }
            })
            .collect();
        let panes = AgentChange::saved(self.agent_panes.as_ref(), self.panes());
        workspace::Workspace { projects, viewer_open: panes.viewer, agent_open: panes.agent, swapped: self.swapped, agent: self.chosen_agent.clone(), split: self.sections.read(cx).size(PROJECTS).map(f32::from), explorer_collapsed: self.tree_hidden, explorer_width: Some(self.tree_cols.read(cx).widths().0.into()), live_split: self.sections.read(cx).size(LIVE).map(f32::from), live_folded: self.sections.read(cx).folded(LIVE), projects_folded: self.sections.read(cx).folded(PROJECTS), left_width: Some(self.sidebars.read(cx).widths().0.into()), right_width: self.sidebars.read(cx).widths().1.map(f32::from) }
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
        // Blank threads lose nothing by stopping, so they go first.
        idle.sort_by_key(|t| (!t.blank, t.touched));
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
        // Only the agent's work counts as activity, not a session opening or its settings, and
        // not a resumed session replaying its history.
        if !self.thread.loading && matches!(event, Event::Update(_) | Event::Permission { .. } | Event::Stopped(_) | Event::Accepted { .. }) {
            self.thread.active_at = now_ms();
        }
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
            Event::Options(options) => {
                self.thread.options = options;
            }
            Event::OptionSet { id, value } => {
                if !self.took_setting(&id, window, cx)
                    && let Some(key) = self.thread.agent.clone()
                {
                    self.choices.entry(key.clone()).or_default().insert(id.clone(), value.clone());
                    cx.background_spawn(async move { config::save_choice(&key, &id, &value) }).detach();
                }
            }
            Event::Refused { id, text } => {
                self.thread.status = text;
                self.took_setting(&id, window, cx);
            }
            Event::Ready { agent, fresh } => {
                self.thread.ready = true;
                self.thread.loading = false;
                self.thread.picking = false;
                log::info!("agent: {agent} ready");
                self.thread.status = "ready".into();
                self.thread.session = self.thread.client.as_ref().and_then(|client| client.session());
                self.request_save(cx);
                if fresh {
                    self.apply_choices(window, cx);
                } else {
                    self.send_pending(window, cx);
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
                // The order of the thread list is saved with the time the turn ended.
                self.request_save(cx);
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
                self.thread.applying = None;
                self.unstart();
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
                self.thread.applying = None;
                self.unstart();
            }
        }
        self.thread.relist(changed);
        self.thread.list.scroll_to_end();
        cx.notify();
    }



    /// After its agent failed or exited: a blank thread, or one whose first message never got
    /// a session, starts its agent again when it is next asked for. That includes one whose
    /// agent is still held but never became ready, as it would never answer.
    fn unstart(&mut self) {
        let thread = &mut self.thread;
        if thread.blank || thread.unstarted() || thread.session.is_none() && !thread.ready && !thread.picking && !thread.loading {
            thread.drop_agent();
        }
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
        self.thread.shown = None;
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
        let doc = self.project.doc.take().filter(|_| self.project.current.as_deref() == Some(path));
        Some(Self::doc_for(doc, chunks, cx))
    }

    /// `doc` showing `chunks`, or a new one: reading the same document again keeps its scroll
    /// position and its selection, as far as they still make sense.
    fn doc_for(doc: Option<Doc>, chunks: Rc<Vec<String>>, cx: &mut Context<Self>) -> Doc {
        match doc {
            Some(doc) if *doc.chunks == *chunks => doc,
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
                Doc { chunks, ..doc }
            }
            None => Doc {
                list: ListState::new(chunks.len(), ListAlignment::Top, px(400.)),
                chunks,
                selection: cx.new(|_| ui::markdown::Selection::default()),
                focus: cx.focus_handle(),
            },
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
        self.project.files_tree.select(path, cx);
        cx.notify();
    }

    /// Sends the draft: as a new turn, or, while the agent is working, into the queue
    /// (`later` for after the turn rather than into it).
    fn send(&mut self, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.thread.draft.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        if self.thread.blank || self.thread.unstarted() {
            return self.start_thread(prompt, later, window, cx);
        }
        let thread = &self.thread;
        let starting = thread.client.is_some()
            && (thread.applying.is_some() || !thread.ready && !thread.loading && !thread.picking && thread.restore.is_none());
        if starting && !self.thread.busy {
            // The agent is still starting, or taking its settings: the message waits for it,
            // shown as sent.
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
        self.thread.blank = false;
        self.thread.busy = true;
        self.thread.touched = Instant::now();
        self.thread.active_at = now_ms();
        // The message that was last stops being so: it loses its bottom padding and folds its
        // finished runs of tool calls, so it is laid out again too.
        let was_last = self.thread.messages.len().saturating_sub(1);
        self.thread.messages.push(Msg::User(prompt.to_string()));
        self.thread.messages.push(Msg::Agent(vec![]));
        self.thread.relist(was_last);
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

    /// Folds or unfolds the run of tool calls that starts at `block`.
    fn fold_run(&mut self, msg: usize, block: usize, folded: bool, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Tool(tool)) = blocks.get_mut(block) else { return };
        tool.fold = Some(folded);
        self.thread.relist(msg);
        cx.notify();
    }

    fn toggle_thought(&mut self, msg: usize, block: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Thought(_, open)) = blocks.get_mut(block) else { return };
        *open = !*open;
        self.thread.relist(msg);
        cx.notify();
    }

    /// Opens every tool call's details, or closes them all when none is left to open. Runs folded
    /// to a line count as something left to open: they are unfolded, details and all.
    fn toggle_tools(&mut self, cx: &mut Context<Self>) {
        let count = self.thread.messages.len();
        let mut from = None;
        for (ix, msg) in self.thread.messages.iter_mut().enumerate() {
            let Msg::Agent(blocks) = msg else { continue };
            for start in folded_runs(blocks, ix + 1 == count) {
                let Some(Block::Tool(tool)) = blocks.get_mut(start) else { continue };
                tool.fold = Some(false);
                from.get_or_insert(ix);
            }
        }
        if let Some(from) = from {
            for msg in &mut self.thread.messages {
                let Msg::Agent(blocks) = msg else { continue };
                for block in blocks {
                    if let Block::Tool(tool) = block {
                        tool.open = true;
                    }
                }
            }
            self.thread.relist(from);
            cx.notify();
            return;
        }
        if let Some(from) = toggle_all(&mut self.thread.messages, |b| if let Block::Tool(tool) = b { Some(&mut tool.open) } else { None }) {
            self.thread.relist(from);
            cx.notify();
        }
    }

    /// Opens every thought, or folds them all when none is left to open.
    fn toggle_thoughts(&mut self, cx: &mut Context<Self>) {
        if let Some(from) = toggle_all(&mut self.thread.messages, |b| if let Block::Thought(_, shown) = b { Some(shown) } else { None }) {
            self.thread.relist(from);
            cx.notify();
        }
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

    /// The file tree beside the viewer: every file, or the changed ones in Changes mode, under a
    /// header with the mode switch. `lead` is the window's own control, when this is the
    /// leftmost pane.
    fn explorer(&self, lead: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.global::<Palette>();
        let this = cx.entity();
        let mode = self.mode;
        let (source, changes, hide) = (cx.entity(), cx.entity(), cx.entity());
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
            )
            .child(
                Button::new("hide-tree")
                    .icon(IconName::PanelLeftClose)
                    .ghost()
                    .small()
                    .tooltip("Hide the file tree")
                    .on_click(move |_, _, cx| hide.update(cx, |reader, cx| reader.set_tree_hidden(true, cx))),
            );
        let title = match mode {
            Mode::Source => "Explorer",
            Mode::Diff => "Changes",
        };
        let state = &if mode == Mode::Diff { &self.project.changes_tree } else { &self.project.files_tree }.state;
        let marks = if mode == Mode::Diff { self.project.marks.clone() } else { Rc::default() };
        let (muted, danger) = (colors.muted_foreground, colors.danger);
        let kinds = [colors.primary, colors.warning, colors.info, colors.added];
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
                        FileKind::Other => muted.opacity(theme::tokens::MUTED),
                    };
                    div().size(rems(0.5)).flex_none().rounded(theme::tokens::RADIUS_MARK).bg(tint).into_any_element()
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
                            // A file picked in the tree is what the viewer shows, not an agent's view.
                            reader.thread.shown = None;
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
        let foreground = colors.foreground;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header_row(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .children(lead)
                    .child(div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(foreground).child(title.to_string())),
                modes,
                cx,
            ))
            .children(self.worktree_strip(cx))
            .children((mode == Mode::Diff).then(|| self.pr_strip(cx)))
            .child(div().flex_1().min_h_0().p_1().child(body))
            .into_any_element()
    }

    /// Hides the file tree beside the viewer, or shows it again.
    fn set_tree_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.tree_hidden = hidden;
        self.request_save(cx);
        cx.notify();
    }

    /// The left side: the live agents, and every project with its threads.
    fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Palette>();
        let (hover, selected, muted, foreground) = (colors.list_hover, colors.list_selected, colors.muted_foreground, colors.foreground);
        let views = self.views();
        let (active_project, active_thread) = (self.project_id, self.thread.id);
        let project_count = views.len();
        let many = project_count > 1;
        let add = cx.entity();
        // Groups are spaced apart by more than anything inside one, so the eye reads them as units.
        let mut tree = div().id("agents-tree").size_full().overflow_y_scroll().flex().flex_col().gap_6().px_2().pt_1().pb_3();
        // Folded, only the header shows, so no project is built.
        let projects_folded = self.sections.read(cx).folded(PROJECTS);
        for view in views.iter().filter(|_| !projects_folded) {
            let pid = view.id;
            let (select, close, start) = (cx.entity(), cx.entity(), cx.entity());
            // A project and its threads are one group, set apart from the next by space, not a box.
            // The one on screen has its name at full strength, the rest are muted; the selected
            // thread is the only filled row. Names and thread titles share one left edge, after a column for the
            // folder or status mark.
            let label = if pid == active_project { foreground } else { muted };
            let mut group = div().flex().flex_col().gap_px();
            group = group.child(
                div()
                    .id(("agent-project", pid as usize))
                    .group(row_group("agent-project", pid))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1p5()
                    .h(theme::tokens::ROW_HEIGHT)
                    .mb_0p5()
                    .rounded(colors.radius)
                    .cursor_pointer()
                    .hover(move |row| row.bg(hover))
                    .on_click(move |_, window, cx| select.update(cx, |reader, cx| reader.show_project(pid, window, cx)))
                    .child(
                        div()
                            .w(rems(1.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(Icon::new(IconName::Folder).size(IconSize::XSmall).color(label)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(rems(4.))
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(label)
                            .child(view.project.name()),
                    )
                    .when_some(self.heads.get(&view.project.root).filter(|_| view.project.shows_branch()), |row, head| {
                        row.child(div().min_w_0().max_w(rems(7.)).flex().items_center().gap_1().text_xs().text_color(muted).child(
                            div().truncate().child(format!("⎇ {}", head.name)),
                        ).when(head.dirty, |tag| tag.child(div().flex_none().child("●"))))
                    })
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
            // Most recently active first. Past `THREADS_SHOWN` the rest fold away, except the
            // thread on screen, which stays in sight.
            let mut recent: Vec<&Thread> = view.threads.iter().copied().filter(|t| !t.blank || t.id == view.shown).collect();
            recent.sort_by_key(|t| (std::cmp::Reverse(t.active_at), std::cmp::Reverse(t.id)));
            let expanded = self.threads_expanded.contains(&pid);
            let foldable = recent.len() > THREADS_SHOWN;
            let hidden = if expanded || !foldable {
                0
            } else {
                let mut keep: Vec<&Thread> = recent[..THREADS_SHOWN].to_vec();
                if !keep.iter().any(|t| t.id == view.shown) {
                    if let Some(shown) = recent.iter().find(|t| t.id == view.shown) {
                        keep[THREADS_SHOWN - 1] = shown;
                    }
                }
                let hidden = recent.len() - THREADS_SHOWN;
                recent = keep;
                hidden
            };
            for thread in recent {
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
                        .h(theme::tokens::ROW_HEIGHT)
                        .rounded(colors.radius)
                        .cursor_pointer()
                        .when((pid, tid) == (active_project, active_thread), |row| row.bg(selected))
                        .when((pid, tid) != (active_project, active_thread), |row| row.hover(move |row| row.bg(hover)))
                        .on_click(move |_, window, cx| select.update(cx, |reader, cx| reader.open_thread(pid, tid, window, cx)))
                        .child(div().w(rems(1.)).flex_none().flex().justify_center().text_xs().text_color(tint).child(glyph))
                        .child(
                            // The project is a quiet label; the threads are what you pick, so they
                            // carry the weight.
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(if (pid, tid) == (active_project, active_thread) { foreground } else { foreground.opacity(theme::tokens::GUIDE) })
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
            if foldable && (hidden > 0 || expanded) {
                let toggle = cx.entity();
                let label = if expanded { "Show less".to_string() } else { format!("{hidden} more") };
                group = group.child(
                    div()
                        .id(("agent-more", pid as usize))
                        .flex()
                        .items_center()
                        .px_1p5()
                        .h(theme::tokens::ROW_HEIGHT)
                        .pl(rems(1.75))
                        .rounded(colors.radius)
                        .cursor_pointer()
                        .text_xs()
                        .text_color(muted)
                        .hover(move |row| row.bg(hover))
                        .on_click(move |_, _, cx| {
                            toggle.update(cx, |reader, cx| {
                                if !reader.threads_expanded.insert(pid) {
                                    reader.threads_expanded.remove(&pid);
                                }
                                cx.notify();
                            })
                        })
                        .child(label),
                );
            }
            tree = tree.child(group);
        }
        let title = self.fold_title(
            PROJECTS,
            div()
                .flex()
                .items_center()
                .gap_2()
                .child("PROJECTS")
                .child(count_badge(project_count, muted)),
            cx,
        );
        let projects = div()
            .size_full()
            .flex()
            .flex_col()
            // The same kind of header as the explorer's below: a title, and what you can do here.
            .child(header_row(
                title,
                Button::new("agents-add")
                    .icon(IconName::Plus)
                    .tooltip("Add a project: open a folder (Ctrl+Shift+O)")
                    .ghost()
                    .small()
                    .on_click(move |_, window, cx| add.update(cx, |reader, cx| reader.pick_project(window, cx))),
                cx,
            ).border_b_0())
            .children(self.worktree_status.clone().filter(|_| !projects_folded).map(|text| {
                div().px_3().py_1().text_xs().text_color(muted).max_h(rems(5.)).overflow_hidden().child(text)
            }))
            .child(div().flex_1().min_h_0().child(tree));
        let (live, any_live) = self.live_section(&views, cx);
        let colors = cx.global::<Palette>();
        // Live agents sit above everything, in a pane of their own whose height is dragged, so
        // that the projects below don't move when an agent starts or stops. With none, it is
        // only its header and a line.
        let pane = div().size_full().bg(colors.sidebar).child(
            stack(&self.sections).fixed(LIVE, (!any_live).then_some(rems(LIVE_EMPTY_REMS))).section(live).section(projects),
        );
        floating(pane, cx)
    }

    /// The threads that are asking, finished unseen or running, plus any agent still alive and
    /// waiting, across every project, with a line of what each is doing. Jumping to one is a click.
    /// Also whether there are any, since the pane folds to its header when there are none.
    fn live_section(&self, views: &[View<'_>], cx: &mut Context<Self>) -> (AnyElement, bool) {
        let colors = cx.global::<Palette>();
        let (hover, selected, muted, foreground, radius) =
            (colors.list_hover, colors.list_selected, colors.muted_foreground, colors.foreground, colors.radius);
        let mut live: Vec<(u8, &View<'_>, &Thread)> = views
            .iter()
            .flat_map(|view| view.threads.iter().map(move |thread| (view, *thread)))
            .filter_map(|(view, thread)| Some((thread.activity().live_rank().or(thread.client.is_some().then_some(IDLE_RANK))?, view, thread)))
            .collect();
        live.sort_by_key(|(rank, _, thread)| (*rank, thread.id));
        let count = live.len();
        let folded = self.sections.read(cx).folded(LIVE);
        // Folded, the most pressing thread's mark stays in view.
        let (glyph, tint) = if folded { live.first().map_or(("", muted), |(_, _, thread)| thread.activity().mark(colors)) } else { ("", muted) };
        let (active_project, active_thread) = (self.project_id, self.thread.id);
        let mut rows = div().id("live-list").size_full().overflow_y_scroll().flex().flex_col().gap_0p5().px_2().pt_1().pb_2();
        if live.is_empty() && !folded {
            rows = rows.child(div().px_2().py_1().text_xs().text_color(muted).child("No active agents"));
        }
        for (_, view, thread) in live.into_iter().filter(|_| !folded) {
            let (pid, tid) = (view.id, thread.id);
            let (glyph, tint) = thread.activity().mark(colors);
            let select = cx.entity();
            let preview = thread.preview();
            let detail = if preview.is_empty() { view.project.name() } else { format!("{} · {preview}", view.project.name()) };
            let current = (pid, tid) == (active_project, active_thread);
            rows = rows.child(
                div()
                    .id(("live-thread", tid as usize))
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .cursor_pointer()
                    .when(current, |row| row.bg(selected))
                    .hover(move |row| row.bg(hover))
                    .on_click(move |_, window, cx| select.update(cx, |reader, cx| reader.open_thread(pid, tid, window, cx)))
                    .child(div().w(rems(1.)).flex_none().flex().justify_center().text_sm().text_color(tint).child(glyph))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_sm().text_color(foreground).child(thread.title()))
                            .child(div().truncate().text_xs().text_color(muted.opacity(theme::tokens::GUIDE)).child(detail)),
                    ),
            );
        }
        let title = self.fold_title(
            LIVE,
            div().flex().items_center().gap_2().child("LIVE").child(count_badge(count, muted)),
            cx,
        );
        let section = div()
            .size_full()
            .flex()
            .flex_col()
            .child(header_row(
                title,
                div().text_sm().text_color(tint).child(glyph),
                cx,
            ).border_b_0())
            .child(div().flex_1().min_h_0().child(rows))
            .into_any_element();
        (section, count > 0)
    }

    /// A section's title with a chevron, which folds it down to its header or opens it again.
    fn fold_title(&self, section: usize, label: impl IntoElement, cx: &App) -> AnyElement {
        let folded = self.sections.read(cx).folded(section);
        let (sections, muted) = (self.sections.clone(), cx.global::<Palette>().muted_foreground);
        div()
            .id(("section-fold", section))
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .cursor_pointer()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(muted)
            .on_click(move |_, _, cx| {
                sections.update(cx, |sections, cx| {
                    sections.toggle(section);
                    cx.notify();
                })
            })
            .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).size(IconSize::XSmall).color(muted))
            .child(label)
            .into_any_element()
    }

    /// The file tree is drawn beside the viewer: not hidden, and the pane wide enough for both.
    fn tree_visible(&self) -> bool {
        !self.tree_hidden && !self.viewer_narrow.get()
    }

    fn pick_hint(&self, what: &str) -> String {
        pick_hint(what, self.tree_visible(), self.tree_hidden)
    }

    /// The viewer or the diff viewer, as the project's mode says, with the file tree beside it
    /// unless that is hidden or the agent's own view is shown, which has no files to pick.
    fn pane(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let tabs = self.view_tabs(cx);
        let has_tabs = tabs.is_some();
        let shown = self.thread.shown.as_deref().and_then(|id| {
            let view = self.thread.views.list().iter().find(|view| view.id == id)?;
            Some((view, self.thread.docs.get(id)?))
        });
        let with_tree = shown.is_none() && self.tree_visible();
        // The window's own control goes in the leftmost header: the tree's, or the viewer's when
        // the tree is away, where a button brings it back.
        let (tree_lead, body_lead) = if with_tree {
            (lead, None)
        } else if shown.is_none() && self.tree_hidden {
            let show = cx.entity();
            let button = Button::new("show-tree")
                .icon(IconName::PanelLeftOpen)
                .ghost()
                .small()
                .tooltip("Show the file tree")
                .on_click(move |_, _, cx| show.update(cx, |reader, cx| reader.set_tree_hidden(false, cx)))
                .into_any_element();
            (None, Some(div().flex().items_center().gap_1().children(lead).child(button).into_any_element()))
        } else {
            (None, lead)
        };
        let body = match (shown, self.mode) {
            (Some((view, doc)), _) => self.agent_view(&view.title, doc, body_lead, trail, cx).into_any_element(),
            (None, Mode::Source) => self.viewer(body_lead, trail, cx).into_any_element(),
            (None, Mode::Diff) => self.diff_viewer(body_lead, trail, cx).into_any_element(),
        };
        let background = cx.global::<Palette>().background;
        let body = if with_tree {
            // The viewers paint their own background; the column paints its own too, unless the
            // pane does for both, which would lay a second translucent layer under glass.
            let column = div().size_full().when(!has_tabs, |d| d.bg(background)).child(self.explorer(tree_lead, cx));
            sidebars(&self.tree_cols).id("tree").left(true, TREE_RANGE, column).center(div().size_full().child(body)).into_any_element()
        } else {
            body
        };
        // Measures the pane, which it is drawn at whatever the tree is doing, and draws again
        // when that changes whether the tree fits.
        let narrow = self.viewer_narrow.clone();
        let measure = gpui::canvas(
            move |bounds, window, _| {
                let now = !tree_fits(bounds.size.width.into());
                if narrow.replace(now) != now {
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        floating(div().size_full().flex().flex_col().relative().when(has_tabs, |d| d.bg(background)).child(measure).children(tabs).child(div().flex_1().min_h_0().child(body)), cx)
    }

    /// A view the agent made, as a markdown document.
    fn agent_view(&self, title: &str, doc: &Doc, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let background = cx.global::<Palette>().background;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(pane_header(title, status_text("from the agent".into(), cx), lead, trail, cx))
            .child(div().flex_1().min_h_0().child(self.markdown_view(doc, cx)))
    }

    /// Files (or changes) and the agent's views as tabs, with a prompt for one it added while
    /// the user was busy. None while the agent has made no views.
    fn view_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.thread.views.is_empty() {
            return None;
        }
        let colors = cx.global::<Palette>();
        let border = colors.border;
        let this = cx.entity();
        let shown = self.thread.shown.as_deref();
        let home = {
            let this = this.clone();
            Button::new("tab-files")
                .label(if self.mode == Mode::Diff { "Changes" } else { "Files" })
                .ghost()
                .small()
                .selected(shown.is_none())
                .on_click(move |_, _, cx| this.update(cx, |reader, cx| {
                    reader.thread.shown = None;
                    cx.notify();
                }))
        };
        let tabs = self.thread.views.list().iter().map(|view| {
            let (open, close, id) = (this.clone(), this.clone(), view.id.clone());
            let closing = id.clone();
            div()
                .flex()
                .items_center()
                .child(
                    Button::new(SharedString::from(format!("tab-{}", view.id)))
                        .label(view.title.clone())
                        .ghost()
                        .small()
                        .selected(shown == Some(view.id.as_str()))
                        .on_click(move |_, _, cx| open.update(cx, |reader, cx| reader.show_agent_view(&id, cx))),
                )
                .child(
                    Button::new(SharedString::from(format!("close-{}", view.id)))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip("Close this view")
                        .on_click(move |_, _, cx| close.update(cx, |reader, cx| reader.close_agent_view(&closing, cx))),
                )
        });
        let offer = self.thread.offered.as_ref().and_then(|id| {
            let view = self.thread.views.list().iter().find(|view| view.id == *id)?;
            let (this, id) = (this.clone(), id.clone());
            Some(
                Button::new("offered-view")
                    .label(format!("The agent wants to show “{}”", view.title))
                    .primary()
                    .small()
                    .on_click(move |_, _, cx| this.update(cx, |reader, cx| reader.show_agent_view(&id, cx))),
            )
        });
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(border)
                .child(home)
                .children(tabs)
                .children(offer)
                .into_any_element(),
        )
    }

    fn close_agent_view(&mut self, id: &str, cx: &mut Context<Self>) {
        self.thread.views.apply(command::Command::ViewClose { id: id.to_string() }).ok();
        self.sync_view_docs(cx);
        cx.notify();
    }

    fn diff_viewer(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let colors = cx.global::<Palette>();
        let Some(path) = self.project.picked.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .bg(colors.background)
                .child(pane_header("Changes", status_text(String::new(), cx), lead, trail, cx))
                .child(div().flex_1().min_h_0().child(centered(self.pick_hint("a changed file"), cx)));
        };
        let (rows, pairs, syntax) = (self.project.rows.clone(), self.project.pairs.clone(), self.project.colors.clone());
        let palettes = syntax.palettes(cx);
        let (added, removed) = self.project.counts;
        let (success, danger, muted, border) =
            (colors.added, colors.danger, colors.muted_foreground, colors.border);
        let font = cx.global::<Palette>().mono_font_family.clone();
        let split = self.split;
        // Only the rows in view are built, so a long diff costs no more than a short one.
        let body: AnyElement = if rows.is_empty() && self.project.diffing.is_some() {
            div().into_any_element()
        } else if rows.is_empty() {
            centered("No textual changes".into(), cx)
        } else {
            let code = |text: &str, styles: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)>| {
                StyledText::new(SharedString::from(text.to_string())).with_highlights(styles)
            };
            list(self.project.diff_list.clone(), move |ix, _, _| {
                let line = div().flex().w_full().min_h(rems(1.5)).items_start().font_family(font.clone()).text_sm();
                if split {
                    let cell = |cell: &Option<diff::Cell>, first: bool| {
                        let half = div().flex_1().min_w_0().self_stretch().flex().items_start().gap_2();
                        let half = if first { half.border_r_1().border_color(border) } else { half };
                        let Some(cell) = cell else { return half.bg(muted.opacity(theme::tokens::TINT_FAINT)) };
                        let tint = match cell.kind {
                            Kind::Added => Some(success.opacity(theme::tokens::TINT)),
                            Kind::Removed => Some(danger.opacity(theme::tokens::TINT)),
                            _ => None,
                        };
                        let half = match tint {
                            Some(tint) => half.bg(tint),
                            None => half,
                        };
                        let styles = syntax.line(first, cell.number, &cell.text, &palettes);
                        half.child(div().w(rems(3.)).flex_none().text_right().text_color(muted).child(cell.number.to_string()))
                            .child(div().flex_1().min_w_0().child(code(&cell.text, styles)))
                    };
                    return match &pairs[ix] {
                        Pair::Hunk(text) => line.px_2().bg(muted.opacity(theme::tokens::TINT_SUBTLE)).text_color(muted).child(text.clone()),
                        Pair::Line(old, new) => line.child(cell(old, true)).child(cell(new, false)),
                    }
                    .into_any_element();
                }
                let row = &rows[ix];
                let number = |n: Option<u32>| {
                    div().w(rems(3.)).flex_none().text_right().text_color(muted).child(n.map_or(String::new(), |n| n.to_string()))
                };
                let (tint, sign) = match row.kind {
                    Kind::Added => (Some(success.opacity(theme::tokens::TINT)), "+"),
                    Kind::Removed => (Some(danger.opacity(theme::tokens::TINT)), "-"),
                    Kind::Hunk => (Some(muted.opacity(theme::tokens::TINT_SUBTLE)), ""),
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
                // Removed lines read as the old version had them, the rest as the new one does.
                let styles = match (row.kind, row.old, row.new) {
                    (Kind::Removed, Some(n), _) => syntax.line(true, n, &row.text, &palettes),
                    (_, _, Some(n)) => syntax.line(false, n, &row.text, &palettes),
                    _ => vec![],
                };
                line.child(number(row.old))
                    .child(number(row.new))
                    .child(div().w(rems(0.75)).flex_none().text_color(muted).child(sign))
                    .child(div().flex_1().min_w_0().child(code(&row.text, styles)))
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

    fn viewer(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let background = cx.global::<Palette>().background;
        let title = self.project.current.clone().unwrap_or_else(|| "No file open".into());
        let doc = self.project.doc.as_ref().filter(|_| self.rendered);
        let body: AnyElement = match (&self.project.code, &self.project.notice) {
            (_, Some(notice)) => centered(notice.clone(), cx),
            (_, None) if let Some(doc) = doc => self.markdown_view(doc, cx),
            // Cached: the viewer redraws when it changes, not with every agent update or caret blink elsewhere.
            (Some(code), None) => code.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            (None, None) => centered(self.pick_hint("a file"), cx),
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
        // No background of its own: the viewer around it paints one.
        div()
            .id("markdown-file")
            .key_context("Markdown")
            .track_focus(&focus)
            .size_full()
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

    /// A menu for each of the thread's session settings: the model first, then the agent's
    /// others in the order it gave them.
    fn setting_menus(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let this = cx.entity();
        let (busy, id) = (self.thread.busy, self.thread.id);
        Thread::ordered(self.thread.options.iter().collect())
            .into_iter()
            .map(|option| {
                let key = SharedString::from(format!("option-{id}-{}", option.id));
                let (this, setting) = (this.clone(), option.clone());
                let button = Button::new(key.clone()).label(option.current_name().to_string()).ghost().small().dropdown_caret(true);
                if busy {
                    // Agents differ on changing settings mid-turn, so it waits for the turn to end.
                    return button.tooltip(format!("{}: available when the agent is idle", option.name)).into_any_element();
                }
                dropdown(key, button.tooltip(option.name.clone()), move || {
                    setting
                        .choices
                        .iter()
                        .map(|(value, name)| {
                            let (this, setting_id, value) = (this.clone(), setting.id.clone(), value.clone());
                            let label = if *value == setting.current { format!("{}{name}", CHECKED) } else { name.clone() };
                            MenuItem::new(label, move |_, cx| this.update(cx, |reader, _| reader.choose_option(&setting_id, &value)))
                        })
                        .collect()
                })
                .into_any_element()
            })
            .collect()
    }

    fn agent_panel(&self, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let this = cx.entity();
        self.thread.selection.update(cx, |s, _| s.begin_frame());
        // Only the messages in view are built and laid out, however long the thread gets.
        let selection = self.thread.selection.clone();
        let thread: AnyElement = if self.thread.blank && self.thread.messages.is_empty() {
            centered("Pick an agent and its settings below, then send a message to start a thread".into(), cx)
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
                        .pt_6()
                        .when(last, |this| this.pb_6())
                        .child(render_msg(ix, msg, last, &this, &selection, window, cx))
                        .into_any_element()
                }),
            )
            .size_full()
            .into_any_element()
        };
        // The session's settings, which can be changed before the thread starts and after.
        let settings = self.setting_menus(cx);
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
                .pill()
                .small()
                .tooltip("Stop")
                .on_click(move |_, window, cx| stop.update(cx, |reader, cx| reader.stop(window, cx)))
        };
        let send_button = |send: Entity<Reader>, tooltip: &'static str| {
            let button = Button::new("send")
                .icon(IconName::ArrowUp)
                .ghost()
                .pill()
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
        // Before the thread starts, its agent is chosen here; after, it is only named.
        let chips: AnyElement = if self.thread.blank {
            let name = self.agent_label().map(str::to_string).or_else(|| self.chosen().map(|agent| agent.name.clone()));
            let name = name.unwrap_or_else(|| "No agent".to_string());
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
                                    MenuItem::new(agent.name.clone(), move |window, cx| {
                                        pick.update(cx, |reader, cx| reader.pick_agent(key.clone(), window, cx))
                                    })
                                })
                                .collect()
                        },
                    )
                    .menu(self.agent_menu.clone()),
                )
                .children(settings)
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
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().pl_2().text_sm().text_color(muted).child(self.agent_label().unwrap_or("Agent").to_string()))
                .children(settings)
                .into_any_element()
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
                            ui::markdown::all_text_of(reader.thread.selectable_sources())
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
                div().flex_none().px_3().pt_2().when(cx.global::<Palette>().glass.is_none(), |this| this.pb_3()).flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(860.0))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p(rems(COMPOSER_PAD_REM))
                        .rounded(colors.radius_xl)
                        .border(px(BOX_BORDER))
                        .border_color(colors.border)
                        .bg(colors.sidebar)
                        .when(!queued.is_empty(), |this| {
                            this.child(div().flex().flex_col().gap_1().pb_1().border_b_1().border_color(border).children(queued))
                        })
                        .child(div().py_1().child(Input::new(&self.thread.draft).large()))
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
    last: bool,
    this: &Entity<Reader>,
    selection: &Entity<ui::markdown::Selection>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let blocks = match msg {
        Msg::User(text) => {
            let colors = cx.global::<Palette>();
            let (bg, radius) = (colors.secondary, colors.radius_xl);
            let text_el = ui::markdown::plain(("user", ix), text, selection, window, cx);
            return div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui::relative(0.9))
                        .px_4()
                        .py_3()
                        .rounded(radius)
                        .bg(bg)
                        .text_base()
                        .child(text_el),
                )
                .into_any_element();
        }
        Msg::Agent(blocks) => blocks,
    };
    let colors = cx.global::<Palette>();
    let mut column = div().flex().flex_col().min_w_0();
    // Text blocks sit where the composer's text does; boxes (tool cards) span the composer's width.
    let text_inset = text_inset(window);
    if blocks.is_empty() {
        let thinking = div().text_sm().text_color(colors.muted_foreground).child("Thinking…");
        column = column.child(div().px(text_inset).child(thinking));
    }
    let mut n = 0;
    while n < blocks.len() {
        let id = ix * 10_000 + n;
        // Runs of tool calls read as one list; anything else is set a step apart.
        let run = n > 0 && matches!((&blocks[n - 1], &blocks[n]), (Block::Tool(_), Block::Tool(_)));
        // A run of three or more tool calls gets a header that can fold it to a single line, and
        // its first call sits as close under that as the calls do to each other.
        let len = if matches!(blocks[n], Block::Tool(_)) && !run { blocks[n..].iter().take_while(|b| matches!(b, Block::Tool(_))).count() } else { 0 };
        let titled = len >= 3;
        if titled {
            let folded = run_folded(&blocks[n..n + len], last && n + len == blocks.len());
            column = column.child(div().when(n > 0, |this| this.mt_4()).child(tool_run(ix, n, &blocks[n..n + len], folded, this, text_inset, cx)));
            if folded {
                n += len;
                continue;
            }
        }
        let (el, boxed) = match &blocks[n] {
            Block::Text(text) => (ui::markdown::markdown(("answer", id), text, selection, window, cx).into_any_element(), false),
            Block::Thought(text, open) => (thought(ix, n, text, *open, this, selection, window, cx), false),
            Block::Tool(tool) => (tool_card(ix, n, tool, this, text_inset, selection, window, cx).into_any_element(), true),
            Block::Permission(ask) => (permission(ix, n, ask, this, text_inset, cx).into_any_element(), true),
            Block::Plan(entries) => (plan(entries, cx).into_any_element(), false),
            Block::Approve { agent, new_agent, mcp, answered, .. } => (approval(ix, n, agent, *new_agent, mcp, *answered, this, text_inset, cx).into_any_element(), true),
        };
        let el = if boxed { el } else { div().px(text_inset).child(el).into_any_element() };
        column = column.child(div().when(n > 0, |this| if run || titled { this.mt_0p5() } else { this.mt_4() }).child(el));
        n += 1;
    }
    column.into_any_element()
}

/// Whether a run of tool calls shows as one line. A run the user hasn't clicked folds once the
/// agent has moved on from it (`live` is false) and everything in it went well, so nothing running
/// or failed hides. `live` is a run still at the end of the latest message.
fn run_folded(run: &[Block], live: bool) -> bool {
    let Some(Block::Tool(first)) = run.first() else { return false };
    first.fold.unwrap_or(!live && run.iter().all(|b| matches!(b, Block::Tool(t) if t.status == Step::Done)))
}

/// Where each run of tool calls shown folded starts in `blocks`.
fn folded_runs(blocks: &[Block], last: bool) -> Vec<usize> {
    let mut starts = vec![];
    let mut n = 0;
    while n < blocks.len() {
        if !matches!(blocks[n], Block::Tool(_)) {
            n += 1;
            continue;
        }
        let len = blocks[n..].iter().take_while(|b| matches!(b, Block::Tool(_))).count();
        if len >= 3 && run_folded(&blocks[n..n + len], last && n + len == blocks.len()) {
            starts.push(n);
        }
        n += len;
    }
    starts
}

/// The header of a run of tool calls: how many there were, and a click folds or unfolds them.
fn tool_run(ix: usize, n: usize, run: &[Block], folded: bool, this: &Entity<Reader>, text_inset: gpui::Pixels, cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    let tools: Vec<&ToolBlock> = run.iter().filter_map(|b| if let Block::Tool(t) = b { Some(t) } else { None }).collect();
    let status = if tools.iter().any(|t| t.status == Step::Failed) {
        Step::Failed
    } else if tools.iter().all(|t| t.status == Step::Done) {
        Step::Done
    } else {
        Step::Working
    };
    let (mark, tint) = step_mark(status, colors);
    let (muted, hover, radius) = (colors.muted_foreground, colors.list_hover, colors.radius);
    let toggle = this.clone();
    div()
        .id(("tool-run", ix * 10_000 + n))
        .flex()
        .items_center()
        .gap_2()
        .py_1p5()
        .px(text_inset)
        .rounded(radius)
        .cursor_pointer()
        .text_sm()
        .hover(move |row| row.bg(hover))
        .on_click(move |_, _, cx| toggle.update(cx, |reader, cx| reader.fold_run(ix, n, !folded, cx)))
        .child(div().text_color(tint).child(mark))
        .child(div().text_color(muted).child(format!("{} tool calls", tools.len())))
        .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).size(IconSize::XSmall).color(muted))
}

/// Opens every block `flag` picks out, or closes them all when none is left to open. Returns
/// the first message that changed, if any.
fn toggle_all(messages: &mut [Msg], flag: fn(&mut Block) -> Option<&mut bool>) -> Option<usize> {
    let open = messages.iter_mut().any(|m| match m {
        Msg::Agent(blocks) => blocks.iter_mut().any(|b| flag(b).is_some_and(|f| !*f)),
        Msg::User(_) => false,
    });
    let mut first = None;
    for (ix, msg) in messages.iter_mut().enumerate() {
        let Msg::Agent(blocks) = msg else { continue };
        for shown in blocks.iter_mut().filter_map(flag) {
            if *shown != open {
                *shown = open;
                first.get_or_insert(ix);
            }
        }
    }
    first
}

/// A thought: a quiet "Thinking" row that opens to the agent's reasoning.
fn thought(ix: usize, n: usize, text: &str, open: bool, this: &Entity<Reader>, selection: &Entity<ui::markdown::Selection>, window: &mut Window, cx: &mut App) -> AnyElement {
    let muted = cx.global::<Palette>().muted_foreground;
    let toggle = this.clone();
    let header = div()
        .id(("thought", ix * 10_000 + n))
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_sm()
        .text_color(muted)
        .on_click(move |_, _, cx| toggle.update(cx, |reader, cx| reader.toggle_thought(ix, n, cx)))
        .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(IconSize::XSmall))
        .child("Thinking");
    let column = div().flex().flex_col().gap_1().child(header);
    if open {
        column.child(div().opacity(theme::tokens::DIM).child(ui::markdown::italic(("thought", ix * 10_000 + n), text, selection, window, cx))).into_any_element()
    } else {
        column.into_any_element()
    }
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
                .rounded(colors.radius)
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
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .text_sm()
        .children(entries.iter().map(|(text, status)| {
            let (mark, tint) = step_mark(*status, colors);
            let done = *status == Step::Done;
            div()
                .flex()
                .gap_2()
                .child(div().flex_none().text_color(tint).child(mark))
                .child(div().flex_1().min_w_0().text_color(if done { colors.muted_foreground } else { colors.foreground }).child(text.clone()))
        }))
}

#[allow(clippy::too_many_arguments)]
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
    let (muted, hover, border) = (colors.muted_foreground, colors.list_hover, colors.border);
    let radius = colors.radius;
    let settled = tool.status == Step::Done;
    let toggle = this.clone();
    let reveal = this.clone();
    let path = tool.path.clone();
    // A tool call is a quiet row, not a box: it lights up under the pointer, and its text sits on
    // the composer's text edge like the answer's.
    let mut card = div()
        .id(("tool-row", ix * 10_000 + n))
        .flex()
        .flex_col()
        .gap_2()
        .py_1p5()
        .px(text_inset)
        .rounded(radius)
        .hover(move |row| row.bg(hover))
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
                        .items_start()
                        .gap_2()
                        .cursor_pointer()
                        .text_sm()
                        .on_click(move |_, _, cx| toggle.update(cx, |reader, cx| reader.toggle_tool(ix, n, cx)))
                        .child(div().text_color(tint).child(mark))
                        .child(div().flex_1().min_w_0().truncate().text_color(if settled { muted } else { colors.foreground }).child(tool.title.clone())),
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
                let family = mono.clone();
                let body = ui::markdown::plain_styled(
                    ("tool-text", (ix * 10_000 + n) * 2 + which),
                    text,
                    selection,
                    |style| {
                        style.font_family = family;
                        style.color = muted;
                    },
                    window,
                    cx,
                );
                card = card.child(
                    div()
                        .id(("tool-pane", (ix * 10_000 + n) * 2 + which))
                        .max_h(rems(16.))
                        .overflow_y_scroll()
                        .border_l_2()
                        .border_color(border)
                        .pl_3()
                        .font_family(mono)
                        .text_xs()
                        .text_color(muted)
                        .child(body),
                );
            }
        }
    }
    card
}

/// The one option in a permission request rendered as the filled button: the first way of saying
/// yes, however the agent orders the rest. Options that describe themselves in `kind` (ACP) are
/// picked by it; agents that send no kinds at all get their first "allow" in the name honoured.
fn primary_option(options: &[PermissionOption]) -> Option<usize> {
    if options.iter().any(|o| !o.kind.is_empty()) {
        options.iter().position(|o| o.kind == "allow_once")
    } else {
        options.iter().position(|o| o.name.to_lowercase().contains("allow"))
    }
}

fn permission(ix: usize, n: usize, ask: &Ask, this: &Entity<Reader>, text_inset: gpui::Pixels, cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    let (muted, warning) = (colors.muted_foreground, colors.warning);
    let primary = primary_option(&ask.options);
    let row = div().flex().flex_wrap().gap_2();
    let row = match &ask.answered {
        Some(name) => row.child(div().text_color(muted).child(format!("→ {name}"))),
        None => row.children(ask.options.iter().enumerate().map(|(o, option)| {
            let this = this.clone();
            let button = Button::new(("permit", ix * 1_000_000 + n * 100 + o))
                .label(option.name.clone())
                .small()
                .on_click(move |_, _, cx| this.update(cx, |reader, cx| reader.answer(ix, n, o, cx)));
            if primary == Some(o) { button.primary() } else { button }
        })),
    };
    let waiting = ask.answered.is_none();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .py_3()
        .px(text_inset - px(BOX_BORDER))
        .rounded(colors.radius)
        .border(px(BOX_BORDER))
        // A question still waiting for you is outlined in the warning tone; an answered one settles.
        .border_color(if waiting { warning.opacity(theme::tokens::MUTED) } else { colors.border })
        .text_sm()
        .child(div().flex().gap_2().child(div().flex_none().text_color(muted).child("Permission")).child(div().flex_1().min_w_0().child(ask.title.clone())))
        .child(row)
}

/// Asks whether Po may run an agent command it doesn't ship with.
#[allow(clippy::too_many_arguments)]
fn approval(ix: usize, n: usize, agent: &config::Agent, new_agent: bool, mcp: &[config::McpServer], answered: Option<bool>, this: &Entity<Reader>, text_inset: gpui::Pixels, cx: &App) -> impl IntoElement {
    let colors = cx.global::<Palette>();
    let (muted, warning) = (colors.muted_foreground, colors.warning);
    let row = div().flex().flex_wrap().gap_2();
    let row = match answered {
        Some(yes) => row.child(div().text_color(muted).child(match (yes, new_agent) {
            (true, _) => "→ Approved",
            (false, true) => "→ Not run",
            (false, false) => "→ Left out",
        })),
        None => row
            .child({
                let this = this.clone();
                Button::new(("approve-run", ix * 100 + n)).label("Run it").primary().small().on_click(move |_, window, cx| {
                    this.update(cx, |reader, cx| reader.answer_approval(ix, n, true, window, cx))
                })
            })
            .child({
                let this = this.clone();
                Button::new(("approve-skip", ix * 100 + n)).label(if new_agent { "Don't run" } else { "Leave out" }).small().on_click(move |_, window, cx| {
                    this.update(cx, |reader, cx| reader.answer_approval(ix, n, false, window, cx))
                })
            }),
    };
    let mut lines: Vec<String> = Vec::new();
    if new_agent {
        lines.push(match &agent.daemon {
            Some(url) => format!("{}  (daemon {url})", agent.command),
            None => agent.command.clone(),
        });
    }
    lines.extend(mcp.iter().map(|server| format!("{}: {}", server.name, server.line())));
    let question = match (new_agent, mcp.is_empty()) {
        (true, true) => format!("“{}” is not one of Po's own agents: it comes from your config. Run this command?", agent.name),
        (true, false) => format!("“{}” is not one of Po's own agents, and your config lists MCP servers for it. Run these commands?", agent.name),
        (false, _) => "Your config lists MCP servers for the agent to start, which run as you. Hand them over?".to_string(),
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .py_3()
        .px(text_inset - px(BOX_BORDER))
        .rounded(colors.radius)
        .border(px(BOX_BORDER))
        .border_color(if answered.is_none() { warning.opacity(theme::tokens::MUTED) } else { colors.border })
        .text_sm()
        .child(div().flex().gap_2().child(div().flex_none().text_color(muted).child("Approve")).child(
            div().flex_1().min_w_0().child(question),
        ))
        .children(lines.into_iter().map(|line| div().font_family(cx.global::<Palette>().mono_font_family.clone()).child(line)))
        .child(row)
}

fn status_text(text: String, cx: &App) -> impl IntoElement {
    div().flex_none().truncate().text_xs().text_color(cx.global::<Palette>().muted_foreground).child(text)
}

/// A section's count, in a small pill beside its title.
fn count_badge(count: usize, muted: gpui::Hsla) -> gpui::Div {
    div()
        .px_1p5()
        .rounded(theme::tokens::RADIUS_FULL)
        .bg(muted.opacity(theme::tokens::TINT))
        .text_color(muted)
        .font_weight(FontWeight::MEDIUM)
        .child(count.to_string())
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
fn pane_header(title: &str, right: impl IntoElement, lead: Option<AnyElement>, trail: Option<AnyElement>, cx: &App) -> gpui::Div {
    let colors = cx.global::<Palette>();
    header_row(
        div().flex().items_center().gap_1().min_w_0().children(lead).child(
            div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(colors.foreground).child(title.to_string()),
        ),
        div().flex().items_center().gap_2().child(right).children(trail),
        cx,
    )
    // The transcript scrolls under this one, so it keeps its line.
    .when(colors.glass.is_some(), |row| row.border_b_1().border_color(colors.border))
}

/// `pane` as a sheet of glass floating apart from its neighbours when the glass look is on,
/// else as it is. Each pane keeps half the gap; the window's padding makes up the other half at
/// its edges. gpui clips to rectangles, not rounded corners, so the pane leaves its bottom
/// corners' height empty: a row tinted down there would poke square past the rim.
fn floating(pane: impl IntoElement + Styled, cx: &App) -> AnyElement {
    let colors = cx.global::<Palette>();
    match colors.glass {
        Some(glass) => div()
            .size_full()
            .p(glass.inset / 2.)
            .child(pane.rounded(colors.radius_xl).pb(colors.radius_xl).border_1().border_color(glass.rim).shadow(glass.shadow()).overflow_hidden())
            .into_any_element(),
        None => pane.into_any_element(),
    }
}

/// A pane header: `left` and `right` on a bar with a line under it.
fn header_row(left: impl IntoElement, right: impl IntoElement, cx: &App) -> gpui::Div {
    let colors = cx.global::<Palette>();
    div()
        .h_11()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        // On glass the headers' titles set the sections apart; no line is drawn under them.
        .when(colors.glass.is_none(), |row| row.border_b_1().border_color(colors.border))
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
        self.settle_agent_changes();
        if !self.painted {
            self.painted = true;
            trace::mark("first render");
            if trace::exit_after_first_frame() {
                cx.defer(|_| std::process::exit(0));
            }
        }
        let colors = cx.global::<Palette>();
        let foreground = colors.foreground;
        let glass = colors.glass;
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
            (floating(self.agent_panel(Some(sidebar_toggle), Some(trailing), cx), cx), self.pane(None, None, cx))
        } else if swapped {
            // The conversation sits on the right; with it collapsed the viewer keeps the middle
            // and carries both controls.
            if agent_open {
                (self.pane(Some(sidebar_toggle), None, cx), floating(self.agent_panel(None, Some(trailing), cx), cx))
            } else {
                (self.pane(Some(sidebar_toggle), Some(trailing), cx), self.pane(None, None, cx))
            }
        } else {
            (floating(self.agent_panel(Some(sidebar_toggle), None, cx), cx), self.pane(None, Some(trailing), cx))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            // No background of its own: the `Shell` around it paints the window's.
            .text_color(foreground)
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|reader, _, _, _| reader.last_input = Instant::now()))
            .on_scroll_wheel(cx.listener(|reader, _, _, _| reader.last_input = Instant::now()))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|reader, _, _, _| reader.last_input = Instant::now()))
            .on_action(cx.listener(|reader, _: &ShowFinder, window, cx| reader.show_finder(window, cx)))
            .on_action(cx.listener(|reader, _: &ToggleTools, _, cx| reader.toggle_tools(cx)))
            .on_action(cx.listener(|reader, _: &ToggleThoughts, _, cx| reader.toggle_thoughts(cx)))
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
                div().flex_1().min_h_0().when_some(glass, |this, glass| this.p(glass.inset / 2.)).child(
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
    /// What the window was last told to show behind its contents.
    appearance: WindowBackgroundAppearance,
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        window.set_rem_size(palette.font_size);
        // Glass shows the desktop through the window, blurred where the platform can.
        let appearance =
            if palette.glass.is_some() { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Opaque };
        if appearance != self.appearance {
            self.appearance = appearance;
            window.set_background_appearance(appearance);
        }
        div()
            .id("root")
            .relative()
            .size_full()
            .font_family(palette.font_family.clone())
            .bg(palette.window)
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
    if cli.len() == 1 && cli[0] == "mcp" {
        if let Err(error) = mcp::serve() {
            eprintln!("po mcp: {error:#}");
            std::process::exit(1);
        }
        return;
    }
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
            KeyBinding::new("ctrl-o", ToggleTools, None),
            KeyBinding::new("ctrl-t", ToggleThoughts, None),
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
            KeyBinding::new("secondary-shift-o", OpenProject, None),
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
                theme::watch(cx);
                let reader = cx.new(|cx| Reader::new(root, pr, command, prefetch, window, cx));
                cx.new(|_| Shell { child: reader, appearance: WindowBackgroundAppearance::Opaque })
            },
        )
        .expect("window");
        cx.activate(true);
    });
}

/// What a thread is doing, in a line for the live list: the question it waits on, the tool it is
/// running, or the last thing it said.
fn preview_of(messages: &[Msg], busy: bool) -> String {
    if let Some(Msg::Agent(blocks)) = messages.last() {
        if let Some(ask) = blocks.iter().find_map(|b| match b {
            Block::Permission(ask) if ask.answered.is_none() => Some(ask),
            _ => None,
        }) {
            return format!("Approve: {}", ask.title);
        }
        if let Some(what) = blocks.iter().find_map(|b| match b {
            Block::Approve { agent, new_agent: true, answered: None, .. } => Some(agent.command.clone()),
            Block::Approve { mcp, answered: None, .. } => Some(format!("MCP servers: {}", mcp.iter().map(|server| server.name.as_str()).collect::<Vec<_>>().join(", "))),
            _ => None,
        }) {
            return format!("Approve running: {what}");
        }
        // A tool still going is the latest thing, whatever text came after it.
        if let Some(tool) = blocks.iter().rev().find_map(|b| match b {
            Block::Tool(tool) if tool.status == Step::Working => Some(tool),
            _ => None,
        }) {
            return tool.title.clone();
        }
        for block in blocks.iter().rev() {
            if let Block::Text(text) = block
                && let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty())
            {
                return line.trim().to_string();
            }
        }
    }
    if busy { "Working…".into() } else { String::new() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_file_tree_needs_a_pane_of_at_least_the_minimum_width() {
        use super::{TREE_MIN_PANE, tree_fits};
        assert!(tree_fits(TREE_MIN_PANE));
        assert!(!tree_fits(TREE_MIN_PANE - 1.0));
    }

    #[test]
    fn an_empty_viewer_says_how_to_get_the_tree_back() {
        use super::pick_hint;
        assert_eq!(pick_hint("a file", true, false), "Pick a file in the tree");
        assert!(pick_hint("a file", false, true).ends_with("show the file tree"));
        assert!(pick_hint("a file", false, false).contains("widen the pane"));
    }

    #[test]
    fn an_agents_layout_is_saved_as_the_users_until_the_user_changes_it() {
        use super::AgentChange;
        // The agent opened the viewer: while it is open, the user's closed one is what is saved.
        let change = AgentChange::after(None, 1, 2);
        assert_eq!(AgentChange::saved(Some(&change), 2), 1);
        // The user moved on from it: what they left is theirs.
        assert_eq!(AgentChange::saved(Some(&change), 3), 3);
        // A second move on top of the agent's keeps the user's from before the first.
        let modes = AgentChange::after(None, 'f', 'd');
        let again = AgentChange::after(Some(modes), 'd', 'c');
        assert_eq!(AgentChange::saved(Some(&again), 'c'), 'f');
        // A move after the user changed it starts from theirs.
        let after_user = AgentChange::after(Some(modes), 'c', 'd');
        assert_eq!(AgentChange::saved(Some(&after_user), 'd'), 'c');
        assert_eq!(AgentChange::saved(Some(&after_user), 'x'), 'x');
        assert_eq!(AgentChange::saved(None, 'x'), 'x');
        // The user moved off the agent's layout and back by hand: the record is gone, so what
        // they came back to is what is saved.
        let change = AgentChange::after(None, 1, 2);
        assert!(AgentChange::settled(Some(change), 2).is_some());
        let gone = AgentChange::settled(Some(change), 3);
        assert!(gone.is_none());
        assert_eq!(AgentChange::saved(gone.as_ref(), 2), 2);
    }

    #[test]
    fn an_agents_own_view_show_does_not_drop_its_pending_move() {
        use super::{Mode, PendingMove, Screen};
        use crate::command::Command;
        let from = Screen { side_open: false, agent_open: false, mode: Mode::Source, current: None, picked: None, shown: None };
        // The agent shows a view on the thread whose file open waits: the move follows what the
        // agent touched, so it applies once the pause comes.
        let mut pending = PendingMove {
            thread: 1,
            command: Command::FileOpen { path: "a.rs".into(), line: None },
            from: from.clone(),
        };
        let shown = Screen { side_open: true, agent_open: true, mode: Mode::Source, current: None, picked: None, shown: Some("v".into()) };
        pending.follows_agent(1, &shown);
        assert_eq!(pending.from.shown.as_deref(), Some("v"));
        assert!(pending.from.side_open && pending.from.agent_open);
        // What the agent did not touch stays as deferred, so a user's change to it still counts.
        assert!(matches!(pending.from.mode, Mode::Source));
        // A move of another thread does not follow: it drops when its thread goes off screen.
        let mut other = PendingMove { thread: 2, command: Command::ViewerClose, from };
        other.follows_agent(1, &shown);
        assert_eq!(other.from.shown, None);
    }

    fn thought_flag(b: &mut Block) -> Option<&mut bool> {
        if let Block::Thought(_, shown) = b { Some(shown) } else { None }
    }

    #[test]
    fn toggling_opens_all_then_folds_all() {
        let mut messages = vec![
            Msg::User("hi".into()),
            Msg::Agent(vec![Block::Thought("a".into(), true), Block::Text("t".into())]),
            Msg::Agent(vec![Block::Thought("b".into(), false)]),
        ];
        assert_eq!(toggle_all(&mut messages, thought_flag), Some(2));
        assert!(matches!(&messages[1], Msg::Agent(b) if matches!(b[0], Block::Thought(_, true))));
        assert!(matches!(&messages[2], Msg::Agent(b) if matches!(b[0], Block::Thought(_, true))));
        assert_eq!(toggle_all(&mut messages, thought_flag), Some(1));
        assert!(matches!(&messages[1], Msg::Agent(b) if matches!(b[0], Block::Thought(_, false))));
        assert_eq!(toggle_all(&mut vec![Msg::User("x".into())], thought_flag), None);
    }

    use super::{Activity, Block, Msg, PermissionOption, Step, ToolBlock, folded_runs, preview_of, language, run_folded, toggle_all, match_files, primary_option, syntax};

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

    fn option(name: &str, kind: &str) -> PermissionOption {
        PermissionOption { id: name.to_lowercase(), name: name.into(), kind: kind.into() }
    }

    // “Always allow” listed first is not the first way of saying yes: the once-option is.
    #[test]
    fn allow_always_first_is_not_primary() {
        let options = vec![option("Always allow", "allow_always"), option("Allow once", "allow_once"), option("Deny", "reject_once")];
        assert_eq!(primary_option(&options), Some(1));
    }

    #[test]
    fn first_of_several_allow_once_wins() {
        let options = vec![option("Allow once", "allow_once"), option("Allow once (this project)", "allow_once"), option("Deny", "reject_once")];
        assert_eq!(primary_option(&options), Some(0));
    }

    // No option says yes once, so nothing is primary and all buttons stay plain.
    #[test]
    fn kinds_without_allow_once_leave_nothing_primary() {
        let options = vec![option("Always allow", "allow_always"), option("Deny once", "reject_once"), option("Always deny", "reject_always")];
        assert_eq!(primary_option(&options), None);
    }

    // Agents from before `kind` was in the protocol describe options in names only.
    #[test]
    fn names_only_fall_back_to_the_first_allow() {
        let options = vec![option("Allow once", ""), option("Allow for session", ""), option("Deny", "")];
        assert_eq!(primary_option(&options), Some(0));
    }

    fn tool(title: &str, status: Step) -> Block {
        Block::Tool(ToolBlock { id: String::new(), title: title.into(), status, open: false, fold: None, arguments: String::new(), result: String::new(), path: None })
    }

    #[test]
    fn a_run_of_finished_calls_folds_once_it_is_behind_the_latest_message() {
        let run = vec![tool("a", Step::Done), tool("b", Step::Done), tool("c", Step::Done)];
        assert!(run_folded(&run, false));
        assert!(!run_folded(&run, true), "the latest message stays open");
        let failed = vec![tool("a", Step::Done), tool("b", Step::Failed), tool("c", Step::Done)];
        assert!(!run_folded(&failed, false), "a failure doesn't hide");
        let running = vec![tool("a", Step::Done), tool("b", Step::Working), tool("c", Step::Waiting)];
        assert!(!run_folded(&running, false), "a call still going doesn't hide");
    }

    #[test]
    fn a_click_on_a_run_overrides_the_default() {
        let mut run = vec![tool("a", Step::Done), tool("b", Step::Done), tool("c", Step::Done)];
        let Block::Tool(first) = &mut run[0] else { unreachable!() };
        first.fold = Some(false);
        assert!(!run_folded(&run, false));
        let mut failed = vec![tool("a", Step::Failed), tool("b", Step::Done), tool("c", Step::Done)];
        let Block::Tool(first) = &mut failed[0] else { unreachable!() };
        first.fold = Some(true);
        assert!(run_folded(&failed, true));
    }

    #[test]
    fn only_runs_of_three_or_more_are_found_folded() {
        let blocks = vec![
            tool("a", Step::Done),
            tool("b", Step::Done),
            Block::Text("x".into()),
            tool("c", Step::Done),
            tool("d", Step::Done),
            tool("e", Step::Done),
        ];
        assert_eq!(folded_runs(&blocks, false), vec![3]);
        assert!(folded_runs(&blocks, true).is_empty());
    }

    #[test]
    fn a_run_followed_by_text_folds_in_the_latest_message() {
        let blocks = vec![tool("a", Step::Done), tool("b", Step::Done), tool("c", Step::Done), Block::Text("x".into())];
        assert_eq!(folded_runs(&blocks, true), vec![0]);
        assert!(folded_runs(&blocks[..3], true).is_empty());
    }

    #[test]
    fn a_running_tool_is_the_preview_even_when_text_follows_it() {
        let messages = vec![Msg::Agent(vec![tool("Run tests", Step::Working), Block::Text("Looking at the output\n".into())])];
        assert_eq!(preview_of(&messages, true), "Run tests");
    }

    #[test]
    fn the_preview_falls_back_to_the_last_line_then_to_working() {
        let said = vec![Msg::Agent(vec![tool("Read", Step::Done), Block::Text("first\n\nlast line  \n\n".into())])];
        assert_eq!(preview_of(&said, true), "last line");
        let silent = vec![Msg::Agent(vec![tool("Read", Step::Done)])];
        assert_eq!(preview_of(&silent, true), "Working…");
        assert_eq!(preview_of(&silent, false), "");
    }

    #[test]
    fn the_live_list_puts_questions_first_and_leaves_idle_threads_out() {
        let mut ranks: Vec<_> = [Activity::Running, Activity::Idle, Activity::Unread, Activity::Asking]
            .into_iter()
            .filter_map(|a| a.live_rank().map(|rank| (rank, a)))
            .collect();
        ranks.sort_by_key(|(rank, _)| *rank);
        assert!(matches!(ranks.as_slice(), [(_, Activity::Asking), (_, Activity::Unread), (_, Activity::Running)]));
    }
}
