#![recursion_limit = "1024"]
mod acp;
mod agent;
mod alert;
mod attach;
mod command;
mod config;
mod control;
mod ctl;
mod dex;
mod diff;
mod explorer;
mod fonts;
mod issues;
mod mcp;
mod nsh;
mod pr;
mod reader;
mod schemes;
mod settings;
mod terminal;
mod terminal_view;
mod theme;
mod state;
mod date;
mod trace;
mod ui;
mod update;
mod workspace;
mod worktree;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use acp::{ConfigOption, Event, PermissionOption, SessionInfo};
use agent::Client;
use attach::Attachment;
use dex::Queue;
use diff::{Change, Kind, Row};
use theme::Palette;
use ui::resizable::{SidebarsState, sidebars};
use explorer::FileTree;
use futures::StreamExt;
use serde_json::Value;

use gpui::{
    Focusable as _,
    AnyElement, App, Pixels, TitlebarOptions, WindowControlArea, point, Bounds, Context, Entity, IntoElement, ParentElement,
    Render, Styled, Subscription, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, div,
    prelude::*, px, rems, size, KeyBinding,
};

gpui::actions!(po, [ToggleTools, ToggleThoughts, ToggleLeft, ToggleRight, ShowSource, ShowChanges, ShowChat, SwapPanes, ShowHistory, ToggleSplit, ToggleRendered, NewThread, OpenProject, ShowFinder, ToggleFocus, ShowSettings, CloseSettings, CloseWindow, ToggleTerminal]);

/// The id the viewer's tabs know the terminal by.
const TERMINAL_TAB: &str = "po:terminal";

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
    /// On the first call of a run of [`MIN_RUN`] or more: whether the run is folded to one line. `None`
    /// leaves it to `run_folded`'s default, until the user clicks the run's header.
    fold: Option<bool>,
    /// The plan entry that was in progress when the call began: what the call is working towards.
    task: Option<String>,
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

/// Where text headed for an agent came from, so that it is labelled as coming from there at
/// every step: sent at once, waiting for the agent, queued, or taken back. It never ends up in
/// the user's draft, and it never claims "the user has `x` open" when the user didn't send it.
/// The thread and Po origins come from `thread.send` and notify-parent.
#[allow(dead_code)]
enum Origin {
    /// The user's draft.
    User,
    /// Another thread of this window, sending through the API (`thread.send`).
    Thread { id: u64, title: String },
    /// Po itself, telling an orchestrator what its child did (notify-parent).
    Po,
}

/// How much longer a parent must wait before it may be told something, given how long ago it
/// last was; `None` when it may be told now (never told, or long enough ago). A wait of nothing
/// is not a wait: it would put the notice back for ever.
fn notice_wait(since_last: Option<Duration>) -> Option<Duration> {
    NOTICE_RATE.checked_sub(since_last?).filter(|left| !left.is_zero())
}

/// What starts a message of Po's in a transcript, so that it isn't taken for the user's.
const PO_MARK: &str = "[Po] ";

/// Where a picture to attach comes from.
enum Source {
    Bytes(Vec<u8>),
    Path(PathBuf),
}

/// A message headed for an agent, with where it came from.
struct Outgoing {
    text: String,
    origin: Origin,
    images: Vec<Arc<Attachment>>,
}

impl Outgoing {
    /// How the transcript shows it: Po's own messages say so, and never name the thread.
    fn shown(&self) -> String {
        match self.origin {
            Origin::Po => format!("{PO_MARK}{}", self.text),
            _ => self.text.clone(),
        }
    }

    fn user(text: String, images: Vec<Arc<Attachment>>) -> Self {
        Self { text, origin: Origin::User, images }
    }

    /// What a daemon that takes only text is sent, and later tells back as accepted.
    fn queued_text(&self) -> String {
        Client::queued_text(&self.text, &self.images)
    }
}

/// How a thread reports to the thread that started it, when one did. Every report is a turn of
/// the parent's, so they are coalesced and rate limited (see [`Reader::flush_notice`]).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Notify {
    /// A turn's end, a failure, an ask, a sleep or a close.
    #[default]
    Done,
    /// Also that a turn started.
    All,
    Never,
}

impl Notify {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "done" => Self::Done,
            "all" => Self::All,
            "never" => Self::Never,
            _ => return None,
        })
    }
}

/// A message typed while the agent was working.
struct Queued {
    out: Outgoing,
    /// Where the daemon holds it; `None` when it waits here for the turn to end.
    kind: Option<Queue>,
}

#[derive(Clone)]
enum Msg {
    User(String, Vec<Arc<Attachment>>),
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

/// Where a message for the agent goes, judged by where its thread is: the decision
/// [`Reader::submit`] carries out, pure, so every path is testable without a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Where {
    /// The thread has no session of its own yet: it starts with this message.
    Start,
    /// The agent is starting, taking its settings, or asleep: the message waits for it, shown
    /// as sent.
    Wait,
    /// The agent is ready and idle: dispatch now.
    Dispatch,
    /// The agent takes messages mid-turn: steer now, or a follow-up after the turn (`later`).
    Take(Queue),
    /// The agent can't take messages mid-turn: the thread queues it for the turn's end.
    Queue,
}

fn route(blank: bool, unstarted: bool, asleep: bool, starting: bool, busy: bool, takes_messages: bool, later: bool) -> Where {
    if blank || unstarted {
        Where::Start
    } else if !busy && (starting || asleep) {
        Where::Wait
    } else if !busy {
        Where::Dispatch
    } else if takes_messages {
        Where::Take(if later { Queue::Followup } else { Queue::Steer })
    } else {
        Where::Queue
    }
}

/// A thread out of a list of views, by its id: for reading, which never swaps one in.
fn find_thread<'a>(views: &'a [View<'a>], id: u64) -> Option<(&'a View<'a>, &'a Thread)> {
    views.iter().find_map(|view| view.threads.iter().find(|thread| thread.id == id).map(|thread| (view, *thread)))
}

/// The status of a thing that runs, as the read model says it.
fn step_name(status: Step) -> &'static str {
    match status {
        Step::Waiting => "waiting",
        Step::Working => "working",
        Step::Done => "done",
        Step::Failed => "failed",
    }
}

/// A thread's transcript as entries: the user's text, and the agent's text, thoughts, tool
/// calls and plan. Permission asks show as a call that is waiting or done; approval cards are
/// the user's to answer and are not in the transcript.
fn transcript(thread: &Thread, full: bool) -> Vec<state::Entry> {
    let mut out = Vec::new();
    for msg in &thread.messages {
        match msg {
            Msg::User(text, _) => out.push(state::Entry { index: out.len(), role: "user", text: text.clone(), tools: vec![], plan: None }),
            Msg::Agent(blocks) => {
                let mut said = Vec::new();
                let mut tools = Vec::new();
                let mut plan = None;
                for block in blocks {
                    match block {
                        Block::Text(text) | Block::Thought(text, _) => said.push(text.clone()),
                        Block::Tool(tool) => tools.push(state::ToolCall {
                            title: tool.title.clone(),
                            status: step_name(tool.status).into(),
                            arguments: full.then(|| tool.arguments.clone()),
                            result: full.then(|| tool.result.clone()),
                        }),
                        Block::Plan(items) => plan = Some(items.iter().map(|(text, step)| (text.clone(), step_name(*step).into())).collect()),
                        Block::Permission(ask) => tools.push(state::ToolCall {
                            title: ask.title.clone(),
                            status: if ask.answered.is_none() { "waiting" } else { "done" }.into(),
                            arguments: None,
                            result: None,
                        }),
                        // The user's click answers this; it is told of by the state.
                        Block::Approve { .. } => {}
                    }
                }
                out.push(state::Entry { index: out.len(), role: "agent", text: said.join("\n\n"), tools, plan });
            }
        }
    }
    out
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
fn pick_hint(what: &str, tree_visible: bool) -> String {
    if tree_visible {
        format!("Pick {what} in the tree")
    } else {
        format!("Pick {what} with Ctrl+P, or go back to the explorer")
    }
}

/// What the sidebar lists and the viewer shows.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Source,
    Diff,
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
    /// The picked file's diff as rows, which say whether it has textual changes and how many lines
    /// it adds and removes.
    rows: Rc<Vec<Row>>,
    counts: (usize, usize),
    /// The picked file's old and new versions, which the diff view compares.
    versions: Option<(nsh::source::Shared, nsh::source::Shared)>,
    current: Option<String>,
    stamp: Option<Stamp>,
    code: Option<nsh::source::Shared>,
    /// What the current file is: the markdown flag of it, loaded elsewhere.
    is_markdown: bool,
    notice: Option<String>,
    /// Every file, folders closed until the user opens them.
    files_tree: FileTree,
    /// Set when this is a thread's own checkout, made from another project's repository.
    worktree: Option<worktree::Worktree>,
    /// The project's shell, started the first time the terminal is opened, and whether the viewer
    /// shows it.
    terminal: Option<Entity<terminal_view::TerminalView>>,
    terminal_shown: bool,
    /// Threads closed here, newest first, offered back by the session picker.
    closed: Vec<workspace::ThreadState>,
}

impl Project {
    /// Notes a closed thread, newest first. A session is there once, and only so many are kept.
    fn remember(&mut self, thread: workspace::ThreadState) {
        self.closed.retain(|known| known.session != thread.session);
        self.closed.insert(0, thread);
        self.closed.truncate(workspace::CLOSED_KEPT);
    }

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
    /// Forms the agent put on screen through Po's MCP server, and the form of each.
    forms: command::Forms,
    /// The view the viewer shows in place of a file or the changes.
    shown: Option<String>,
    /// A view the agent added while the user was busy, waiting for a click.
    offered: Option<String>,
    /// Names the thread to the events of its agent while it isn't the one on screen.
    id: u64,
    /// The thread that started this one through the API, if any. Only its parent may write to
    /// it, and it is never written to by a grandparent: a child starts at Viewer, so a
    /// grandchild can't exist. Saved as the parent's *session* (phase 4), never as its id.
    parent: Option<u64>,
    /// Its last turn ended in an error, or its agent stopped or exited mid-turn. Cleared when
    /// anything starts again, so the state reads Failed only while nothing newer has happened.
    failed: bool,
    /// Why it failed, the last time it did.
    last_error: Option<String>,
    /// Text sent through the API that a stopped turn dropped, for `thread.get` to report.
    dropped: Vec<String>,
    /// How much of its context the agent says the session uses.
    usage: Option<state::Usage>,
    /// How it tells the thread that started it (its parent) what happened to it. `Done`: when
    /// its turn ends, it fails, it asks, or it is closed or put to sleep.
    notify: Notify,
    /// Settings the API asked for, applied between turns and never remembered as the user's
    /// choice: they are read before the carried-over choices, and nothing here writes to them.
    overrides: HashMap<String, String>,
    /// The ids of settings set through the API, so their answers are not saved as choices.
    api_options: HashSet<String>,
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
    pending: Option<Outgoing>,
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
    /// What the agent was granted when it started: its tools were listed under that, so a raise
    /// since takes a restart to reach it.
    started_grant: Option<command::Grant>,
    draft: String,
    /// Pictures attached to the draft, sent with it.
    attachments: Vec<Arc<Attachment>>,
    /// Why some of what was pasted or dropped was left out, until the draft changes.
    attach_note: Option<String>,
}

impl Thread {

    fn fail(&mut self, text: String) {
        self.busy = false;
        self.failed = true;
        self.last_error = Some(text.clone());
        self.messages.push(Msg::Agent(vec![Block::Text(format!("**Error:** {text}"))]));
        self.relist(self.messages.len() - 1);
    }

    /// The bare facts the control API's read model is derived from.
    fn facts(&self) -> crate::state::Facts {
        let mut approval = false;
        let mut asking = false;
        for msg in &self.messages {
            let Msg::Agent(blocks) = msg else { continue };
            for block in blocks {
                match block {
                    Block::Approve { answered: None, .. } => approval = true,
                    Block::Permission(Ask { answered: None, .. }) => asking = true,
                    _ => {}
                }
            }
        }
        crate::state::Facts { approval, asking, failed: self.failed, asleep: self.asleep(), started: self.client.is_some(), ready: self.ready, busy: self.busy }
    }

    /// The conversation the screen draws takes the whole list of messages in every
    /// publish, so there is nothing to tell a list piecewise.
    fn relist(&mut self, _from: usize) {}

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
            Msg::User(..) => false,
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
                let picture = (update["content"]["type"] == "image").then(|| Attachment::from_block(&update["content"])).flatten().map(Arc::new);
                match self.messages.last_mut() {
                    Some(Msg::User(body, pictures)) => {
                        body.push_str(text);
                        pictures.extend(picture);
                    }
                    _ => {
                        self.messages.push(Msg::User(text.to_string(), picture.into_iter().collect()));
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
            Some("tool_call") => {
                let task = task_in_progress(&self.messages);
                self.blocks().push(Block::Tool(ToolBlock {
                    id: update["toolCallId"].as_str().unwrap_or_default().to_string(),
                    title: update["title"].as_str().unwrap_or("Tool").to_string(),
                    status: step(update["status"].as_str().unwrap_or("pending")),
                    open: false,
                    fold: None,
                    task,
                    arguments: pretty(&update["rawInput"]),
                    result: tool_result(update),
                    path: touched(update),
                }))
            }
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
    /// Waiting on the user: a question to answer, or a finished turn nobody has looked at, unless
    /// `parent_hears` of it, since then the parent agent picks it up.
    fn needs_you(self, parent_hears: bool) -> bool {
        self == Activity::Asking || (self == Activity::Unread && !parent_hears)
    }
}

/// How long ago `then` (ms since the epoch) was, as the sidebar says it: `5m`, `3h`, `2d`, `6w`,
/// `4mo`. Nothing for the last minute, and for a time that was never recorded.
fn ago(now: u64, then: u64) -> Option<String> {
    // Threads from before the time was kept hold a counter, not a time.
    if then < 1_000_000_000_000 || then > now {
        return None;
    }
    let minutes = (now - then) / 60_000;
    Some(match minutes {
        0 => return None,
        1..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h", minutes / 60),
        1440..=20159 => format!("{}d", minutes / 1440),
        20160..=86399 => format!("{}w", minutes / 10080),
        _ => format!("{}mo", minutes / 43200),
    })
}

/// `title` with its first letter in capitals when it opens with a plain word, as "fix the build"
/// does. One that opens with a path, a command or an identifier (`src/main.rs`, `cargo test`,
/// `snake_case`, `v2`) is left as typed.
fn capitalized(title: String) -> String {
    let word = title.split_whitespace().next().unwrap_or("").trim_end_matches([',', ':', ';', '.', '?', '!']);
    // A command (`cargo test`, `curl localhost`) opens with a plain word too, but is followed by
    // arguments: a flag, a path, a URL or a number. Prose is words and punctuation only.
    let prose = title.split_whitespace().skip(1).all(|w| w.chars().all(|c| c.is_alphabetic() || "'’,:;.?!-".contains(c)));
    // Bare subcommands (`cargo test`, `git status`) read as prose, so the usual tools are named.
    let tools = [
        "cargo", "git", "npm", "npx", "pnpm", "yarn", "bun", "make", "cd", "ls", "cat", "rm", "grep", "sudo", "gh", "docker", "kubectl", "rustc", "python", "pip", "node", "go", "curl", "ssh",
    ];
    let plain = !word.is_empty() && word.chars().all(|c| c.is_lowercase()) && prose && !tools.contains(&word);
    match title.chars().next() {
        Some(_) if plain => crate::nsh::sentence(&title),
        _ => title,
    }
}

/// The time now in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The most threads one thread may start (§5). With `max_live` at its default of 8, an
/// orchestrator and four children leave room for the user's own threads.
const MAX_CHILDREN: usize = 4;
/// The most `thread.wait`s the window holds, beside the socket's connections, so waits can't
/// push the viewer tools out.
const MAX_WAITS: usize = 8;
/// How long a child's changes may pile up before its parent is told.
const NOTICE_COALESCE: Duration = Duration::from_secs(2);
/// How often a parent may be told: every notice is a turn of the parent's, which costs tokens.
const NOTICE_RATE: Duration = Duration::from_secs(10);

impl Thread {
    /// A thread with no session yet and nothing said.
    fn blank(id: u64) -> Self {
        Self {
            id,
            parent: None,
            failed: false,
            last_error: None,
            dropped: vec![],
            usage: None,
            notify: Notify::default(),
            overrides: HashMap::new(),
            api_options: HashSet::new(),
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
            started_grant: None,
            draft: String::new(),
            attachments: vec![],
            attach_note: None,
            views: Default::default(),
            forms: Default::default(),
            shown: None,
            offered: None,
        }
    }

    /// What it is doing, in a line.
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
            // A thread is named for what it was asked, not for what Po told it.
            Msg::User(text, _) if text.starts_with(PO_MARK) => None,
            Msg::User(text, _) => text.lines().find(|line| !line.trim().is_empty()),
            Msg::Agent(_) => None,
        });
        match first {
            Some(line) => {
                let line = line.trim();
                let short: String = line.chars().take(72).collect();
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

    /// Whether closing it would lose something: a turn is running, a question waits or
    /// messages are queued. A blank thread has nothing to lose.
    fn at_risk(&self) -> bool {
        !self.blank && (self.busy || self.activity() == Activity::Asking || !self.queue.is_empty())
    }

    /// Lets a closed thread go. A running turn is asked to stop first and its agent given a
    /// moment to wind down before the process is killed, off the UI thread.
    fn retire(mut thread: Self) {
        /// How long a stopped agent has to wind down before its process is killed.
        const WIND_DOWN: Duration = Duration::from_secs(1);
        let Some(client) = thread.client.take() else { return };
        if thread.busy {
            client.cancel();
            std::thread::spawn(move || {
                std::thread::sleep(WIND_DOWN);
                drop(client);
            });
        }
    }

    /// What is kept of it once closed, if it has a session to come back from.
    fn closed_state(&self) -> Option<workspace::ThreadState> {
        if self.blank {
            return None;
        }
        Some(workspace::ThreadState { agent: self.agent.clone()?, session: self.session.clone()?, title: self.title(), active_at: self.active_at, parent_session: None })
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

/// One `thread.wait`: who is waiting, on which threads (empty: its children), until a state in
/// `until` (empty: any change), and where the answer goes. A `form.wait` instead carries the
/// form's id and nothing else.
struct Waiter {
    token: u64,
    caller: command::Caller,
    threads: Vec<u64>,
    until: Vec<&'static str>,
    form: Option<String>,
    reply: std::sync::mpsc::Sender<command::Reply>,
}

/// What a parent hasn't heard about its children yet.
struct Notice {
    text: String,
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
    /// Whether the session is also taking the choices of earlier threads. False when it was
    /// woken only to apply what the API asked for.
    carrying: bool,
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

/// What a thread with the Global grant put to the user, to click or turn away: nothing it asks
/// for happens until they do.
struct Suggestion {
    id: u64,
    /// The thread that suggested it, and its title as it was.
    from: u64,
    by: String,
    what: Proposal,
}

enum Proposal {
    /// Go to a project that is open.
    Go(u64),
    /// Open a folder that is not a project yet.
    Open(PathBuf),
    /// Check a PR out for review, from the checkout at `from`.
    Pr { spec: String, from: PathBuf },
    /// Merge a worktree project into the checkout it came from.
    Merge(u64),
}

/// One control command, as the audit log keeps it.
struct Audit {
    at: u64,
    caller: command::Caller,
    command: &'static str,
    outcome: String,
    /// How many times in a row it was this, for the same caller.
    count: u32,
}

/// How many commands the audit log keeps, newest last.
const MAX_AUDIT: usize = 500;

/// How many suggestions may wait at once, so a loop can't fill the sidebar.
const MAX_SUGGESTIONS: usize = 8;

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

/// What the chat last showed, to tell what changed since: the thread, the lengths of its lists and
/// whether it was scrolled to the end.
type ChatMarks = ((u64, u64), usize, usize, bool, usize);

struct Reader {
    /// How agents started from this window reach it; none where there is no socket.
    control: Option<control::Control>,
    /// What each thread's agent may do, held in memory only: it is lost on restart and is not
    /// shared between windows, so nothing an agent can read or write raises its own reach.
    /// A thread without an entry has [`command::Grant::initial`], and the grants can only be
    /// changed from the UI, never from anything an agent could write to.
    grants: HashMap<u64, command::Grant>,
    /// Projects the API opened, which it alone may close again (and only if the user has no
    /// thread of their own in them), and what it has put to the user to click.
    api_projects: HashSet<u64>,
    suggestions: Vec<Suggestion>,
    next_suggestion: u64,
    audit: std::collections::VecDeque<Audit>,
    /// `po ctl watch` connections, each told of every change of a thread's state.
    subscribers: Vec<std::sync::mpsc::Sender<command::Reply>>,
    /// A worktree for `thread.create_isolated` is being made, and the numbers its replies carry.
    isolating: bool,
    next_request: u64,
    /// `thread.wait`s held until a thread reaches a state or the wait times out. At most two
    /// per caller, [`MAX_WAITS`] in all, so waits can't push the viewer tools out.
    waiters: Vec<Waiter>,
    next_waiter: u64,
    /// What a parent hasn't been told about its children yet, one notice per parent: lines
    /// coalesce into it for [`NOTICE_COALESCE`], and the rate limit holds the flush back.
    notices: HashMap<u64, Notice>,
    notice_scheduled: HashSet<u64>,
    notice_sent_at: HashMap<u64, Instant>,
    /// This window's own handle, for telling a parent about a child from where there is no
    /// `window` in hand (the idle sweep).
    window: gpui::AnyWindowHandle,
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
    /// The strips above the file tree (a PR review's field, a worktree's finish line), drawn by
    /// NSH, and what keeps it in Po's theme.
    explorer_pane: Entity<nsh_platform_gpui::AppView>,
    tabs: Entity<nsh_platform_gpui::AppView>,
    _tabs_theme: Subscription,
    _explorer_theme: Subscription,
    /// The sidebar's filter: threads and projects that don't match it are left out of its lists.
    /// What the sidebar's filter holds.
    side_query: String,
    /// The viewer's source pane, drawn by NSH, and what keeps it in Po's theme.
    source: Entity<nsh_platform_gpui::AppView>,
    _source_theme: Subscription,
    /// The viewer's diff pane, drawn by NSH, and what keeps it in Po's theme.
    diff: Entity<nsh_platform_gpui::AppView>,
    _diff_theme: Subscription,
    /// The agent's own view or form shown in the viewer, drawn by NSH.
    agent_pane: Entity<nsh_platform_gpui::AppView>,
    _agent_pane_theme: Subscription,
    /// The window's controls the pane being built carries: its lead, then its trail.
    pane_controls: std::cell::RefCell<(Vec<nsh::header::Control>, Vec<nsh::header::Control>)>,
    /// A line the viewer was asked to scroll to, and a number that changes with each ask.
    reveal: Option<(u32, u64)>,
    reveal_serial: u64,
    /// The sidebar, drawn by NSH, and what keeps it in Po's theme.
    sidebar: Entity<nsh_platform_gpui::AppView>,
    _sidebar_theme: Subscription,
    /// The conversation pane, drawn by NSH from [`Self::chat_snapshot`], and what keeps it in Po's theme.
    chat: Entity<nsh_platform_gpui::AppView>,
    _chat_theme: Subscription,
    /// What the conversation was last published as, one row for how many messages the transcript
    /// showed and whether a turn went on the thread it shows; a change bumps [`Self::chat_follow`].
    chat_marks: std::cell::Cell<ChatMarks>,
    /// The project and thread the conversation showed last.
    chat_shown: std::cell::Cell<(u64, u64)>,
    /// What the transcript's next publishing carries as [`Snapshot::follow`], bumped when the thread gets louder.
    chat_follow: std::cell::Cell<u64>,
    /// Like [`Self::chat_follow`], for the thread growing under the reader's eyes: the transcript
    /// follows it only while it is at its end.
    chat_follow_soft: std::cell::Cell<u64>,
    /// What [`Snapshot::draft_reset`] carries next, bumped whenever the app writes the draft itself.
    chat_reset: std::cell::Cell<u64>,
    /// What [`nsh::chat::Snapshot::focus`] carries next.
    chat_focus: std::cell::Cell<u64>,
    /// The thread (project, thread) whose close was pressed once and waits for a second press,
    /// because closing it would lose work. Any other command puts it away.
    closing: Option<(u64, u64)>,
    /// The issues page, shown in place of the conversation while `issues_open`; the sidebar has a row for it.
    issues: Entity<nsh::overlay::Overlay>,
    issues_open: bool,
    /// The project whose issues [`issues::shared`] holds, so it is only switched when this changes.
    issues_root: std::cell::RefCell<PathBuf>,
    /// An issue "Work on this" is making a worktree for, to link its branch once it exists.
    issue_for_worktree: Option<(PathBuf, u64)>,
    /// Progress or failure of the last attempt to open a PR.
    pr_status: Option<String>,
    opening: bool,
    /// The number of times the strip's PR field was emptied: the screen empties its field on
    /// seeing a new one.
    pr_serial: u64,
    /// The last thing that happened to a worktree (made, merged, closed), or why it failed.
    worktree_status: Option<String>,
    /// A merge or discard is running; asking for either again waits.
    finishing: bool,
    /// Discard was asked for once and waits for a second click, since it can't be undone.
    confirm_discard: bool,
    /// The Ctrl+P finder, drawn by NSH: what its field holds, and the files that match it.
    finder: Entity<nsh::overlay::Overlay>,
    finder_query: String,
    finder_matches: Vec<String>,
    finder_open: bool,
    /// The settings page (Ctrl+,), while it is open.
    settings: Entity<nsh::overlay::Overlay>,
    settings_open: bool,
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
    /// Projects whose thread list is shown in full rather than cut to the active ones.
    threads_expanded: HashSet<u64>,
    /// Projects whose threads are folded away in the sidebar.
    projects_collapsed: HashSet<u64>,
    /// Each project's checked-out branch, read off the UI thread now and then.
    heads: HashMap<PathBuf, worktree::Head>,
    /// The width of the file tree inside the viewer pane.
    tree_cols: Entity<SidebarsState>,
    /// The viewer pane is too narrow for the file tree, as of the last frame it was drawn.
    viewer_narrow: Rc<Cell<bool>>,
    /// In a pane too narrow for the tree beside the file, the explorer is what the pane shows.
    browse: bool,
    /// The viewer pane (a file or a diff) on the right is shown.
    side_open: bool,
    /// The viewer is in the middle and the conversation on the right, instead of the reverse.
    /// Only matters while the viewer is open.
    swapped: bool,
    /// The conversation pane is shown while it sits on the right, rather than collapsed. Only
    /// matters while it is there (`side_open` and `swapped`); in the middle it is always shown.
    agent_open: bool,
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
    /// What this window last saved or restored, for saving beside other windows.
    known_projects: workspace::Known,
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
        let (diff_pane, diff_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::diff::Diff>(), window, cx);
        let (agent_pane, agent_pane_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::agent::Agent>(), window, cx);
        let (source, source_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::source::Source>(), window, cx);
        let (sidebar, sidebar_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::sidebar::Sidebar>(), window, cx);
        let (chat_pane, chat_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::chat::Chat>(), window, cx);
        let (tabs, tabs_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::tabs::Tabs>(), window, cx);
        let (explorer_pane, explorer_theme) = nsh::mount(nsh_platform_gpui::Root::of::<nsh::explorer::Explorer>(), window, cx);
        let finder_window = cx.new(|cx| nsh::overlay::Overlay::new(560., 8., nsh_platform_gpui::Root::of::<nsh::finder::Finder>, &nsh::finder::HEIGHT, cx));
        let settings_window = cx.new(|cx| nsh::overlay::Overlay::new(760., 24., nsh_platform_gpui::Root::of::<nsh::settings::Settings>, &nsh::settings::HEIGHT, cx));
        let issues = cx.new(|cx| nsh::overlay::Overlay::page(1000., nsh_platform_gpui::Root::of::<nsh::issues::Issues>, &nsh::issues::HEIGHT, cx));
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
            cx.subscribe_in(&finder_window, window, |reader, _, event: &nsh::overlay::OverlayEvent, window, cx| match event {
                nsh::overlay::OverlayEvent::Close => reader.hide_finder(window, cx),
            }),
            cx.subscribe_in(&settings_window, window, |reader, _, event: &nsh::overlay::OverlayEvent, window, cx| match event {
                nsh::overlay::OverlayEvent::Close => reader.hide_settings(window, cx),
            }),
            cx.subscribe_in(&issues, window, |reader, _, event: &nsh::overlay::OverlayEvent, window, cx| match event {
                nsh::overlay::OverlayEvent::Close => reader.hide_issues(window, cx),
            }),
        ];
        let settings = config::load();
        let mut agents = config::agents(&settings);
        let idle_minutes = settings["idle_minutes"].as_u64().unwrap_or(15);
        if let Some(command) = command.as_ref().filter(|c| !agents.iter().any(|agent| agent.id == **c)) {
            agents.push(config::Agent { id: command.clone(), name: "Custom".into(), command: command.clone(), daemon: None, vouched: true });
        }
        let project = Self::build_project(root, pr, files);
            let thread = Thread::blank(0);
        let (control, requests) = control::start().unzip();
        let mut reader = Self {
            control,
            grants: HashMap::new(),
            api_projects: HashSet::new(),
            suggestions: vec![],
            next_suggestion: 1,
            audit: std::collections::VecDeque::new(),
            subscribers: vec![],
            isolating: false,
            next_request: 1,
            waiters: vec![],
            next_waiter: 1,
            notices: HashMap::new(),
            notice_scheduled: HashSet::new(),
            notice_sent_at: HashMap::new(),
            window: window.window_handle(),
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
            explorer_pane,
            tabs,
            _tabs_theme: tabs_theme,
            _explorer_theme: explorer_theme,
            side_query: String::new(),
            source,
            _source_theme: source_theme,
            diff: diff_pane,
            _diff_theme: diff_theme,
            agent_pane,
            _agent_pane_theme: agent_pane_theme,
            pane_controls: Default::default(),
            reveal: None,
            reveal_serial: 0,
            closing: None,
            sidebar,
            _sidebar_theme: sidebar_theme,
            chat: chat_pane,
            _chat_theme: chat_theme,
            chat_marks: std::cell::Cell::new(((0, 0), 0, 0, false, 0)),
            chat_shown: std::cell::Cell::new((u64::MAX, u64::MAX)),
            chat_follow: std::cell::Cell::new(0),
            chat_follow_soft: std::cell::Cell::new(0),
            chat_reset: std::cell::Cell::new(0),
            chat_focus: std::cell::Cell::new(0),
            issues,
            issues_open: false,
            issues_root: Default::default(),
            issue_for_worktree: None,
            pr_status: None,
            opening: false,
            worktree_status: None,
            finishing: false,
            confirm_discard: false,
            finder: finder_window,
            finder_query: String::new(),
            finder_matches: Vec::new(),
            finder_open: false,
            pr_serial: 0,
            settings: settings_window,
            settings_open: false,
            painted: false,
            agents,
            notifications: settings["notifications"].as_bool().unwrap_or(true),
            idle_after: (idle_minutes > 0).then(|| Duration::from_secs(idle_minutes * 60)),
            max_live: settings["max_live_agents"].as_u64().unwrap_or(8).max(1) as usize,
            sidebars,
            left_open: true,
            threads_expanded: HashSet::new(),
            projects_collapsed: HashSet::new(),
            heads: HashMap::new(),
            tree_cols,
            viewer_narrow: Rc::default(),
            browse: true,
            side_open: true,
            swapped: config::viewer_left(&settings).unwrap_or(false),
            agent_open: true,
            choices: config::load_choices(),
            chosen_agent: command.clone(),
            split: false,
            rendered: true,
            focus: cx.focus_handle(),
            save_pending: false,
            known_projects: Default::default(),
            _subscriptions: subscriptions,
        };
        if let Some(requests) = requests {
            Self::serve_control(requests, window, cx);
        }
        Self::serve_nsh(window, cx);
        Self::watch_issues(cx);
        reader.focus.focus(window, cx);
        trace::mark("reader built");
        reader.refresh_tree();
        trace::mark("tree");
        let resumed = workspace::Workspace::load(&reader.home).is_some_and(|saved| reader.restore(saved, cx));
        reader.known_projects = reader.workspace(cx).known();
        trace::mark("restore");
        match command.filter(|_| !resumed) {
            Some(command) => reader.connect(&command, false, window, cx),
            None => reader.wake_thread(window, cx),
        }
        trace::mark("connect");
        reader.watch(watch, window, cx);
        trace::mark("watch");
        Self::poll_heads(cx);
        Self::sweep_idle(window, cx);
        reader
    }
}

/// The fewest tool calls in a row that fold to one line.
const MIN_RUN: usize = 2;

/// Whether a run of tool calls shows as one line. A run the user hasn't clicked starts folded,
/// whether its calls are still going or not.
fn run_folded(run: &[Block]) -> bool {
    let Some(Block::Tool(first)) = run.first() else { return false };
    first.fold.unwrap_or(run_outcome(run).0 != Step::Failed)
}

/// How a run of tool calls went, and how many of its calls failed. A failure the later calls got
/// past is only counted; the run is failed when none of them went well or the last one failed.
fn run_outcome(run: &[Block]) -> (Step, usize) {
    let steps: Vec<Step> = run.iter().filter_map(|b| if let Block::Tool(t) = b { Some(t.status) } else { None }).collect();
    let failed = steps.iter().filter(|s| **s == Step::Failed).count();
    let step = if steps.iter().any(|s| matches!(s, Step::Waiting | Step::Working)) {
        Step::Working
    } else if failed == steps.len() || steps.last() == Some(&Step::Failed) {
        Step::Failed
    } else {
        Step::Done
    };
    (step, failed)
}

/// Where each run of tool calls shown folded starts in `blocks`.
fn folded_runs(blocks: &[Block]) -> Vec<usize> {
    let mut starts = vec![];
    let mut n = 0;
    while n < blocks.len() {
        if !matches!(blocks[n], Block::Tool(_)) {
            n += 1;
            continue;
        }
        let len = blocks[n..].iter().take_while(|b| matches!(b, Block::Tool(_))).count();
        if len >= MIN_RUN && run_folded(&blocks[n..n + len]) {
            starts.push(n);
        }
        n += len;
    }
    starts
}


/// Opens every block `flag` picks out, or closes them all when none is left to open. Returns
/// the first message that changed, if any.
fn toggle_all(messages: &mut [Msg], flag: fn(&mut Block) -> Option<&mut bool>) -> Option<usize> {
    let open = messages.iter_mut().any(|m| match m {
        Msg::Agent(blocks) => blocks.iter_mut().any(|b| flag(b).is_some_and(|f| !*f)),
        Msg::User(..) => false,
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

/// Where the file of a `file:` URL pasted in, dropped on it, is: the absolute path, its `%XX`
/// escapes decoded.
fn file_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // What follows the host (empty or `localhost`) is the path, with its leading slash.
    let path = match rest.find('/') {
        Some(at) if rest[..at].is_empty() || rest[..at].eq_ignore_ascii_case("localhost") => &rest[at..],
        _ => return None,
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let decoded = (bytes[at] == b'%').then(|| bytes.get(at + 1..at + 3)).flatten().and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match decoded {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The message of the conversation as the screen drawn by NSH shows it: the user's text with its
/// pictures, or the agent's blocks.
fn chat_items(messages: &[Msg]) -> Vec<nsh::chat::Item> {
    messages.iter().enumerate().map(|(ix, msg)| match msg {
        Msg::User(text, pictures) => nsh::chat::Item::User {
            text: text.clone(),
            pictures: pictures.iter().map(|a| nsh::chat::Picture { uri: a.data_uri(), size: a.size }).collect(),
        },
        Msg::Agent(blocks) => nsh::chat::Item::Agent { index: ix, blocks: chat_blocks(blocks) },
    }).collect()
}

/// Whether the run's calls waited, were all told to go, all went well, or one failed.
fn chat_phase(step: Step) -> nsh::chat::Phase {
    match step {
        Step::Waiting => nsh::chat::Phase::Waiting,
        Step::Working => nsh::chat::Phase::Working,
        Step::Done => nsh::chat::Phase::Done,
        Step::Failed => nsh::chat::Phase::Failed,
    }
}

/// The plan entry in progress in the latest plan, which a call that begins now works towards.
fn task_in_progress(messages: &[Msg]) -> Option<String> {
    let entries = messages.iter().rev().find_map(|msg| match msg {
        Msg::Agent(blocks) => blocks.iter().find_map(|b| if let Block::Plan(entries) = b { Some(entries) } else { None }),
        _ => None,
    })?;
    entries.iter().find(|(_, step)| *step == Step::Working).map(|(text, _)| text.clone())
}

/// The plan task a run of calls worked towards: the one task its calls name, none when they name
/// no task or span several.
fn run_task(run: &[Block]) -> Option<String> {
    let mut tasks = run.iter().filter_map(|b| if let Block::Tool(tool) = b { tool.task.as_deref() } else { None });
    let first = tasks.next()?;
    tasks.all(|task| task == first).then(|| first.to_string())
}

/// One agent message's blocks for the conversation drawn by NSH. Runs of [`MIN_RUN`] or more tool calls
/// also carry the row that folds them; the calls stay in place either way, so the indexes their
/// answers are told by hold.
fn chat_blocks(blocks: &[Block]) -> Vec<nsh::chat::Block> {
    let mut out: Vec<nsh::chat::Block> = Vec::with_capacity(blocks.len());
    let mut n = 0;
    while n < blocks.len() {
        let run = n > 0 && matches!(&blocks[n - 1], Block::Tool(_));
        let len = if matches!(blocks[n], Block::Tool(_)) && !run { blocks[n..].iter().take_while(|b| matches!(b, Block::Tool(_))).count() } else { 0 };
        if len >= MIN_RUN {
            let run = &blocks[n..n + len];
            let (phase, failed) = run_outcome(run);
            let task = run_task(run);
            out.push(nsh::chat::Block::Run { index: n, count: len, failed, phase: chat_phase(phase), folded: run_folded(run), task });
            for (k, block) in run.iter().enumerate() {
                if let Block::Tool(tool) = block {
                    out.push(nsh::chat::Block::Tool(chat_tool(n + k, tool)));
                }
            }
            n += len;
            continue;
        }
        out.push(match &blocks[n] {
            Block::Text(text) => nsh::chat::Block::Markdown(nsh::source::Shared(text.as_str().into())),
            Block::Thought(text, open) => nsh::chat::Block::Thought { index: n, text: text.clone(), open: *open },
            Block::Tool(tool) => nsh::chat::Block::Tool(chat_tool(n, tool)),
            Block::Permission(ask) => {
                let primary = primary_option(&ask.options);
                nsh::chat::Block::Ask {
                    index: n,
                    title: ask.title.clone(),
                    options: ask.options.iter().enumerate().map(|(at, option)| nsh::chat::AskOption {
                        at,
                        label: option.name.clone(),
                        primary: primary == Some(at),
                    }).collect(),
                    answered: ask.answered.clone(),
                }
            }
            Block::Plan(entries) => nsh::chat::Block::Plan(entries.iter().map(|(text, step)| nsh::chat::Entry { text: text.clone(), phase: chat_phase(*step) }).collect()),
            Block::Approve { agent, new_agent, mcp, resume: _, answered } => {
                let (question, lines) = approval_text(agent, *new_agent, mcp);
                nsh::chat::Block::Approve {
                    index: n,
                    question,
                    lines,
                    answered: answered.map(|said| match (said, new_agent) {
                        (true, _) => "Approved".into(),
                        (false, true) => "Not run".into(),
                        (false, false) => "Left out".into(),
                    }),
                }
            }
        });
        n += 1;
    }
    out
}

/// One tool call for the conversation drawn by NSH.
fn chat_tool(at: usize, tool: &ToolBlock) -> nsh::chat::Tool {
    nsh::chat::Tool {
        index: at,
        title: tool.title.clone(),
        phase: chat_phase(tool.status),
        open: tool.open,
        arguments: tool.arguments.clone(),
        result: tool.result.clone(),
        path: tool.path.clone(),
    }
}

/// What the approval card says: the question Po asks, and the commands it would hand over.
fn approval_text(agent: &config::Agent, new_agent: bool, mcp: &[config::McpServer]) -> (String, Vec<String>) {
    let mut lines = Vec::new();
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
    (question, lines)
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.settle_agent_changes();
        // A terminal that is no longer drawn must not keep the keys: they would go to a shell nobody sees.
        if !self.project.terminal_shown && self.project.terminal.as_ref().is_some_and(|terminal| terminal.focus_handle(cx).is_focused(window)) {
            let focus = self.focus.clone();
            cx.defer_in(window, move |_, window, cx| window.focus(&focus, cx));
        }
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
        // The window's controls live in the headers of the panes at its edges. Each button is one
        // pane and never changes what it means: the sidebar toggle at the left of the leftmost
        // pane, the viewer toggle in the viewer's header (or, while it is closed, in the
        // conversation's, on the side the viewer opens), and, with the viewer open, a button that
        // gives it the room by hiding the conversation.
        let focus = side_open && !agent_open;
        use nsh::header::Control;
        let controls = |lead: Vec<Control>, trail: Vec<Control>| *self.pane_controls.borrow_mut() = (lead, trail);
        let trail_all = vec![Control::Focus(focus), Control::Viewer(side_open)];
        // The viewer pane is only built where it shows: a pane nobody sees would publish over the
        // one that is.
        let hidden = || div().size_full().into_any_element();
        let (middle, side) = if self.issues_open {
            (self.issues.clone().into_any_element(), hidden())
        } else if !side_open {
            // Only the conversation shows, and the viewer's button waits on the side it opens.
            let chat_lead = vec![Control::Sidebar(left_open), Control::Viewer(side_open)];
            let chat_trail = vec![Control::Viewer(side_open)];
            let mut chat = |lead: Vec<Control>, trail: Vec<Control>| floating(self.chat_pane(lead, trail, cx), cx);
            if swapped {
                (chat(chat_lead, vec![]), hidden())
            } else {
                (chat(vec![Control::Sidebar(left_open)], chat_trail), hidden())
            }
        } else if focus {
            controls(vec![Control::Sidebar(left_open)], trail_all);
            (self.pane(cx), hidden())
        } else if swapped {
            controls(vec![Control::Sidebar(left_open)], trail_all);
            (self.pane(cx), floating(self.chat_pane(vec![], vec![], cx), cx))
        } else {
            controls(vec![], trail_all);
            (floating(self.chat_pane(vec![Control::Sidebar(left_open)], vec![], cx), cx), self.pane(cx))
        };
        let right_open = side_open && !focus && !self.issues_open;
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
            .on_action(cx.listener(|reader, _: &ShowFinder, window, cx| {
                reader.hide_issues(window, cx);
                reader.show_finder(window, cx)
            }))
            .on_action(cx.listener(|reader, _: &ShowSettings, window, cx| {
                reader.hide_issues(window, cx);
                reader.toggle_settings(window, cx)
            }))
            .on_action(cx.listener(|reader, _: &CloseSettings, window, cx| reader.hide_settings(window, cx)))
            .on_action(cx.listener(|reader, _: &ToggleTools, _, cx| reader.toggle_tools(cx)))
            .on_action(cx.listener(|reader, _: &ToggleThoughts, _, cx| reader.toggle_thoughts(cx)))
            .on_action(cx.listener(|reader, _: &ToggleLeft, _, cx| reader.toggle_left(cx)))
            .on_action(cx.listener(|reader, _: &ToggleRight, _, cx| reader.toggle_right(cx)))
            .on_action(cx.listener(|reader, _: &ShowSource, _, cx| reader.show_viewer(Mode::Source, cx)))
            .on_action(cx.listener(|reader, _: &ShowChanges, _, cx| reader.show_viewer(Mode::Diff, cx)))
            .on_action(cx.listener(|reader, _: &ShowChat, _, cx| reader.show_chat(cx)))
            .on_action(cx.listener(|reader, _: &SwapPanes, _, cx| reader.swap_panes(cx)))
            .on_action(cx.listener(|reader, _: &ToggleFocus, _, cx| reader.toggle_focus(cx)))
            .on_action(cx.listener(|reader, _: &ToggleTerminal, window, cx| reader.toggle_terminal(window, cx)))
            .on_action(cx.listener(|reader, _: &ShowHistory, window, cx| reader.resume_thread(window, cx)))
            .on_action(cx.listener(|reader, _: &ToggleSplit, _, cx| reader.set_split(!reader.split, cx)))
            .on_action(cx.listener(|reader, _: &ToggleRendered, _, cx| reader.toggle_rendered(cx)))
            .on_action(cx.listener(|reader, _: &OpenProject, window, cx| {
                reader.hide_issues(window, cx);
                reader.pick_project(window, cx)
            }))
            .on_action(cx.listener(|reader, _: &NewThread, window, cx| {
                reader.hide_issues(window, cx);
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
            .when(self.finder_open, |this| this.child(self.finder.clone()))
            .when(self.settings_open, |this| this.child(self.settings.clone()))
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

/// Height of the row holding the viewer's tabs.
const TABS_HEIGHT: gpui::Rems = rems(2.5);

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
    if cli.first().is_some_and(|arg| arg == "ctl") {
        if let Err(error) = ctl::run(&cli[1..]) {
            eprintln!("po ctl: {error:#}");
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
        cx.bind_keys([
            KeyBinding::new("ctrl-o", ToggleTools, None),
            KeyBinding::new("ctrl-t", ToggleThoughts, None),
            KeyBinding::new("secondary-b", ToggleLeft, None),
            KeyBinding::new("secondary-shift-b", ToggleRight, None),
            KeyBinding::new("secondary-1", ShowSource, None),
            KeyBinding::new("secondary-2", ShowChanges, None),
            KeyBinding::new("secondary-3", ShowChat, None),
            KeyBinding::new("secondary-shift-s", SwapPanes, None),
            KeyBinding::new("secondary-shift-f", ToggleFocus, None),
            KeyBinding::new("secondary-shift-h", ShowHistory, None),
            KeyBinding::new("secondary-\\", ToggleSplit, None),
            KeyBinding::new("secondary-shift-m", ToggleRendered, None),
            KeyBinding::new("secondary-n", NewThread, None),
            KeyBinding::new("secondary-shift-o", OpenProject, None),
            KeyBinding::new("secondary-p", ShowFinder, None),
            KeyBinding::new("secondary-,", ShowSettings, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
            KeyBinding::new("secondary-`", ToggleTerminal, None),
            KeyBinding::new("escape", CloseSettings, Some("Settings")),
            // The Input binds up/down itself and swallows them when single-line, so these name it.
        ]);
        // Ctrl+O, T, B, N, P, W and \ are keys of readline, fzf and tmux: while the terminal has the
        // focus they go to the shell and not to the window. (Where the window's keys are Cmd's, only
        // the first two are Ctrl's.)
        let shell_keys: &[&str] = if cfg!(target_os = "macos") { &["ctrl-o", "ctrl-t"] } else { &["ctrl-o", "ctrl-t", "ctrl-b", "ctrl-n", "ctrl-p", "ctrl-w", "ctrl-\\"] };
        cx.bind_keys(shell_keys.iter().map(|key| KeyBinding::new(key, gpui::NoAction, Some("Terminal"))));
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
        nsh::init();
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

/// What a thread is doing, in a line: the question it waits on, the tool it is
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
    fn a_thread_title_opens_with_a_capital_unless_it_is_code() {
        let cap = |t: &str| super::capitalized(t.to_string());
        assert_eq!(cap("fix the build"), "Fix the build");
        assert_eq!(cap("why, though?"), "Why, though?");
        assert_eq!(cap("Already fine"), "Already fine");
        for as_typed in ["src/main.rs is slow", "cargo test fails", "snake_case rename", "v2 plan", "#115 review", "…", ""] {
            assert_eq!(cap(as_typed), as_typed);
        }
    }

    use super::{Queue, Where, ago, route, run_task, task_in_progress};

    #[test]
    fn the_sidebar_says_how_long_ago_in_a_word() {
        let now = 1_800_000_000_000;
        let before = |ms: u64| ago(now, now - ms);
        assert_eq!(before(30_000), None);
        assert_eq!(before(5 * 60_000).as_deref(), Some("5m"));
        assert_eq!(before(3 * 3_600_000).as_deref(), Some("3h"));
        assert_eq!(before(2 * 86_400_000).as_deref(), Some("2d"));
        assert_eq!(before(21 * 86_400_000).as_deref(), Some("3w"));
        assert_eq!(before(120 * 86_400_000).as_deref(), Some("4mo"));
        // A counter from before the time was kept, and a time in the future, say nothing.
        assert_eq!(ago(now, 7), None);
        assert_eq!(ago(now, now + 1), None);
    }

    #[test]
    fn a_project_remembers_the_threads_closed_in_it_newest_first_once_each_and_not_too_many() {
        let mut project = super::Reader::build_project(std::path::PathBuf::from("/x"), None, vec![]);
        let state = |session: &str| crate::workspace::ThreadState { agent: "a".into(), session: session.into(), title: String::new(), active_at: 0, parent_session: None };
        for n in 0..15 {
            project.remember(state(&format!("s{n}")));
        }
        project.remember(state("s10"));
        assert_eq!(project.closed.len(), crate::workspace::CLOSED_KEPT);
        assert_eq!(project.closed[0].session, "s10");
        assert_eq!(project.closed.iter().filter(|c| c.session == "s10").count(), 1);
        assert_eq!(project.closed[1].session, "s14");
    }

    #[test]
    fn a_blank_thread_or_one_without_a_session_is_not_remembered_when_closed() {
        let mut thread = super::Thread::blank(1);
        assert!(thread.closed_state().is_none(), "blank");
        thread.blank = false;
        thread.agent = Some("a".into());
        assert!(thread.closed_state().is_none(), "no session");
        thread.session = Some("s".into());
        assert_eq!(thread.closed_state().map(|c| c.session), Some("s".to_string()));
    }

    // A stand-in agent that copies what it is sent to a file, to see that a running thread's
    // agent is asked to stop before it is killed, and that an idle one is simply let go.
    fn closed_with(busy: bool) -> String {
        let path = std::env::temp_dir().join(format!("po-retire-{}-{busy}", std::process::id()));
        std::fs::write(&path, "").unwrap();
        let (client, _events) = crate::acp::Client::spawn(&format!("tee {}", path.display()), &std::env::temp_dir(), false, |_| vec![]).expect("spawns tee");
        client.load("s1");
        let mut thread = super::Thread::blank(1);
        thread.busy = busy;
        thread.client = Some(crate::agent::Client::Acp(client));
        super::Thread::retire(thread);
        // Waits for the cancel to arrive; when none is expected, long enough to have seen it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(if busy { 3000 } else { 400 });
        while std::time::Instant::now() < deadline && !(busy && std::fs::read_to_string(&path).unwrap().contains("session/cancel")) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let sent = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        sent
    }

    #[test]
    fn a_running_thread_is_asked_to_stop_when_closed_and_an_idle_one_is_not() {
        assert!(closed_with(true).contains("session/cancel"));
        assert!(!closed_with(false).contains("session/cancel"));
    }

    #[test]
    fn only_a_thread_with_work_to_lose_asks_before_closing() {
        let mut thread = super::Thread::blank(1);
        assert!(!thread.at_risk(), "blank");
        thread.blank = false;
        assert!(!thread.at_risk(), "idle");
        thread.busy = true;
        assert!(thread.at_risk(), "running");
        thread.busy = false;
        thread.queue.push(super::Queued { out: super::Outgoing { text: "next".into(), origin: super::Origin::User, images: vec![] }, kind: None });
        assert!(thread.at_risk(), "queued");
    }

    // submit's paths (§4.3 of the control plan): blank, unstarted, starting, asleep, ready,
    // busy for an agent that takes messages mid-turn (dex) and one that doesn't (ACP).
    #[test]
    fn a_message_goes_where_its_thread_is() {
        assert_eq!(route(true, false, false, false, false, false, false), Where::Start, "blank");
        assert_eq!(route(false, true, false, false, false, false, false), Where::Start, "unstarted");
        assert_eq!(route(false, false, false, true, false, false, false), Where::Wait, "starting");
        assert_eq!(route(false, false, true, false, false, false, false), Where::Wait, "asleep");
        assert_eq!(route(false, false, false, false, false, false, false), Where::Dispatch, "ready");
        assert_eq!(route(false, false, false, false, true, true, false), Where::Take(Queue::Steer), "dex mid-turn");
        assert_eq!(route(false, false, false, false, true, true, true), Where::Take(Queue::Followup), "dex follow-up");
        assert_eq!(route(false, false, false, false, true, false, false), Where::Queue, "ACP mid-turn");
        // A thread that is busy is not starting anything, however the rest stands.
        assert_eq!(route(false, false, false, true, true, false, false), Where::Queue);
    }

    #[test]
    fn the_file_tree_needs_a_pane_of_at_least_the_minimum_width() {
        use super::{TREE_MIN_PANE, tree_fits};
        assert!(tree_fits(TREE_MIN_PANE));
        assert!(!tree_fits(TREE_MIN_PANE - 1.0));
    }

    #[test]
    fn an_empty_viewer_says_how_to_get_the_tree_back() {
        use super::pick_hint;
        assert_eq!(pick_hint("a file", true), "Pick a file in the tree");
        assert!(pick_hint("a file", false).ends_with("back to the explorer"));
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
            Msg::User("hi".into(), vec![]),
            Msg::Agent(vec![Block::Thought("a".into(), true), Block::Text("t".into())]),
            Msg::Agent(vec![Block::Thought("b".into(), false)]),
        ];
        assert_eq!(toggle_all(&mut messages, thought_flag), Some(2));
        assert!(matches!(&messages[1], Msg::Agent(b) if matches!(b[0], Block::Thought(_, true))));
        assert!(matches!(&messages[2], Msg::Agent(b) if matches!(b[0], Block::Thought(_, true))));
        assert_eq!(toggle_all(&mut messages, thought_flag), Some(1));
        assert!(matches!(&messages[1], Msg::Agent(b) if matches!(b[0], Block::Thought(_, false))));
        assert_eq!(toggle_all(&mut [Msg::User("x".into(), vec![])], thought_flag), None);
    }

    use super::{NOTICE_RATE, notice_wait, Activity, Block, Msg, PermissionOption, Step, ToolBlock, folded_runs, preview_of, language, run_folded, toggle_all, match_files, primary_option};

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
    fn elixir_files_are_named() {
        assert_eq!(language("lib/app.ex"), "Elixir");
        assert_eq!(language("lib/app.exs"), "Elixir");
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

    fn tool_for(task: Option<&str>) -> Block {
        Block::Tool(ToolBlock { id: String::new(), title: "call".into(), status: Step::Done, open: false, fold: None, task: task.map(str::to_string), arguments: String::new(), result: String::new(), path: None })
    }

    #[test]
    fn a_run_is_named_by_the_one_task_its_calls_work_towards() {
        assert_eq!(run_task(&[tool_for(None), tool_for(Some("Fix it")), tool_for(Some("Fix it"))]), Some("Fix it".to_string()));
        assert_eq!(run_task(&[tool_for(None), tool_for(None)]), None);
        assert_eq!(run_task(&[tool_for(Some("A")), tool_for(Some("B")), tool_for(Some("B"))]), None);
    }

    #[test]
    fn a_call_takes_the_task_in_progress_in_the_latest_plan() {
        let mut messages = vec![Msg::Agent(vec![Block::Plan(vec![("Read".into(), Step::Done), ("Fix it".into(), Step::Working)])])];
        assert_eq!(task_in_progress(&messages), Some("Fix it".to_string()));
        messages.push(Msg::User("go on".into(), vec![]));
        messages.push(Msg::Agent(vec![Block::Plan(vec![("Fix it".into(), Step::Done)])]));
        assert_eq!(task_in_progress(&messages), None);
        assert_eq!(task_in_progress(&[]), None);
    }

    fn tool(title: &str, status: Step) -> Block {
        Block::Tool(ToolBlock { id: String::new(), title: title.into(), status, open: false, fold: None, task: None, arguments: String::new(), result: String::new(), path: None })
    }

    #[test]
    fn a_run_of_calls_starts_folded_even_while_it_runs_or_fails() {
        let run = vec![tool("a", Step::Done), tool("b", Step::Done), tool("c", Step::Done)];
        assert!(run_folded(&run));
        let failed = vec![tool("a", Step::Done), tool("b", Step::Failed), tool("c", Step::Done)];
        assert!(run_folded(&failed));
        let running = vec![tool("a", Step::Done), tool("b", Step::Working), tool("c", Step::Waiting)];
        assert!(run_folded(&running));
    }

    #[test]
    fn a_parent_never_told_is_told_now_and_one_just_told_waits() {
        use std::time::Duration;
        assert_eq!(notice_wait(None), None);
        assert_eq!(notice_wait(Some(NOTICE_RATE)), None);
        assert_eq!(notice_wait(Some(NOTICE_RATE + Duration::from_secs(1))), None);
        assert_eq!(notice_wait(Some(Duration::ZERO)), Some(NOTICE_RATE));
        assert_eq!(notice_wait(Some(Duration::from_secs(4))), Some(NOTICE_RATE - Duration::from_secs(4)));
    }

    #[test]
    fn a_click_on_a_run_overrides_the_default() {
        let mut run = vec![tool("a", Step::Done), tool("b", Step::Done), tool("c", Step::Done)];
        let Block::Tool(first) = &mut run[0] else { unreachable!() };
        first.fold = Some(false);
        assert!(!run_folded(&run));
        let mut failed = vec![tool("a", Step::Failed), tool("b", Step::Done), tool("c", Step::Done)];
        let Block::Tool(first) = &mut failed[0] else { unreachable!() };
        first.fold = Some(true);
        assert!(run_folded(&failed));
    }

    #[test]
    fn only_runs_of_two_or_more_are_found_folded() {
        let blocks = vec![
            tool("a", Step::Done),
            Block::Text("x".into()),
            tool("b", Step::Done),
            tool("c", Step::Done),
        ];
        assert_eq!(folded_runs(&blocks), vec![2]);
    }

    #[test]
    fn a_run_followed_by_text_folds_in_the_latest_message() {
        let blocks = vec![tool("a", Step::Done), tool("b", Step::Done), tool("c", Step::Done), Block::Text("x".into())];
        assert_eq!(folded_runs(&blocks), vec![0]);
        assert_eq!(folded_runs(&blocks[..3]), vec![0]);
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
    fn only_a_question_or_an_unseen_turn_needs_you() {
        assert!(Activity::Asking.needs_you(false) && Activity::Unread.needs_you(false));
        assert!(!Activity::Running.needs_you(false) && !Activity::Idle.needs_you(false));
        // A child's parent picks up its finished turn; a question is still the user's.
        assert!(!Activity::Unread.needs_you(true) && Activity::Asking.needs_you(true));
    }
}

#[cfg(test)]
mod file_url_tests {
    use super::file_of;

    #[test]
    fn file_urls_become_absolute_paths() {
        assert_eq!(file_of("file:///home/aks/a.png").as_deref(), Some("/home/aks/a.png"));
        assert_eq!(file_of("file://localhost/tmp/a%20b.png").as_deref(), Some("/tmp/a b.png"));
        assert_eq!(file_of("file://other/tmp/a.png"), None);
        assert_eq!(file_of("https://example.com/a.png"), None);
    }
}
