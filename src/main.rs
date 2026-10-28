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
mod schemes;
mod settings;
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
    AnyElement, App, Pixels, TitlebarOptions, WindowControlArea, point, Bounds, Context, Entity, IntoElement, ParentElement,
    Render, Styled, Subscription, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, div,
    prelude::*, px, rems, size, KeyBinding,
};

gpui::actions!(po, [ToggleTools, ToggleThoughts, ToggleLeft, ToggleRight, ShowSource, ShowChanges, ShowChat, SwapPanes, ShowHistory, ToggleSplit, ToggleRendered, NewThread, OpenProject, ShowFinder, ToggleFocus, ShowSettings, CloseSettings, CloseWindow]);

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

/// Where text headed for an agent came from, so that it is labelled as coming from there at
/// every step: sent at once, waiting for the agent, queued, or taken back. It never ends up in
/// the user's draft, and it never claims "the user has `x` open" when the user didn't send it.
/// The thread and Po origins are produced from phase 2 on (`thread.send`, notify-parent).
#[allow(dead_code)]
enum Origin {
    /// The user's draft.
    User,
    /// Another thread of this window, sending through the API (phase 2's `thread.send`).
    Thread { id: u64, title: String },
    /// Po itself, telling an orchestrator what its child did (phase 2's notify-parent).
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
    /// Waiting on the user: a question to answer, or a finished turn nobody has looked at, unless
    /// `parent_hears` of it, since then the parent agent picks it up.
    fn needs_you(self, parent_hears: bool) -> bool {
        self == Activity::Asking || (self == Activity::Unread && !parent_hears)
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

struct Reader {
    /// How agents started from this window reach it; none where there is no socket.
    control: Option<control::Control>,
    /// What each thread's agent may do, held in memory only: it is lost on restart and is not
    /// shared between windows, so nothing an agent can read or write raises its own reach.
    /// A thread without an entry has [`command::Grant::default`], and the grants can only be
    /// changed from the UI, never from anything an agent could write to (phase 1 adds the menu).
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
    chat_marks: std::cell::Cell<((u64, u64), usize, usize, bool, usize)>,
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
        let finder_window = cx.new(|cx| nsh::overlay::Overlay::new(560., 8., || nsh_platform_gpui::Root::of::<nsh::finder::Finder>(), &nsh::finder::HEIGHT, cx));
        let settings_window = cx.new(|cx| nsh::overlay::Overlay::new(680., 24., || nsh_platform_gpui::Root::of::<nsh::settings::Settings>(), &nsh::settings::HEIGHT, cx));
        let issues = cx.new(|cx| nsh::overlay::Overlay::page(1000., || nsh_platform_gpui::Root::of::<nsh::issues::Issues>(), &nsh::issues::HEIGHT, cx));
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
            let thread = Thread {
            views: Default::default(),
            forms: Default::default(),
            shown: None,
            offered: None,
            id: 0,
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
            draft: String::new(),
            attachments: vec![],
            attach_note: None,
        };
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
        if let Some(mut requests) = requests {
            let handle = window.window_handle();
            cx.spawn(async move |this, cx| {
                while let Some(request) = requests.next().await {
                    let answered = handle.update(cx, |_, window, cx| {
                        this.update(cx, |reader, cx| {
                            if matches!(request.command, command::Command::EventsSubscribe) {
                                let authorized = reader.authorize(request.caller, &request.command);
                                reader.record(request.caller, "events.subscribe", authorized.as_ref().map_or_else(|why| format!("refused: {why}"), |()| "ok".into()), true);
                                match authorized {
                                    Ok(()) => {
                                        request.reply.send(Ok(serde_json::json!({ "subscribed": true }))).ok();
                                        reader.subscribers.push(request.reply);
                                    }
                                    Err(why) => {
                                        request.reply.send(Err(why)).ok();
                                    }
                                }
                                return;
                            }
                            match reader.run_command(request.caller, request.command, window, cx) {
                                command::Answer::Now(reply) => {
                                    request.reply.send(reply).ok();
                                }
                                command::Answer::Later(wait) => {
                                    reader.add_waiter(request.caller, wait, request.reply, window, cx);
                                }
                            };
                        })
                    });
                    if answered.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        // What the user does on a screen drawn by NSH, run here where the window and the agents are.
        if let Some(mut commands) = nsh::bus::commands() {
            let handle = window.window_handle();
            cx.spawn(async move |this, cx| {
                while let Some(cmd) = commands.recv().await {
                    let ran = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.run(cmd, window, cx)));
                    if !matches!(ran, Ok(Ok(()))) {
                        break;
                    }
                }
            })
            .detach();
        }
        // The sidebar's count of issues follows them, whoever changes them.
        let mut issue_changes = issues::shared::watch();
        cx.spawn(async move |this, cx| {
            while issue_changes.changed().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
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
        let sweep = reader.window;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                let alive = sweep.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.sleep_idle(window, cx)));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        reader
    }

    /// A project over `root`, showing `files` in its explorer until the next walk.
    fn build_project(root: PathBuf, pr: Option<pr::Pr>, files: Vec<String>) -> Project {
        let files_tree = FileTree::new(false);
        let changes_tree = FileTree::new(true);
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
            versions: None,
            counts: (0, 0),
            current: None,
            stamp: None,
            code: None,
            is_markdown: false,
            notice: None,
            files_tree,
            worktree: None,
        }
    }

    /// Rebuilds the explorer from `files`, keeping folders open and the current file selected.
    fn refresh_tree(&mut self) {
        self.project.files_tree.set_paths(&self.project.files, self.project.current.as_deref());
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
            self.project.changes_tree.set_paths(&paths, None);
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
                    self.set_rows(vec![], None);
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
        self.refresh_tree();
        if self.finder_open {
            self.refind();
        }
        cx.notify();
    }

    /// Shows the diff `rows` colored by `colors`, and works out their side-by-side pairs and counts once.
    fn set_rows(&mut self, rows: Vec<Row>, versions: Option<(String, String)>) {
        self.project.counts = rows.iter().fold((0, 0), |(a, r), row| match row.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            _ => (a, r),
        });
        self.project.rows = Rc::new(rows);
        self.project.versions = versions.map(|(old, new)| (nsh::source::Shared(Arc::from(old)), nsh::source::Shared(Arc::from(new))));
    }

    /// Shows `path`'s diff in the viewer, once `git` has produced it off the UI thread.
    fn pick(&mut self, path: &str, cx: &mut Context<Self>) {
        self.browse = false;
        let Some(change) = self.project.changes.iter().find(|c| c.path == path).cloned() else { return };
        if self.project.picked.as_deref() != Some(path) {
            self.project.picked = Some(path.to_string());
            // The last file's diff isn't shown under this one's name while this one loads.
            self.set_rows(vec![], None);
            self.project.changes_tree.select(path);
        }
        let (root, base, path) = (self.project.root.clone(), self.project.base.clone(), path.to_string());
        self.project.diffing = Some(cx.spawn(async move |this, cx| {
            let (rows, versions) = cx
                .background_executor()
                .spawn(async move {
                    let rows = diff::file_diff(&root, &base, &change);
                    // Only files with changed lines need their versions.
                    let versions = (!rows.is_empty()).then(|| {
                        let (old, new) = diff::versions(&root, &base, &change);
                        (old.unwrap_or_default(), new.unwrap_or_default())
                    });
                    (rows, versions)
                })
                .await;
            this.update(cx, |reader, cx| {
                if reader.project.picked.as_deref() == Some(&path) {
                    reader.set_rows(rows, versions);
                    reader.project.diffing = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Starts checking out the PR `spec` the strip asked for; the window switches to it when
    /// that finishes.
    fn open_pr(&mut self, spec: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = spec.trim().to_string();
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
                                // The strip's field clears itself on seeing a bumped serial.
                                reader.pr_serial += 1;
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
        self.refresh_tree();
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
        let project = Self::build_project(root, pr, vec![]);
        self.open_project(project, if review { Mode::Diff } else { Mode::Source }, agent, window, cx);
    }

    /// Shows `project` in `mode`, with a thread of its own that runs `agent`, if any. The
    /// project on screen goes on running in the background.
    fn open_project(&mut self, project: Project, mode: Mode, agent: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.blank_thread();
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
    fn new_worktree_thread(&mut self, key: &str, first: Option<(Outgoing, (u64, u64))>, window: &mut Window, cx: &mut Context<Self>) {
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
                                reader.issue_for_worktree = None;
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

    fn open_worktree(&mut self, wt: worktree::Worktree, agent: String, first: Option<(Outgoing, (u64, u64))>, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("worktree: {} for {agent}", wt.spec.branch);
        if let Some((root, number)) = self.issue_for_worktree.take() {
            Self::issue_started(&root, number, Some(wt.spec.branch.clone()));
        }
        let mut project = Self::build_project(wt.spec.path.clone(), None, vec![]);
        project.base = wt.spec.base.clone();
        project.worktree = Some(wt);
        self.confirm_discard = false;
        self.open_project(project, Mode::Diff, Some(agent), window, cx);
        if let Some((out, (pid, tid))) = first {
            self.begin_turn(&out);
            self.thread.pending = Some(out);
            // The thread it was typed in stays only if something has happened to it since.
            let blank = self.views().iter().any(|v| v.id == pid && v.threads.iter().any(|t| t.id == tid && t.blank));
            if blank {
                self.close_any_thread(pid, tid, window, cx);
            }
        }
    }

    /// Merges the worktree on screen into the checkout it came from, or throws it away.
    fn finish_worktree(&mut self, merge: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.worktree.is_none() || self.finishing {
            return;
        }
        if !merge && !self.confirm_discard {
            self.confirm_discard = true;
            cx.notify();
            return;
        }
        self.confirm_discard = false;
        self.finish_worktree_of(self.project_id, merge, None, window, cx);
    }

    /// [`Self::finish_worktree`] for project `id`, on screen or not, once it is decided. With
    /// `tell`, that thread hears how it went.
    fn finish_worktree_of(&mut self, id: u64, merge: bool, tell: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.project_by_id(id).and_then(|project| project.worktree.as_ref()).map(|wt| wt.spec.clone()) else {
            if let Some(thread) = tell {
                self.tell(thread, format!("Project {id} is not a worktree any more."), window, cx);
            }
            return;
        };
        if self.finishing {
            if let Some(thread) = tell {
                self.tell(thread, "The user agreed to merge, but another worktree was being made or merged; ask again.".into(), window, cx);
            }
            return;
        }
        self.finishing = true;
        self.worktree_status = Some(if merge { "Merging…" } else { "Discarding…" }.into());
        let handle = window.window_handle();
        let title = self.views().iter().find(|view| view.id == id).and_then(|view| view.threads.iter().find(|thread| thread.id == view.shown)).map(|thread| thread.title()).unwrap_or_default();
        let message = format!("po: {title}");
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
                        let told = match result {
                            Ok(into) => {
                                // A merged branch finishes the issues linked to it.
                                let landed = into.is_some().then(|| reader.project_by_id(id).and_then(|p| p.worktree.as_ref()).map(|wt| (wt.spec.repo.clone(), wt.spec.branch.clone()))).flatten();
                                if let Some((repo, branch)) = landed {
                                    issues::shared::branch_merged(&repo, &branch);
                                }
                                reader.close_worktree_project(id, window, cx);
                                reader.api_projects.remove(&id);
                                let status = match into {
                                    Some(into) => format!("Merged into {into}"),
                                    None => "Discarded".into(),
                                };
                                reader.worktree_status = Some(status.clone());
                                status
                            }
                            Err(error) => {
                                log::warn!("worktree: {error}");
                                reader.worktree_status = Some(error.clone());
                                format!("Not merged: {error}")
                            }
                        };
                        if let Some(thread) = tell {
                            reader.tell(thread, told, window, cx);
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
    fn run_command(&mut self, caller: command::Caller, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Answer {
        let (name, writes) = (command.name(), command.writes());
        let answer = self.run_command_inner(caller, command, window, cx);
        let outcome = match &answer {
            command::Answer::Now(Ok(_)) => "ok".to_string(),
            command::Answer::Now(Err(why)) => format!("refused: {why}"),
            command::Answer::Later(_) => "waiting".to_string(),
        };
        self.record(caller, name, outcome, writes);
        answer
    }

    fn run_command_inner(&mut self, caller: command::Caller, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Answer {
        log::info!("control: {caller:?} {}", command.name());
        if let Err(why) = self.authorize(caller, &command) {
            log::warn!("control: {caller:?} {} refused: {why}", command.name());
            return command::Answer::Now(Err(why));
        }
        // The user's connection has no thread of its own; its commands act on the one on screen.
        let id = match caller {
            command::Caller::Thread(id) => id,
            command::Caller::User => self.thread.id,
        };
        match command.family() {
            command::Family::View => command::Answer::Now(self.run_view(id, command, window, cx)),
            command::Family::Project if command.writes() => command::Answer::Now(self.run_project(id, command, window, cx)),
            command::Family::Control | command::Family::Agent | command::Family::Project | command::Family::Window => command::Answer::Now(self.run_read(caller, id, command)),
            command::Family::Thread => self.run_thread(caller, id, command, window, cx),
        }
    }

    /// Whether `caller` may run `command` at all: what it has been granted, and how the thread
    /// the command names stands to the caller's own.
    fn authorize(&self, caller: command::Caller, command: &command::Command) -> Result<(), String> {
        let (grant, id) = match caller {
            command::Caller::User => (command::Grant::user(), None),
            command::Caller::Thread(id) => (self.grants.get(&id).copied().unwrap_or_default(), Some(id)),
        };
        if command.user_only() && id.is_some() {
            return Err("only the user, from `po ctl`, may ask this".into());
        }
        if command.needs_parent() && id.is_none() {
            return Err("starting, steering and waiting on threads is an agent's; `po ctl` can read, answer, stop and close".into());
        }
        // The user has no thread of their own, so nothing stands between them and a target.
        let relation = match command.target() {
            Some(target) => match id {
                Some(id) => self.relation_to(id, target),
                None => command::Relation::Own,
            },
            None => command::Relation::Own,
        };
        grant.allows(command, relation).then_some(()).ok_or_else(|| "the user has not granted this".to_string())
    }

    /// How `target` stands to thread `caller`: itself, its child (phase 2 starts children), a
    /// sibling thread of its project, or a thread elsewhere.
    fn relation_to(&self, caller: u64, target: u64) -> command::Relation {
        use command::Relation;
        if target == caller {
            return Relation::Own;
        }
        if self.threads_all().any(|thread| thread.id == target && thread.parent == Some(caller)) {
            return Relation::Child;
        }
        // A sibling stands in the same project; a view holding both says so.
        let projects: Vec<Vec<u64>> = self.views().iter().map(|view| view.threads.iter().map(|thread| thread.id).collect()).collect();
        if state::share_project(&projects, caller, target) {
            Relation::Sibling
        } else {
            Relation::Elsewhere
        }
    }

    /// Every thread of every project this window has.
    fn threads_all(&self) -> impl Iterator<Item = &Thread> {
        std::iter::once(&self.thread).chain(self.parked.iter()).chain(self.slots.iter().flat_map(|slot| std::iter::once(&slot.thread).chain(slot.parked.iter())))
    }

    /// The view family: what the caller's thread puts on screen, and the viewer it may move
    /// while its thread is the one being looked at.
    fn run_view(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        use command::Command;
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
            let shows = matches!(command, Command::ViewShow { .. } | Command::FormShow { .. });
            self.with_thread(id, |reader, visible| {
                let reply = match &command {
                    Command::FormShow { id, title, body, fields, submit } => {
                        // A replaced form is filled in again from scratch; the screen seeds its
                        // answers anew wherever the fields (which carry the starting values)
                        // changed.
                        reader.thread.forms.show(id.clone(), title.clone(), body.clone(), fields.clone(), submit.clone())?
                    }
                    _ => reader.thread.views.apply(command.clone())?,
                };
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

    /// The read families: snapshots of this window's threads, agents and projects. They look
    /// and never swap a thread in to change it — every write is a phase-2 command, and the
    /// reads answer from the state as it is.
    fn run_read(&self, caller: command::Caller, id: u64, command: command::Command) -> command::Reply {
        use command::{Command, Scope};
        use serde_json::json;
        let grant = self.grant_of(caller);
        // Everything, for the user's connection; otherwise what it was granted.
        let everywhere = matches!(caller, command::Caller::User) || grant.scope == Scope::Global;
        match command {
            Command::ControlInfo => {
                let scope = match grant.scope {
                    Scope::Viewer => "viewer",
                    Scope::Orchestrate => "orchestrate",
                    Scope::Global => "global",
                };
                Ok(json!({
                    "grant": { "scope": scope, "read_project": grant.read_project, "approvals": grant.approvals },
                    "thread": id,
                    "window": std::process::id(),
                }))
            }
            Command::AuditList { limit } => {
                let skip = self.audit.len().saturating_sub(limit.unwrap_or(50).clamp(1, MAX_AUDIT));
                let entries: Vec<Value> = self
                    .audit
                    .iter()
                    .skip(skip)
                    .map(|entry| {
                        let who = match entry.caller {
                            command::Caller::User => json!("user"),
                            command::Caller::Thread(id) => json!({ "thread": id }),
                        };
                        json!({ "at": entry.at, "caller": who, "command": entry.command, "outcome": entry.outcome, "count": entry.count })
                    })
                    .collect();
                Ok(json!({ "entries": entries }))
            }
            Command::WindowInfo => {
                let projects: Vec<Value> = self.views().iter().map(|view| self.project_info(view).json()).collect();
                Ok(json!({ "pid": std::process::id(), "projects": projects, "on_screen": self.project_id }))
            }
            Command::ThreadFind { session } => {
                let views = self.views();
                let found = views.iter().flat_map(|view| view.threads.iter().map(move |thread| (view, *thread))).find(|(_, thread)| thread.session.as_deref() == Some(session.as_str()));
                let (_, thread) = found.ok_or("no thread has that session")?;
                // The same reach as reading it by id, and the same answer when it is out of
                // reach as when there is none, so that this doesn't say which sessions exist.
                if !grant.allows(&Command::ThreadGet { thread: Some(thread.id) }, self.relation_to(id, thread.id)) {
                    return Err("no thread has that session".into());
                }
                Ok(json!({ "thread": thread.id }))
            }
            Command::AgentList => {
                let agents: Vec<Value> = self
                    .agents
                    .iter()
                    .map(|agent| state::AgentInfo { id: agent.id.clone(), name: agent.name.clone(), trusted: config::trusted(agent), daemon: agent.daemon.is_some() }.json())
                    .collect();
                Ok(json!({ "agents": agents, "default": self.chosen_agent }))
            }
            Command::Issues { op, args } => self.run_issues(id, op, &args),
            Command::ProjectList => {
                let own = self.own_project(id);
                let projects: Vec<Value> = self
                    .views()
                    .iter()
                    .filter(|view| everywhere || own == Some(view.id))
                    .map(|view| self.project_info(view).json())
                    .collect();
                Ok(json!({ "projects": projects }))
            }
            Command::ProjectChanges { project, path } => {
                let own = self.own_project(id);
                let at = project.or(own).ok_or("that thread is gone")?;
                let views = self.views();
                let view = views.iter().find(|view| view.id == at).ok_or("no such project")?;
                if own != Some(at) && !everywhere {
                    return Err("another project's changes are read at Global".into());
                }
                // Read from the repository now: a project that was never on screen has not had
                // its changes listed, and an agent's isolated worktree is just that.
                let changes = diff::changes(&view.project.root, &view.project.base);
                match path {
                    Some(path) => {
                        let change = changes.iter().find(|change| change.path == path).ok_or(format!("`{path}` has no changes"))?;
                        let rows = diff::file_diff(&view.project.root, &view.project.base, change);
                        let mut text = String::new();
                        for row in rows {
                            if text.len() > state::MAX_DIFF {
                                text.truncate(state::MAX_DIFF);
                                text.push_str("\n…");
                                break;
                            }
                            text.push_str(&match row.kind {
                                diff::Kind::Hunk => format!("{}\n", row.text),
                                diff::Kind::Added => format!("+{}\n", row.text),
                                diff::Kind::Removed => format!("-{}\n", row.text),
                                diff::Kind::Context => format!(" {}\n", row.text),
                            });
                        }
                        Ok(json!({ "path": path, "diff": text }))
                    }
                    None => Ok(json!({
                        "files": changes.iter().map(|change| json!({ "path": change.path, "status": change.status })).collect::<Vec<_>>(),
                    })),
                }
            }
            Command::ThreadList { project, state } => {
                let own = self.own_project(id);
                let want = state.as_deref().and_then(crate::state::ThreadState::from_name);
                let mut threads = Vec::new();
                for view in self.views() {
                    if !(everywhere || own == Some(view.id)) || project.is_some_and(|want| want != view.id) {
                        continue;
                    }
                    for thread in &view.threads {
                        // The same reach table as every other read: itself, its children, and
                        // its project's other threads only with `read_project`.
                        if !grant.allows(&Command::ThreadGet { thread: Some(thread.id) }, self.relation_to(id, thread.id)) {
                            continue;
                        }
                        let info = self.thread_info(&view, thread);
                        if want.is_some_and(|want| want != info.state) {
                            continue;
                        }
                        threads.push(info.json());
                    }
                }
                Ok(json!({ "threads": threads }))
            }
            Command::ThreadGet { thread } => {
                let want = thread.unwrap_or(id);
                let views = self.views();
                let (view, thread) = find_thread(&views, want).ok_or("no such thread")?;
                Ok(self.thread_info(view, thread).json())
            }
            Command::ThreadOptions { thread } => {
                let want = thread.unwrap_or(id);
                let views = self.views();
                let (_, thread) = find_thread(&views, want).ok_or("no such thread")?;
                let options: Vec<Value> = thread
                    .options
                    .iter()
                    .map(|option| state::OptionInfo { id: option.id.clone(), name: option.name.clone(), category: option.category.clone(), current: option.current.clone(), choices: option.choices.clone() }.json())
                    .collect();
                Ok(json!({ "options": options }))
            }
            Command::ThreadMessages { thread, cursor, limit, full } => {
                let want = thread.unwrap_or(id);
                let views = self.views();
                let (_, thread) = find_thread(&views, want).ok_or("no such thread")?;
                let all = transcript(thread, full.is_some_and(|full| full));
                let (reset, start) = match cursor.as_deref().and_then(|cursor| cursor.split_once(':')) {
                    Some((generation, index)) if generation.parse::<u64>() == Ok(thread.generation) => (false, index.parse::<usize>().unwrap_or(0)),
                    Some(_) => (true, 0), // the transcript was cleared or replaced since
                    None => (false, 0),
                };
                let limit = limit.unwrap_or(state::DEFAULT_MESSAGES).max(1);
                let entries: Vec<Value> = all.iter().skip(start).take(limit).map(|entry| entry.json()).collect();
                let more = start + entries.len() < all.len();
                Ok(json!({
                    "entries": entries,
                    "next": more.then(|| format!("{}:{}", thread.generation, start + entries.len())),
                    "reset": reset,
                }))
            }
            Command::ThreadLastReply { thread } => {
                let want = thread.unwrap_or(id);
                let views = self.views();
                let (_, thread) = find_thread(&views, want).ok_or("no such thread")?;
                let text = thread
                    .messages
                    .iter()
                    .rev()
                    .find_map(|msg| match msg {
                        Msg::Agent(blocks) => {
                            let said: Vec<&str> = blocks.iter().filter_map(|block| match block {
                                Block::Text(text) => Some(text.as_str()),
                                _ => None,
                            }).collect();
                            (!said.is_empty()).then(|| said.join("\n\n"))
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let mut text = text;
                if text.len() > command::MAX_BODY {
                    text.truncate(command::MAX_BODY);
                    text.push_str("\n…");
                }
                Ok(json!({ "text": text, "state": state::state(&thread.facts()).name() }))
            }
            _ => Err("not a read".into()),
        }
    }

    fn grant_of(&self, caller: command::Caller) -> command::Grant {
        match caller {
            command::Caller::User => command::Grant::user(),
            command::Caller::Thread(id) => self.grants.get(&id).copied().unwrap_or_default(),
        }
    }

    /// The project writes: opening, closing and offering projects. None of it switches the
    /// screen; a folder that is not a project yet waits for the user's click.
    fn run_project(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        use command::Command;
        use serde_json::json;
        match command {
            Command::Issues { op: issues::api::Op::IssueWork, args } => self.work_issue(id, &args, window, cx),
            Command::Issues { op: issues::api::Op::IssueShow, args } => self.show_issues(id, &args, window, cx),
            Command::Issues { op, args } => self.run_issues(id, op, &args),
            Command::ProjectOpen { path: None, pr: Some(spec) } => {
                // Reviewed from the checkout the caller is in, as the PR strip does.
                let from = self.project_of(id).and_then(|project| self.project_by_id(project).map(|project| project.root.clone())).unwrap_or_else(|| self.home.clone());
                self.suggest(id, Proposal::Pr { spec, from }, cx)?;
                Ok(json!({ "confirm": "asked" }))
            }
            Command::WorktreeMerge { project } => {
                if self.project_by_id(project).and_then(|project| project.worktree.as_ref()).is_none() {
                    return Err("that project is not a worktree".into());
                }
                self.suggest(id, Proposal::Merge(project), cx)?;
                Ok(json!({ "confirm": "asked" }))
            }
            Command::ProjectOpen { path: Some(path), .. } => {
                let root = std::path::Path::new(&path).canonicalize().map_err(|_| format!("`{path}` is not a folder"))?;
                if !root.is_dir() {
                    return Err(format!("`{path}` is not a folder"));
                }
                if root.parent().is_none() || config::home().is_some_and(|home| home.canonicalize().is_ok_and(|home| home == root)) {
                    return Err("not the root or the home directory".into());
                }
                if let Some(view) = self.views().iter().find(|view| view.project.pr.is_none() && view.project.root == root) {
                    return Ok(json!({ "project": view.id, "opened": false }));
                }
                self.suggest(id, Proposal::Open(root), cx)?;
                Ok(json!({ "confirm": "asked" }))
            }
            Command::ProjectOffer { project } => {
                if !self.views().iter().any(|view| view.id == project) {
                    return Err("no such project".into());
                }
                self.suggest(id, Proposal::Go(project), cx)?;
                Ok(json!({}))
            }
            Command::ProjectClose { project } => {
                if !self.api_projects.contains(&project) {
                    return Err("only a project the API opened can be closed by it".into());
                }
                // The user's own threads (any that nobody started and that were used) keep it.
                let views = self.views();
                let view = views.iter().find(|view| view.id == project).ok_or("no such project")?;
                if view.threads.iter().any(|thread| thread.parent.is_none() && !thread.blank) {
                    return Err("the user has threads in it".into());
                }
                self.api_projects.remove(&project);
                self.suggestions.retain(|suggestion| !matches!(suggestion.what, Proposal::Go(p) if p == project));
                self.close_project(project, window, cx);
                Ok(json!({}))
            }
            _ => Err("not a project command".into()),
        }
    }

    /// The issue and epic commands, on the issues of the repo the caller works in (the project on
    /// screen for the user's own `po ctl`).
    fn run_issues(&self, id: u64, op: issues::api::Op, args: &serde_json::Value) -> command::Reply {
        let root = self.issues_root_of(id);
        let today = date::today();
        issues::shared::change_repo(&root, |tracker, now| issues::api::run(op, args, tracker, now, today))
    }

    /// The project the caller works in, whose issues its commands act on; the one on screen for
    /// the user's own `po ctl`.
    fn issues_root_of(&self, id: u64) -> PathBuf {
        self.own_project(id).and_then(|project| self.views().iter().find(|view| view.id == project).map(|view| view.project.root.clone())).unwrap_or_else(|| self.project.root.clone())
    }

    /// `thread.create_isolated`: the worktree is made off the UI thread, then the thread starts
    /// in a project of its own, and the caller is told how it went.
    fn create_isolated(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let command::Command::ThreadCreateIsolated { prompt, agent, options, notify } = command else {
            return Err("not a thread.create_isolated".into());
        };
        self.create_isolated_for(id, prompt, agent, options, notify, None, window, cx)
    }

    /// `issue.work`: a thread in a worktree of its own on an issue, whose branch is linked to the
    /// issue and which is moved to In Progress once the worktree is up.
    fn work_issue(&mut self, id: u64, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let root = self.issues_root_of(id);
        let (number, prompt, key) = issues::shared::change_repo(&root, |tracker, _| {
            let number = issues::api::work_issue(tracker, args)?;
            Ok::<_, String>((number, issues::api::work_prompt(tracker, number).ok_or("that issue is gone")?, tracker.key(number)))
        })?;
        let options = args["options"].as_object().map(|options| options.iter().filter_map(|(id, value)| value.as_str().map(|value| (id.clone(), value.to_string()))).collect()).unwrap_or_default();
        let agent = args["agent"].as_str().map(String::from);
        let notify = args["notify"].as_str().map(String::from);
        let mut reply = self.create_isolated_for(id, prompt, agent, options, notify, Some((root, number)), window, cx)?;
        reply["issue"] = serde_json::json!(key);
        Ok(reply)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_isolated_for(&mut self, id: u64, prompt: String, agent: Option<String>, options: Vec<(String, String)>, notify: Option<String>, issue: Option<(PathBuf, u64)>, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let key = self.child_agent(id, agent)?;
        let project = self.project_of(id).ok_or("that thread is gone")?;
        if self.finishing || self.isolating {
            return Err("a worktree is being made or merged; try again in a moment".into());
        }
        // A worktree is made from the repository, not from another worktree.
        let from = {
            let project = self.project_by_id(project).ok_or("that thread is gone")?;
            match &project.worktree {
                Some(wt) => wt.spec.repo.clone(),
                None => project.root.clone(),
            }
        };
        self.isolating = true;
        let request = self.next_request;
        self.next_request += 1;
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let made = cx.background_executor().spawn(async move { worktree::Worktree::create(&from) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.isolating = false;
                        let told = match made {
                            Ok(wt) => {
                                let branch = wt.spec.branch.clone();
                                let started = reader.start_isolated(id, wt, key, prompt, options, notify, window, cx);
                                if let (Ok(_), Some((root, number))) = (&started, &issue) {
                                    Self::issue_started(root, *number, Some(branch));
                                }
                                started
                            }
                            Err(error) => Err(error),
                        };
                        let text = match told {
                            Ok((thread, project)) => format!("Isolated thread #{thread} is up in project {project} (request {request})."),
                            Err(error) => format!("Isolated thread (request {request}) could not start: {error}"),
                        };
                        reader.tell(id, text, window, cx);
                    })
                })
                .ok();
        })
        .detach();
        Ok(serde_json::json!({ "creating": true, "request": request }))
    }

    /// Puts a made worktree up as a project and starts the child in it; returns the thread and
    /// project. If the parent went away meanwhile the project goes too, keeping its branch.
    #[allow(clippy::too_many_arguments)]
    fn start_isolated(&mut self, parent: u64, wt: worktree::Worktree, key: String, prompt: String, options: Vec<(String, String)>, notify: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Result<(u64, u64), String> {
        let mut project = Self::build_project(wt.spec.path.clone(), None, vec![]);
        project.base = wt.spec.base.clone();
        project.worktree = Some(wt);
        let pid = self.add_slot(project, Mode::Diff, cx);
        match self.start_child(parent, pid, key, prompt, options, notify, window, cx) {
            Ok(reply) => Ok((reply["thread"].as_u64().unwrap_or_default(), pid)),
            Err(error) => {
                self.api_projects.remove(&pid);
                self.close_project(pid, window, cx);
                Err(error)
            }
        }
    }

    /// Says `text` to thread `id` as Po, wherever that thread is.
    fn tell(&mut self, id: u64, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let out = Outgoing { text, origin: Origin::Po, images: vec![] };
        self.with_thread(id, |reader, _| reader.submit(out, false, window, cx));
    }

    /// Puts `what` to the user from thread `from`, once: asking again for the same thing only
    /// makes it the newest.
    fn suggest(&mut self, from: u64, what: Proposal, cx: &mut Context<Self>) -> Result<(), String> {
        let same = |a: &Proposal, b: &Proposal| match (a, b) {
            (Proposal::Go(a), Proposal::Go(b)) => a == b,
            (Proposal::Open(a), Proposal::Open(b)) => a == b,
            (Proposal::Pr { spec: a, .. }, Proposal::Pr { spec: b, .. }) => a == b,
            (Proposal::Merge(a), Proposal::Merge(b)) => a == b,
            _ => false,
        };
        self.suggestions.retain(|suggestion| !(suggestion.from == from && same(&suggestion.what, &what)));
        if self.suggestions.len() >= MAX_SUGGESTIONS {
            return Err("the user has enough waiting; try again after they answer".into());
        }
        let by = self.title_of(from).unwrap_or_else(|| "A thread".into());
        let id = self.next_suggestion;
        self.next_suggestion += 1;
        self.suggestions.push(Suggestion { id, from, by, what });
        cx.notify();
        Ok(())
    }

    /// The user's answer to a suggestion. Accepting a folder opens it in the background and the
    /// thread that asked is told either way; accepting a project goes to it.
    fn answer_suggestion(&mut self, id: u64, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.suggestions.iter().position(|suggestion| suggestion.id == id) else { return };
        let Suggestion { from, what, .. } = self.suggestions.remove(at);
        let from_thread = from;
        let told = match (what, yes) {
            (Proposal::Go(project), true) => {
                self.show_project(project, window, cx);
                None
            }
            (Proposal::Go(_), false) => None,
            (Proposal::Open(root), true) => {
                let project = self.open_in_background(root.clone(), cx);
                Some(format!("The user opened {}: project {project}.", root.display()))
            }
            (Proposal::Open(root), false) => Some(format!("The user declined to open {}.", root.display())),
            (Proposal::Pr { spec, from }, true) => {
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    let result = cx.background_executor().spawn({
                        let spec = spec.clone();
                        async move { pr::Pr::open(&from, &spec) }
                    }).await;
                    handle
                        .update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                let text = match result {
                                    Ok(pr) => {
                                        let root = pr.worktree.clone();
                                        let project = Self::build_project(root, Some(pr), vec![]);
                                        let id = reader.add_slot(project, Mode::Diff, cx);
                                        format!("The user opened PR {spec}: project {id}.")
                                    }
                                    Err(error) => format!("PR {spec} could not be checked out: {error}"),
                                };
                                reader.tell(from_thread, text, window, cx);
                            })
                        })
                        .ok();
                })
                .detach();
                None
            }
            (Proposal::Pr { spec, .. }, false) => Some(format!("The user declined to open PR {spec}.")),
            (Proposal::Merge(project), true) => {
                self.finish_worktree_of(project, true, Some(from), window, cx);
                None
            }
            (Proposal::Merge(project), false) => Some(format!("The user declined to merge project {project}.")),
        };
        if let Some(text) = told {
            self.tell(from, text, window, cx);
        }
        cx.notify();
    }

    /// Adds a project over `root` as a slot, with a blank thread, leaving the screen where it is.
    /// If it is already open, that one is returned.
    fn open_in_background(&mut self, root: PathBuf, cx: &mut Context<Self>) -> u64 {
        if let Some(view) = self.views().iter().find(|view| view.project.pr.is_none() && view.project.root == root) {
            return view.id;
        }
        let project = Self::build_project(root, None, vec![]);
        self.add_slot(project, Mode::Source, cx)
    }

    /// Adds `project` as a slot with a blank thread, leaving the screen where it is, and marks
    /// it as the API's.
    fn add_slot(&mut self, project: Project, mode: Mode, cx: &mut Context<Self>) -> u64 {
        let thread = self.blank_thread();
        let id = self.next_project;
        self.next_project += 1;
        self.slots.push(Slot { id, mode, project, thread, parked: vec![] });
        self.api_projects.insert(id);
        self.request_save(cx);
        cx.notify();
        id
    }

    /// The thread family: what an orchestrator does with the threads it started. Every write
    /// runs on the UI thread through the swap, and nothing here switches the screen.
    fn run_thread(&mut self, caller: command::Caller, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Answer {
        use command::Command;
        // A wait's deadline is capped with the command's own timeout, judged before the
        // command is taken apart.
        let wait_deadline = std::time::Instant::now() + command.timeout();
        match command {
            Command::ThreadCreate { .. } => command::Answer::Now(self.create_thread(id, command, window, cx)),
            Command::ThreadCreateIsolated { .. } => command::Answer::Now(self.create_isolated(id, command, window, cx)),
            Command::ThreadSend { thread: target, text, later } => {
                if target == id {
                    return command::Answer::Now(Err("send to a thread you started, not to yourself".into()));
                }
                let Some(title) = self.title_of(id) else { return command::Answer::Now(Err("that thread is gone".into())) };
                let out = Outgoing { text, origin: Origin::Thread { id, title }, images: vec![] };
                let sent = self.with_thread(target, |reader, _| {
                    // The waiters and the parent hear when this starts or ends the child's turn.
                    Some(reader.observed(window, cx, |reader, window, cx| reader.submit(out, later.unwrap_or(false), window, cx)))
                });
                match sent.flatten() {
                    Some(Where::Dispatch) => command::Answer::Now(Ok(serde_json::json!({ "state": "sent" }))),
                    Some(Where::Wait | Where::Start) => command::Answer::Now(Ok(serde_json::json!({ "state": "pending" }))),
                    Some(_) => command::Answer::Now(Ok(serde_json::json!({ "state": "queued" }))),
                    None => command::Answer::Now(Err("no such thread".into())),
                }
            }
            Command::ThreadAnswer { thread: target, option } => {
                if matches!(caller, command::Caller::Thread(_)) && target == id {
                    return command::Answer::Now(Err("answer a thread you started, not yourself".into()));
                }
                let answered = self.with_thread(target, |reader, _| {
                    reader.observed(window, cx, |reader, _, cx| {
                        // Only a permission ask: the user's approval of a command or an MCP
                        // server is a different block, and not reachable from here.
                        let found = reader.thread.messages.iter().enumerate().rev().find_map(|(msg, entry)| {
                            let Msg::Agent(blocks) = entry else { return None };
                            blocks.iter().enumerate().find_map(|(block, b)| match b {
                                Block::Permission(ask) if ask.answered.is_none() => Some((msg, block, ask.options.iter().position(|o| o.id == option))),
                                _ => None,
                            })
                        });
                        match found {
                            None => Err("that thread is not asking".to_string()),
                            Some((_, _, None)) => Err(format!("`{option}` is not one of its options; thread.get lists them")),
                            Some((msg, block, Some(at))) => {
                                reader.answer(msg, block, at, cx);
                                Ok(serde_json::json!({}))
                            }
                        }
                    })
                });
                command::Answer::Now(answered.unwrap_or_else(|| Err("no such thread".into())))
            }
            Command::ThreadCancel { thread: target } => {
                if matches!(caller, command::Caller::Thread(_)) && target == id {
                    return command::Answer::Now(Err("cancel a thread you started, not yourself".into()));
                }
                let stopped = self.with_thread(target, |reader, _| {
                    reader.stop(cx);
                    let dropped: Vec<Value> = reader.thread.dropped.drain(..).map(serde_json::Value::String).collect();
                    Ok(serde_json::json!({ "dropped": dropped }))
                });
                command::Answer::Now(stopped.unwrap_or_else(|| Err("no such thread".into())))
            }
            Command::ThreadSetOption { thread, id: setting, value } => {
                let target = thread.unwrap_or(id);
                let stored = self.with_thread(target, |reader, _| {
                    let thread = &mut reader.thread;
                    let Some(option) = thread.options.iter().find(|option| option.id == setting) else {
                        return Err(format!("no option `{setting}` here; thread.options lists them"));
                    };
                    if !option.choices.iter().any(|(choice, _)| *choice == value) {
                        return Err(format!("`{value}` is not one of `{setting}`'s choices"));
                    }
                    // A mode may be the agent's permissions (one that skips asking, say): what
                    // an agent may do without the user's click is the user's to change.
                    if option.category == "mode" {
                        return Err("a thread's mode is the user's to change".into());
                    }
                    thread.overrides.insert(setting.clone(), value.clone());
                    thread.api_options.insert(setting.clone());
                    Ok(())
                });
                let stored = match stored {
                    Some(Ok(())) => {
                        // A thread that is ready and idle takes it now; one that is busy
                        // applies what it owes between turns.
                        self.with_thread(target, |reader, _| {
                            if reader.thread.ready && !reader.thread.busy && reader.thread.client.is_some() {
                                reader.apply_overrides(window, cx);
                            }
                            Ok(serde_json::json!({ "pending": true }))
                        })
                        .unwrap_or_else(|| Err("no such thread".into()))
                    }
                    Some(Err(why)) => Err(why),
                    None => Err("no such thread".into()),
                };
                command::Answer::Now(stored)
            }
            Command::ThreadClose { thread: target } => {
                if matches!(caller, command::Caller::Thread(_)) && target == id {
                    return command::Answer::Now(Err("close a thread you started, not yourself".into()));
                }
                match self.project_of(target) {
                    Some(project) => {
                        self.close_any_thread(project, target, window, cx);
                        command::Answer::Now(Ok(serde_json::json!({})))
                    }
                    None => command::Answer::Now(Err("no such thread".into())),
                }
            }
            Command::ThreadWait { threads, until, timeout: _ } => {
                let waiters_of_caller = self.waiters.iter().filter(|waiter| waiter.caller == caller).count();
                if waiters_of_caller >= 2 {
                    return command::Answer::Now(Err("at most 2 waits at a time".into()));
                }
                if self.waiters.len() >= MAX_WAITS {
                    return command::Answer::Now(Err("Po is already holding its share of waits".into()));
                }
                // Waits target children only; a wait on the caller itself never ends well.
                for target in threads.iter().flatten() {
                    if *target == id {
                        return command::Answer::Now(Err("waiting on yourself would never end".into()));
                    }
                    if !self.threads_all().any(|thread| thread.id == *target && thread.parent == Some(id)) {
                        return command::Answer::Now(Err(format!("thread {target} is not one you started")));
                    }
                }
                // Until a state, or `any` change; nothing said means "left running or starting".
                let until = match until.as_deref() {
                    Some("any") => vec![],
                    Some("done") | None => vec!["ready", "asking", "approval", "failed", "asleep", "closed"],
                    Some(state) => vec![crate::state::ThreadState::from_name(state).expect("checked").name()],
                };
                command::Answer::Later(command::Wait { threads: threads.unwrap_or_default(), until, deadline: wait_deadline, form: None })
            }
            Command::FormWait { id: form, timeout: _ } => {
                let waiters_of_caller = self.waiters.iter().filter(|waiter| waiter.caller == caller).count();
                if waiters_of_caller >= 2 {
                    return command::Answer::Now(Err("at most 2 waits at a time".into()));
                }
                if self.waiters.len() >= MAX_WAITS {
                    return command::Answer::Now(Err("Po is already holding its share of waits".into()));
                }
                // A form the user has answered already answers now; one still open is waited on.
                let state = self.with_thread(id, |reader, _| reader.thread.forms.get(&form).and_then(|form| form.answered.clone()));
                match state {
                    Some(Some(values)) => command::Answer::Now(Ok(values)),
                    Some(None) => command::Answer::Later(command::Wait { threads: vec![], until: vec![], deadline: wait_deadline, form: Some(form) }),
                    None => command::Answer::Now(Err(format!("no form `{form}`; show_form returns its id"))),
                }
            }
            // The reads of threads (list, get, messages…) are answered from the state as it is.
            read => command::Answer::Now(self.run_read(caller, id, read)),
        }
    }

    /// Holds a wait the window answered later, and starts its timer.
    fn add_waiter(&mut self, caller: command::Caller, wait: command::Wait, reply: std::sync::mpsc::Sender<command::Reply>, _window: &mut Window, cx: &mut Context<Self>) {
        let (token, handle) = (self.next_waiter, self.window);
        self.next_waiter += 1;
        let left = wait.deadline.saturating_duration_since(std::time::Instant::now());
        self.waiters.push(Waiter { token, caller, threads: wait.threads, until: wait.until, form: wait.form, reply });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(left).await;
            let _ = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.time_out(token, window, cx)));
        })
        .detach();
    }

    /// Keeps a control command in the audit log: what changes something, and anything refused;
    /// plain reads that were answered are not what the log is for, and would let a caller
    /// push the rest out. The same thing twice in a row is counted, not repeated.
    fn record(&mut self, caller: command::Caller, command: &'static str, outcome: String, writes: bool) {
        if !writes && outcome == "ok" {
            return;
        }
        if let Some(last) = self.audit.back_mut()
            && last.caller == caller
            && last.command == command
            && last.outcome == outcome
        {
            last.count += 1;
            last.at = now_ms();
            return;
        }
        if self.audit.len() >= MAX_AUDIT {
            self.audit.pop_front();
        }
        self.audit.push_back(Audit { at: now_ms(), caller, command, outcome, count: 1 });
    }

    /// Replies to a wait that ran out, unless it was already answered.
    fn time_out(&mut self, token: u64, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(at) = self.waiters.iter().position(|waiter| waiter.token == token) {
            let waiter = self.waiters.remove(at);
            waiter.reply.send(Ok(serde_json::json!({ "timed_out": true }))).ok();
        }
    }

    /// Ends the waits of a caller whose turn stopped, failed or exited, or whose grant was
    /// lowered: they would never be answered in time, and the caller is told why.
    fn drop_waiters(&mut self, caller: command::Caller, why: &str) {
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            if waiter.caller == caller {
                waiter.reply.send(Err(format!("cancelled: {why}"))).ok();
            } else {
                kept.push(waiter);
            }
        }
        self.waiters = kept;
    }

    /// Answers the waits `id`'s change satisfies. A waiter whose receiving side is gone is
    /// dropped here, rather than held for a sweep.
    fn wake_waiters(&mut self, id: u64, after: crate::state::ThreadState) {
        // The parent of the thread that changed, computed before the waits are taken out, so
        // the two looks at the threads don't meet.
        let parent = self.threads_all().find(|thread| thread.id == id).and_then(|thread| thread.parent);
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            // A form wait is answered by its user's submit, not by a thread's change of state.
            if waiter.form.is_some() {
                kept.push(waiter);
                continue;
            }
            let watched = matches!(waiter.caller, command::Caller::User)
                || matches!(waiter.caller, command::Caller::Thread(caller) if parent == Some(caller));
            let hit = (waiter.threads.contains(&id) || (waiter.threads.is_empty() && watched))
                && (waiter.until.is_empty() || waiter.until.contains(&after.name()));
            if hit && waiter.reply.send(Ok(serde_json::json!({ "thread": id, "state": after.name(), "timed_out": false }))).is_ok() {
                continue;
            }
            kept.push(waiter);
        }
        self.waiters = kept;
    }

    /// Answers the waits on form `id` of thread `owner`, which the user just submitted.
    fn wake_form(&mut self, owner: u64, id: &str, reply: Value) {
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            let hit = waiter.form.as_deref() == Some(id) && waiter.caller == command::Caller::Thread(owner);
            if hit && waiter.reply.send(Ok(reply.clone())).is_ok() {
                continue;
            }
            kept.push(waiter);
        }
        self.waiters = kept;
    }

    /// Ends the waits on form `id` of thread `owner`, which was closed without an answer.
    fn drop_form(&mut self, owner: u64, id: &str) {
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            let hit = waiter.form.as_deref() == Some(id) && waiter.caller == command::Caller::Thread(owner);
            if hit {
                waiter.reply.send(Err("cancelled: the form was closed".into())).ok();
                continue;
            }
            kept.push(waiter);
        }
        self.waiters = kept;
    }

    /// Runs `work` on the thread swapped in, watching its state: if it changed, the waiters
    /// and the parent hear about it.
    fn observed<R>(&mut self, window: &mut Window, cx: &mut Context<Self>, work: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) -> R) -> R {
        let before = crate::state::state(&self.thread.facts());
        let out = work(self, window, cx);
        let after = crate::state::state(&self.thread.facts());
        if before != after {
            self.changed(self.thread.id, after, window, cx);
        }
        out
    }

    /// Tells whoever listens that `id` now stands in `after`: its waiters, then its parent.
    fn changed(&mut self, id: u64, after: crate::state::ThreadState, window: &mut Window, cx: &mut Context<Self>) {
        self.wake_waiters(id, after);
        self.publish(id, after);
        self.notify_parent(id, after, window, cx);
    }

    /// Tells every `po ctl watch` that thread `id` is now in `after`; one that has hung up is
    /// dropped.
    fn publish(&mut self, id: u64, after: crate::state::ThreadState) {
        if self.subscribers.is_empty() {
            return;
        }
        let event = serde_json::json!({
            "event": "thread.state",
            "thread": id,
            "state": after.name(),
            "title": self.title_of(id),
            "project": self.project_of(id),
            "at": now_ms(),
        });
        self.subscribers.retain(|subscriber| subscriber.send(Ok(event.clone())).is_ok());
    }

    /// Puts a line about `id`'s state on the notice of the thread that started it, unless it
    /// asked not to hear or the change isn't one it asked about.
    fn notify_parent(&mut self, id: u64, after: crate::state::ThreadState, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((Some(parent), level)) = self.with_thread(id, |reader, _| Some((reader.thread.parent, reader.thread.notify))).flatten() else { return };
        if level == Notify::Never {
            return;
        }
        // `done` hears when the turn is over, the child asks, or it is failed, asleep or
        // closed; `all` also hears that a turn started. A parent's own messages never quote
        // the child: what it said is read with last_reply, on purpose.
        if level == Notify::Done && matches!(after, crate::state::ThreadState::Running | crate::state::ThreadState::Starting) {
            return;
        }
        let word = match after {
            crate::state::ThreadState::Running => "started a turn",
            crate::state::ThreadState::Asking | crate::state::ThreadState::Approval => "asks for an answer",
            crate::state::ThreadState::Failed => "failed",
            crate::state::ThreadState::Asleep => "went to sleep",
            crate::state::ThreadState::Closed => "is closed",
            crate::state::ThreadState::Starting => "is starting",
            crate::state::ThreadState::Ready => "finished its turn",
        };
        let line = format!("Thread #{id} {word}. Use last_reply or get_thread to read what it said.");
        let notice = self.notices.entry(parent).or_insert_with(|| Notice { text: String::new() });
        if !notice.text.is_empty() {
            notice.text.push('\n');
        }
        notice.text.push_str(&line);
        // Changes that happen within the coalesce window go out together.
        if self.notice_scheduled.insert(parent) {
            let (handle, parent) = (self.window, parent);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTICE_COALESCE).await;
                let _ = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.flush_notice(parent, window, cx)));
            })
            .detach();
        }
    }

    /// Sends a parent the notice it has been waiting for, once the rate limit lets it speak.
    fn flush_notice(&mut self, parent: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.notices.contains_key(&parent) {
            self.notice_scheduled.remove(&parent);
            return;
        }
        // Every notice is a turn of the parent's: more frequent ones merge into the next.
        if let Some(left) = notice_wait(self.notice_sent_at.get(&parent).map(Instant::elapsed)) {
            let (handle, parent) = (self.window, parent);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(left).await;
                let _ = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.flush_notice(parent, window, cx)));
            })
            .detach();
            return;
        }
        let Some(notice) = self.notices.remove(&parent) else { return };
        self.notice_scheduled.remove(&parent);
        self.notice_sent_at.insert(parent, Instant::now());
        let out = Outgoing { text: notice.text, origin: Origin::Po, images: vec![] };
        self.with_thread(parent, |reader, _| {
            reader.submit(out, false, window, cx);
            Some(())
        });
        cx.notify();
    }

    /// What a thread is called, for labelling text it sends to a child.
    fn title_of(&self, id: u64) -> Option<String> {
        self.threads_all().find(|thread| thread.id == id).map(|thread| thread.title())
    }

    /// The project `id`'s thread belongs to, if it is still here.
    fn project_of(&self, id: u64) -> Option<u64> {
        self.views().into_iter().find(|view| view.threads.iter().any(|thread| thread.id == id)).map(|view| view.id)
    }

    /// The project the caller's thread belongs to, if it is still here.
    fn own_project(&self, id: u64) -> Option<u64> {
        self.views().into_iter().find(|view| view.threads.iter().any(|thread| thread.id == id)).map(|view| view.id)
    }

    /// One thread as the read model sees it: a snapshot of the fields an outsider gets.
    fn thread_info(&self, view: &View<'_>, thread: &Thread) -> state::ThreadInfo {
        let asking = thread.messages.iter().rev().find_map(|msg| {
            let Msg::Agent(blocks) = msg else { return None };
            blocks.iter().find_map(|block| match block {
                Block::Permission(Ask { request: _, title, options, answered: None }) => {
                    Some(state::AskInfo { title: title.clone(), options: options.iter().map(|o| state::AskOption { id: o.id.clone(), name: o.name.clone(), kind: o.kind.clone() }).collect() })
                }
                _ => None,
            })
        });
        let children = self.threads_all().filter(|child| child.parent == Some(thread.id)).map(|child| child.id).collect();
        state::ThreadInfo {
            id: thread.id,
            project: view.id,
            title: thread.title(),
            agent: thread.agent.clone().unwrap_or_default(),
            state: state::state(&thread.facts()),
            unread: thread.unread,
            busy: thread.busy,
            status: thread.status.clone(),
            last_error: thread.last_error.clone(),
            session: thread.session.clone(),
            preview: thread.preview(),
            queued: thread.queue.len(),
            dropped: thread.dropped.clone(),
            asking,
            parent: thread.parent,
            children,
            active_at: thread.active_at,
            on_screen: thread.id == view.shown,
            usage: thread.usage,
        }
    }

    /// One project as the read model sees it.
    fn project_info(&self, view: &View<'_>) -> state::ProjectInfo {
        state::ProjectInfo {
            id: view.id,
            root: view.project.root.to_string_lossy().into_owned(),
            name: view.project.name(),
            branch: match &view.project.worktree {
                Some(wt) => Some(wt.spec.branch.clone()),
                None => self.heads.get(&view.project.root).map(|head| head.name.clone()).or(view.project.pr.as_ref().map(|_| String::new())),
            },
            // The project on screen is watched, so its list is current; another has not had its
            // changes listed (an agent's worktree never has), so it is asked of the repository.
            dirty: if view.id == self.project_id { !view.project.changes.is_empty() } else { !diff::changes(&view.project.root, &view.project.base).is_empty() },
            pr: view.project.pr.as_ref().map(|pr| pr.number),
            worktree_of: view.project.worktree.as_ref().map(|wt| {
                self.views().iter().find(|other| other.project.root == wt.spec.repo).map(|other| other.id).unwrap_or(0)
            }),
            on_screen: view.id == self.project_id,
            threads: view.threads.len(),
        }
    }

    /// Tells what the viewer is showing about views and forms that are gone, since the pane the
    /// NSH screen draws picks its view by id.
    fn sync_view_docs(&mut self, cx: &mut Context<Self>) {
        let live: HashSet<String> = self
            .thread
            .views
            .list()
            .iter()
            .map(|view| view.id.clone())
            .chain(self.thread.forms.list().iter().map(|form| form.id.clone()))
            .collect();
        for slot in [&mut self.thread.shown, &mut self.thread.offered] {
            if slot.as_ref().is_some_and(|id| !live.contains(id)) {
                *slot = None;
                cx.notify();
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
                if let (Some(line), Some(_)) = (line, &self.project.code) {
                    self.reveal_serial += 1;
                    self.reveal = Some((line.saturating_sub(1) as u32, self.reveal_serial));
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
        self.split = split;
        cx.notify();
    }

    /// Puts the viewer on the other side of the conversation, and remembers it in `config.json`
    /// so the Settings page shows it. The conversation shows after a swap.
    fn swap_panes(&mut self, cx: &mut Context<Self>) {
        self.swapped = !self.swapped;
        self.agent_open = true;
        let position = if self.swapped { "left" } else { "right" };
        if let Err(error) = config::update(|config| {
            config.insert("viewer_position".into(), position.into());
        }) {
            log::warn!("viewer position: {error}");
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Shows the viewer, or closes it. It is always the viewer, whichever side it is on.
    fn toggle_right(&mut self, cx: &mut Context<Self>) {
        if self.side_open {
            self.show_chat(cx);
        } else {
            self.open_viewer_wide(cx);
        }
    }

    /// With the viewer open, hides the conversation so the viewer has the room, or brings it back.
    fn toggle_focus(&mut self, cx: &mut Context<Self>) {
        if self.side_open {
            self.agent_open = !self.agent_open;
            self.request_save(cx);
            cx.notify();
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
        self.thread.failed = false;
        self.thread.last_error = None;
        let generation = self.thread.generation;
        let id = self.thread.id;
        self.thread.client = None;
        self.thread.agent = Some(key.to_string());
        self.thread.messages.clear();
        self.thread.relist(0);
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
        // The token the agent's MCP server gets is made once the child exists, so that it is
        // bound to the child's process: from then on it works only from that process and its
        // descendants (see `control`).
        let control = self.control.clone();
        let registered = move |pid: Option<u32>| control.as_ref().map(|control| control.register(id, pid)).unwrap_or_default();
        match Client::spawn(&agent, &self.project.root, resume, registered) {
            Ok((client, mut events)) => {
                self.thread.client = Some(client);
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    // An agent that never answers the handshake would leave the thread on "starting…".
                    let watchdog = this.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(20)).await;
                        handle
                            .update(cx, |_, _, cx| {
                                watchdog.update(cx, |reader, cx| {
                                    reader.with_thread(id, |reader, _| {
                                        let thread = &reader.thread;
                                        if thread.generation == generation && !thread.ready && thread.client.is_some() {
                                            // What was typed goes back to the draft rather than wait for an agent that is gone.
                                            reader.return_pending();
                                            reader.restore_queue(cx);
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
                                            // The waiters and the parent hear of a change of state.
                                            reader.observed(window, cx, |reader, window, cx| reader.on_event(event, visible, window, cx));
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
        self.enforce_cap(window, cx);
        cx.notify();
    }

    /// Opens a blank thread, ready for a message, with its agent starting so that its settings
    /// can be chosen first. The thread on screen keeps running in the background; if it is
    /// itself still blank it is used instead.
    fn new_blank_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread.blank {
            self.add_thread();
        }
        self.start_blank(window, cx);
        self.focus_draft(window, cx);
        cx.notify();
    }

    /// Asks the conversation to put the keyboard in its message field.
    fn focus_draft(&self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.chat_focus.set(self.chat_focus.get() + 1);
    }

    /// The agent a new thread would start with.
    fn chosen(&self) -> Option<&config::Agent> {
        self.chosen_agent.as_ref().and_then(|key| self.agents.iter().find(|agent| agent.id == *key)).or(self.agents.first())
    }

    /// Sets a new session's settings to what was last chosen for its agent, in the order of
    /// [`Thread::ordered`]. A first message waits until they are taken.
    fn apply_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.applying = Some(Applying { carrying: true, ..Default::default() });
        self.apply_next(window, cx);
    }

    /// Applies what the API asked a thread's session to change, without carrying over the
    /// choices of earlier threads; when the session has none to take, the waiting message goes.
    fn apply_overrides(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.overrides.is_empty() {
            return self.send_pending(cx);
        }
        self.thread.applying = Some(Applying::default());
        self.apply_next(window, cx);
    }

    /// Asks the agent for the next setting to take: what the API asked for first, then (while
    /// `carrying`) what was last chosen for its agent — judged by what it offers now, so that
    /// one it only offers after a change is carried over too. Once none are left, the first
    /// message goes.
    fn apply_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = &mut self.thread;
        let (Some(applying), Some(key), Some(client)) = (thread.applying.as_mut(), &thread.agent, &thread.client) else {
            thread.applying = None;
            return self.send_pending(cx);
        };
        let chosen = self.choices.get(key);
        let options: Vec<&ConfigOption> = thread.options.iter().filter(|option| !applying.settled.contains(&option.id)).collect();
        for option in Thread::ordered(options) {
            applying.settled.insert(option.id.clone());
            // What the API asked for beats what was carried over; when this session was woken
            // only for the API's settings, carried choices don't come in at all.
            // Never a mode, though: see `thread.set_option`.
            let asked = thread.overrides.get(&option.id).filter(|_| option.category != "mode");
            let want = if applying.carrying { asked.or(chosen.and_then(|chosen| chosen.get(&option.id))) } else { asked };
            let Some(value) = want.cloned() else { continue };
            if value != option.current && option.choices.iter().any(|(choice, _)| *choice == value) && client.set_option(&option.id, &value) {
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
        self.send_pending(cx);
        // The queued messages waited for the settings; they go now.
        self.send_next(cx);
    }

    /// Sends the first message, once the agent is ready and has the settings it starts with.
    fn send_pending(&mut self, cx: &mut Context<Self>) {
        if !self.thread.ready || self.thread.applying.is_some() {
            return;
        }
        if let Some(out) = self.thread.pending.take()
            && !self.send_to_agent(&out)
        {
            self.thread.pending = Some(out);
            self.return_pending();
            self.thread.fail("The agent has no session yet.".into());
            self.restore_queue(cx);
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
    fn choose_option(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        let thread = &mut self.thread;
        if let Some(client) = &thread.client
            && client.set_option(id, value)
            && let Some(applying) = &mut thread.applying
        {
            // What is chosen here isn't then undone by carrying over an earlier choice.
            applying.settled.insert(id.to_string());
        }
        cx.notify();
    }

    /// Lists an agent's sessions in a new thread, to resume one: those of the agent the thread on
    /// screen is running, or of the chosen one if it has not started.
    fn resume_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else { return };
        log::info!("agent: resuming with {key}");
        self.add_thread();
        self.connect(&key, true, window, cx);
    }

    /// Starts the blank thread on screen with `out` as its first message: in the session its
    /// agent already has open, or in a worktree of its own if that was asked for. `later` is as
    /// for [`Self::send`].
    fn start_thread(&mut self, out: Outgoing, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else {
            self.thread.fail("No agents are configured.".into());
            cx.notify();
            return;
        };
        if self.finishing {
            return;
        }
        // The agent the user last chose is theirs to change; text sent through the API never
        // moves that default.
        if matches!(out.origin, Origin::User) {
            self.chosen_agent = Some(key.clone());
        }
        if self.thread.isolate && worktree::in_repo(&self.project.root) {
            // The draft stays until the worktree exists, so a failure loses nothing.
            let blank = (self.project_id, self.thread.id);
            self.thread.status = "creating worktree…".into();
            self.new_worktree_thread(&key, Some((out, blank)), window, cx);
            return;
        }
        log::info!("agent: new thread with {key}");
        // An agent that failed or was stopped starts again, without the error it left.
        if self.thread.client.is_none() {
            self.connect(&key, false, window, cx);
        }
        // One that couldn't be started has said why, and the next message tries again.
        if self.thread.client.is_none() {
            // Text the user typed stays in their draft; text sent through the API has nowhere
            // else to be, so it waits here for the agent to be approved and started.
            if !matches!(out.origin, Origin::User) {
                self.thread.pending = Some(out);
            }
            return;
        }
        self.thread.blank = false;
        self.submit(out, later, window, cx);
        self.request_save(cx);
    }

    /// Starts a child of the thread `id`: parked in its project, never shown, its grant
    /// Viewer, its first message waiting for the agent. The reply says where it got to: a
    /// thread with an agent that isn't approved says `approval` until the user has clicked.
    fn create_thread(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let command::Command::ThreadCreate { prompt, agent, options, notify } = command else {
            return Err("not a thread.create".into());
        };
        let key = self.child_agent(id, agent)?;
        let Some(project) = self.project_of(id) else { return Err("that thread is gone".into()) };
        self.start_child(id, project, key, prompt, options, notify, window, cx)
    }

    /// The agent a thread of `id`'s may start, if it may start another: one `agent.list` knows,
    /// the caller's own by default.
    fn child_agent(&self, id: u64, agent: Option<String>) -> Result<String, String> {
        let key = match agent {
            Some(key) => key,
            None => self.threads_all().find(|thread| thread.id == id).and_then(|thread| thread.agent.clone()).unwrap_or_default(),
        };
        if self.agents.iter().all(|known| known.id != key) {
            return Err(format!("no agent `{key}`; agent.list has the ids"));
        }
        if self.threads_all().filter(|thread| thread.parent == Some(id)).count() >= MAX_CHILDREN {
            return Err(format!("a thread starts at most {MAX_CHILDREN} others; close one first"));
        }
        Ok(key)
    }

    /// Starts a thread of parent `id`'s in project `project`, parked, with `prompt` as its
    /// first message.
    #[allow(clippy::too_many_arguments)]
    fn start_child(&mut self, id: u64, project: u64, key: String, prompt: String, options: Vec<(String, String)>, notify: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        // The parent may have gone, or started others, since the request was made: a worktree
        // takes a while, and this is where it comes to a thread.
        let Some(title) = self.title_of(id) else { return Err("that thread is gone".into()) };
        if self.threads_all().filter(|thread| thread.parent == Some(id)).count() >= MAX_CHILDREN {
            return Err(format!("a thread starts at most {MAX_CHILDREN} others; close one first"));
        }
        let level = notify.as_deref().and_then(Notify::from_name).unwrap_or_default();
        let out = Outgoing { text: prompt, origin: Origin::Thread { id, title }, images: vec![] };
        let mut fresh = self.blank_thread();
        fresh.blank = false;
        fresh.parent = Some(id);
        fresh.notify = level;
        for (setting, value) in options {
            fresh.overrides.insert(setting.clone(), value);
            fresh.api_options.insert(setting);
        }
        let fresh_id = fresh.id;
        if project == self.project_id {
            self.parked.push(fresh);
        } else if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == project) {
            slot.parked.push(fresh);
        } else {
            return Err("that thread is gone".into());
        }
        let state = self.with_thread(fresh_id, |reader, _| {
            reader.observed(window, cx, |reader, window, cx| {
                reader.connect(&key, false, window, cx);
                if reader.thread.client.is_some() {
                    // Shown as sent, as a message typed while the agent starts is.
                    reader.begin_turn(&out);
                    reader.thread.pending = Some(out);
                } else {
                    // The agent isn't approved or won't run yet; the message waits for it, the
                    // way one typed while the agent starts does.
                    reader.thread.pending = Some(out);
                }
            });
            crate::state::state(&reader.thread.facts()).name().to_string()
        });
        self.request_save(cx);
        cx.notify();
        Ok(serde_json::json!({ "thread": fresh_id, "state": state.unwrap_or_else(|| "starting".into()) }))
    }

    /// Puts a first message back in the queue after the agent failed before it could be sent,
    /// still labelled by where it came from.
    fn return_pending(&mut self) {
        let Some(out) = self.thread.pending.take() else { return };
        let prompt = out.shown();
        let shown = matches!(self.thread.messages.as_slice(), [.., Msg::User(text, _), Msg::Agent(blocks)] if *text == prompt && blocks.is_empty());
        if shown {
            let keep = self.thread.messages.len() - 2;
            self.thread.messages.truncate(keep);
            self.thread.relist(keep);
        }
        self.thread.queue.insert(0, Queued { out, kind: None });
    }

    /// Makes a thread with no agent yet.
    fn blank_thread(&mut self) -> Thread {
        let id = self.next_thread;
        self.next_thread += 1;
        Thread {
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
            draft: String::new(),
            attachments: vec![],
            attach_note: None,
            views: Default::default(),
            forms: Default::default(),
            shown: None,
            offered: None,
        }
    }

    /// Puts a blank thread on screen, parking the one that was, unless that is still blank.
    fn add_thread(&mut self) {
        let fresh = self.blank_thread();
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
    fn sleeping_thread(&mut self, saved: workspace::ThreadState) -> Thread {
        let mut thread = self.blank_thread();
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
    fn restore(&mut self, saved: workspace::Workspace, cx: &mut Context<Self>) -> bool {
        let mut resumed = false;
        let mut seen = HashSet::new();
        // Children of threads that came back with them, by the parent's session.
        let mut linked: Vec<(u64, String)> = vec![];
        self.side_open = saved.viewer_open;
        // The Settings page's choice wins over the layout the last window saved.
        self.swapped = config::viewer_left(&config::load()).unwrap_or(saved.swapped);
        self.agent_open = saved.agent_open;
        self.sidebars.update(cx, |sidebars, _| sidebars.set_widths(saved.left_width.map(px), saved.right_width.map(px)));
        self.tree_cols.update(cx, |cols, _| cols.set_widths(saved.explorer_width.map(px), None));
        self.chosen_agent = saved.agent.clone().or(self.chosen_agent.take());
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
                    let parent = t.parent_session.clone();
                    let thread = self.sleeping_thread(t);
                    linked.extend(parent.map(|parent| (thread.id, parent)));
                    thread
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
            let mut project = Self::build_project(state.root, None, vec![]);
            if let Some(wt) = worktree {
                project.base = wt.spec.base.clone();
                project.worktree = Some(wt);
            }
            let thread = match shown {
                Some(thread) => thread,
                None => self.blank_thread(),
            };
            let id = self.next_project;
            self.next_project += 1;
            let mode = if state.review { Mode::Diff } else { Mode::Source };
            self.slots.push(Slot { id, mode, project, thread, parked: threads });
        }
        self.relink_children(linked);
        resumed
    }

    /// Links each `(child, parent session)` to the thread of this window that has that session.
    /// A parent that is not here leaves its child an ordinary thread.
    fn relink_children(&mut self, linked: Vec<(u64, String)>) {
        let parents: HashMap<String, u64> = self.threads_all().filter_map(|thread| Some((thread.session.clone()?, thread.id))).collect();
        let mut wanted: HashMap<u64, u64> = linked.into_iter().filter_map(|(child, session)| Some((child, *parents.get(&session)?))).filter(|(child, parent)| child != parent).collect();
        // The file can be written by an agent, so it is not trusted to keep the plan's shape:
        // depth stays at 1 (a child of a child is left ordinary, which also rules out cycles),
        // and a parent keeps at most MAX_CHILDREN.
        let children: HashSet<u64> = wanted.keys().copied().collect();
        wanted.retain(|_, parent| !children.contains(parent));
        let mut count: HashMap<u64, usize> = HashMap::new();
        let mut order: Vec<u64> = wanted.keys().copied().collect();
        order.sort_unstable();
        for child in order {
            let n = count.entry(wanted[&child]).or_default();
            *n += 1;
            if *n > MAX_CHILDREN {
                wanted.remove(&child);
            }
        }
        if wanted.is_empty() {
            return;
        }
        let link = |thread: &mut Thread| thread.parent = wanted.get(&thread.id).copied().or(thread.parent);
        link(&mut self.thread);
        self.background_threads().for_each(link);
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
        let all: Vec<&Thread> = self.threads_all().collect();
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
                            let parent_session = t.parent.and_then(|parent| all.iter().find(|p| p.id == parent)).and_then(|p| p.session.clone());
                            Some(workspace::ThreadState { agent: t.agent.clone()?, session: t.session.clone()?, title: t.title(), active_at: t.active_at, parent_session })
                        })
                        .collect(),
                }
            })
            .collect();
        let panes = AgentChange::saved(self.agent_panes.as_ref(), self.panes());
        workspace::Workspace { projects, viewer_open: panes.viewer, agent_open: panes.agent, swapped: self.swapped, agent: self.chosen_agent.clone(), explorer_width: Some(self.tree_cols.read(cx).widths().0.into()), left_width: Some(self.sidebars.read(cx).widths().0.into()), right_width: self.sidebars.read(cx).widths().1.map(f32::from) }
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
                reader.workspace(cx).save(&mut reader.known_projects);
            })
        });
    }

    /// Stops the agents of threads that have been idle in the background for too long.
    fn sleep_idle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(after) = self.idle_after else { return };
        let mut slept: Vec<u64> = vec![];
        for thread in self.background_threads().filter(|t| t.restful() && t.touched.elapsed() > after) {
            log::info!("agent: stopping idle thread {}", thread.id);
            thread.sleep();
            slept.push(thread.id);
        }
        for id in slept {
            // The parent of a sleeping child hears, so it can wait or wake it.
            self.changed(id, crate::state::ThreadState::Asleep, window, cx);
            cx.notify();
        }
    }

    /// Stops the longest idle agents in the background while more than `max_live` run.
    fn enforce_cap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut live = usize::from(self.thread.client.is_some());
        live += self.background_threads().filter(|t| t.client.is_some()).count();
        let Some(excess) = live.checked_sub(self.max_live).filter(|n| *n > 0) else { return };
        let mut idle: Vec<&mut Thread> = self.background_threads().filter(|t| t.restful()).collect();
        // Blank threads lose nothing by stopping, so they go first.
        idle.sort_by_key(|t| (!t.blank, t.touched));
        let mut slept = vec![];
        for thread in idle.into_iter().take(excess) {
            log::info!("agent: stopping thread {} to stay under the limit", thread.id);
            thread.sleep();
            slept.push(thread.id);
        }
        for id in slept {
            self.changed(id, crate::state::ThreadState::Asleep, window, cx);
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
        let before = self.threads_all().find(|thread| thread.id == tid).map(|thread| crate::state::state(&thread.facts()));
        if before.is_some() && before != Some(crate::state::ThreadState::Closed) {
            self.changed(tid, crate::state::ThreadState::Closed, window, cx);
        }
        let last = self.slots.iter().find(|slot| slot.id == pid).is_some_and(|slot| slot.thread.id == tid && slot.parked.is_empty());
        let blank = last.then(|| self.blank_thread());
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
        // Whoever waited on it hears, as does its parent: while it is still here to be looked
        // up, since they find its parent by it.
        let before = self.threads_all().find(|thread| thread.id == id).map(|thread| crate::state::state(&thread.facts()));
        if before.is_some() && before != Some(crate::state::ThreadState::Closed) {
            self.changed(id, crate::state::ThreadState::Closed, window, cx);
        }
        if let Some(at) = self.parked.iter().position(|thread| thread.id == id) {
            self.parked.remove(at);
        } else if self.thread.id == id {
            let next = match self.parked.pop() {
                Some(next) => next,
                None => self.blank_thread(),
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
                    && !self.thread.api_options.contains(&id)
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
                    // A resumed session applies what the API asked for; with none, the waiting
                    // message goes.
                    self.apply_overrides(window, cx);
                }
            }
            Event::Update(update) => {
                if let Some(usage) = state::Usage::from_update(&update) {
                    self.thread.usage = Some(usage);
                }
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
                // A child's parent is told instead; the desktop hears of it only when nobody
                // is waiting on the child.
                let parent_hears = self.thread.parent.is_some() && self.thread.notify != Notify::Never;
                if !(parent_hears || visible) {
                    self.alert(window, visible, "Finished");
                }
                log::info!("agent: stopped ({reason})");
                // What the daemon didn't take into the turn is no longer in its queue.
                for item in &mut self.thread.queue {
                    item.kind = None;
                }
                // What the API asked the session to change applies between turns.
                if self.thread.client.is_some() && !self.thread.overrides.is_empty() {
                    self.apply_overrides(window, cx);
                } else {
                    self.send_next(cx);
                }
                // The order of the thread list is saved with the time the turn ended.
                self.request_save(cx);
                self.drop_waiters(command::Caller::Thread(self.thread.id), "the turn ended");
            }
            Event::Accepted { content, queue } => {
                if let Some(at) = self.thread.queue.iter().position(|q| q.kind == Some(queue) && q.out.queued_text() == content) {
                    self.thread.queue.remove(at);
                }
                self.thread.messages.push(Msg::User(content, vec![]));
            }
            Event::Rejected { content, queue } => {
                // No turn to join: it waits for the turn to end like any other queued message.
                if let Some(item) = self.thread.queue.iter_mut().find(|q| q.kind == Some(queue) && q.out.queued_text() == content) {
                    item.kind = None;
                }
                self.send_next(cx);
            }
            Event::Failed(error) => {
                if !self.thread.ready {
                    self.thread.status = "failed to start".into();
                }
                self.return_pending();
                self.thread.fail(error);
                self.restore_queue(cx);
                self.thread.applying = None;
                self.unstart();
                self.drop_waiters(command::Caller::Thread(self.thread.id), "the turn failed");
            }
            Event::Exited(status) => {
                log::warn!("agent exited: {status}");
                self.thread.ready = false;
                self.thread.status = "exited".into();
                if self.thread.busy {
                    self.return_pending();
                    self.thread.fail(format!("the agent exited ({status})"));
                    self.restore_queue(cx);
                }
                self.thread.applying = None;
                self.unstart();
                self.drop_waiters(command::Caller::Thread(self.thread.id), "the agent exited");
            }
        }
        self.thread.relist(changed);
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

    /// Ctrl+P: shows the finder over the workspace's files, with the last query selected.
    fn show_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder_open = true;
        self.refind();
        nsh::finder::start(&self.finder_query);
        self.finder.update(cx, |finder, cx| finder.open(window, cx));
        cx.notify();
    }

    /// Refilters the finder's matches for its query, and shows them.
    fn refind(&mut self) {
        self.finder_matches = match_files(&self.project.files, &self.finder_query);
        nsh::finder::publish(nsh::finder::Snapshot {
            matches: self.finder_matches.clone(),
            capped: (self.project.files.len() >= MAX_FILES).then_some(MAX_FILES),
        });
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

    /// Ctrl+,: opens the settings page, or closes it when it is already open.
    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            return self.hide_settings(window, cx);
        }
        nsh::settings::start(self.swapped);
        self.settings_open = true;
        self.settings.update(cx, |settings, cx| settings.open(window, cx));
        cx.notify();
    }

    fn toggle_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.issues_open {
            return self.hide_issues(window, cx);
        }
        self.follow_issues();
        self.issues_open = true;
        self.issues.update(cx, |issues, cx| issues.open(window, cx));
        cx.notify();
    }

    fn hide_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.issues_open {
            return;
        }
        self.issues_open = false;
        self.issues.update(cx, |issues, _| issues.close());
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Points the shared issues at the project on screen, if it isn't already. The sidebar's
    /// snapshot does this, so the page and the counts follow the project whatever switched it.
    fn follow_issues(&self) {
        let mut shown = self.issues_root.borrow_mut();
        if *shown != self.project.root {
            *shown = self.project.root.clone();
            issues::shared::set_project(&self.project.root);
        }
    }

    /// Starts a thread on an issue, in a worktree of its own when the project is a repo, and moves
    /// the issue to In Progress with the branch linked.
    fn work_on_issue(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_issues();
        let tracker = issues::shared::watch().borrow().tracker.clone();
        if let Err(message) = issues::api::check_workable(&tracker, number) {
            self.worktree_status = Some(message);
            cx.notify();
            return;
        }
        let Some(prompt) = issues::api::work_prompt(&tracker, number) else {
            return;
        };
        // Another worktree is being made or merged, or there is no agent to start: nothing would
        // begin, so the issue is left as it is.
        if self.finishing || self.chosen().is_none() {
            return;
        }
        self.hide_issues(window, cx);
        self.new_blank_thread(window, cx);
        self.thread.isolate = true;
        let root = self.project.root.clone();
        self.issue_for_worktree = Some((root.clone(), number));
        let out = Outgoing { text: prompt, origin: Origin::User, images: vec![] };
        self.start_thread(out, false, window, cx);
        // With a worktree being made, the issue waits for its branch. Without one the thread
        // went ahead in the project itself, if it started at all.
        if !self.finishing {
            self.issue_for_worktree = None;
            if !self.thread.blank {
                Self::issue_started(&root, number, None);
            }
        }
        cx.notify();
    }

    /// `issue.show`: puts the issues page on screen where the request says. Only for the project
    /// on screen, since the page shows that one.
    fn show_issues(&mut self, id: u64, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        if self.project_of(id).is_some_and(|project| project != self.project_id) {
            return Err("that project is not on screen; the issues page shows the one that is".into());
        }
        // Like any move of what the user is looking at, it is the thread on screen's to make, and
        // not while the user is typing or scrolling.
        if self.threads_all().any(|thread| thread.id == id) {
            if self.thread.id != id {
                return Err("only the thread on screen can put the issues page up".into());
            }
            if self.last_input.elapsed() < QUIET {
                return Err("the user is typing or scrolling; ask again in a moment".into());
            }
        }
        self.follow_issues();
        let nav = issues::api::show_nav(&issues::shared::watch().borrow().tracker, args)?;
        if !self.issues_open {
            self.toggle_issues(window, cx);
        }
        issues::shared::navigate(nav);
        Ok(serde_json::json!({ "shown": true }))
    }

    /// An issue's work has begun: In Progress, and the branch it is on. In the issues of the repo
    /// `root` belongs to, which need not be the one on screen.
    fn issue_started(root: &std::path::Path, number: u64, branch: Option<String>) {
        issues::shared::change_repo(root, |tracker, now| {
            tracker.update(number, now, |issue| {
                if issue.status.is_open() && issue.status != issues::Status::InReview {
                    issue.status = issues::Status::InProgress;
                }
                if let Some(branch) = &branch {
                    let link = issues::Link::Branch(branch.clone());
                    if !issue.links.contains(&link) {
                        issue.links.push(link);
                    }
                }
            });
        });
    }

    fn hide_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.settings.update(cx, |settings, _| settings.close());
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Takes up the options of `config.json` that are read once, after the settings page saved
    /// them. The theme and fonts are reapplied by the config watcher.
    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let settings = config::load();
        self.notifications = settings["notifications"].as_bool().unwrap_or(true);
        let idle_minutes = settings["idle_minutes"].as_u64().unwrap_or(15);
        self.idle_after = (idle_minutes > 0).then(|| Duration::from_secs(idle_minutes * 60));
        self.max_live = settings["max_live_agents"].as_u64().unwrap_or(8).max(1) as usize;
        if let Some(left) = config::viewer_left(&settings) {
            self.swapped = left;
            self.request_save(cx);
        }
        // An agent that was added on the command line isn't in the config; it stays.
        let mut agents = config::agents(&settings);
        let kept: Vec<_> = self.agents.iter().filter(|agent| agent.vouched && !agents.iter().any(|known| known.id == agent.id)).cloned().collect();
        agents.extend(kept);
        self.agents = agents;
        cx.notify();
    }

    /// Hides the finder and gives the keys back to the reader.
    fn hide_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder_open = false;
        self.finder.update(cx, |finder, _| finder.close());
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn open(&mut self, path: &str, _window: &mut Window, cx: &mut Context<Self>) {
        self.browse = false;
        let full = self.project.root.join(path);
        let too_big = std::fs::metadata(&full).is_ok_and(|m| m.len() > 4 * 1024 * 1024);
        // A file too big to show isn't read at all.
        let read = if too_big { Ok(None) } else { std::fs::read_to_string(&full).map(Some) };
        match read {
            Ok(None) => {
                self.project.code = None;
                self.project.is_markdown = false;
                self.project.notice = Some(format!("{path} is too large to show"));
            }
            Ok(Some(text)) => {
                self.project.is_markdown = is_markdown(path);
                // The same file keeps its place: the editor swaps the text in, and the scroll position
                // and caret survive.
                self.project.code = Some(nsh::source::Shared(Arc::from(text)));
                self.project.notice = None;
            }
            Err(error) => {
                log::warn!("open {path}: {error}");
                self.project.code = None;
                self.project.is_markdown = false;
                self.project.notice = Some(format!("{path} is not a text file"));
            }
        }
        self.project.current = Some(path.to_string());
        self.project.stamp = stamp(&full);
        self.project.files_tree.select(path);
        cx.notify();
    }

    /// Sends the draft: as a new turn, or, while the agent is working, into the queue
    /// (`later` for after the turn rather than into it).
    fn send(&mut self, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.thread.draft.trim().to_string();
        if prompt.is_empty() && self.thread.attachments.is_empty() {
            return;
        }
        self.submit(Outgoing::user(prompt, self.thread.attachments.clone()), later, window, cx);
    }

    /// Takes a message headed for the agent wherever the thread is: a blank or unstarted one
    /// starts; one still starting, loading or waking from sleep waits; a busy one is steered
    /// (dex) or queued; a ready idle one dispatches now. The draft is cleared exactly where it
    /// was before, so a message that can't be sent yet stays where the user can edit it.
    fn submit(&mut self, out: Outgoing, later: bool, window: &mut Window, cx: &mut Context<Self>) -> Where {
        let thread = &self.thread;
        let starting = thread.client.is_some()
            && (thread.applying.is_some() || !thread.ready && !thread.loading && !thread.picking && thread.restore.is_none());
        let takes_messages = thread.client.as_ref().is_some_and(|client| client.queues()) && thread.pending.is_none();
        let where_ = route(thread.blank, thread.unstarted(), thread.asleep(), starting, thread.busy, takes_messages, later);
        // Only the user's text is taken out of the draft; text from a thread or from Po was
        // never in it, and their draft is theirs.
        let from_user = matches!(out.origin, Origin::User);
        match where_ {
            Where::Start => {
                self.start_thread(out, later, window, cx);
                Where::Start
            }
            Where::Wait => {
                // The agent is still starting, or taking its settings: the message waits for
                // it, shown as sent. A sleeping thread is woken by it.
                if thread.asleep() {
                    self.wake_thread(window, cx);
                }
                if from_user {
                    self.clear_draft();
                }
                self.begin_turn(&out);
                self.thread.pending = Some(out);
                cx.notify();
                Where::Wait
            }
            Where::Dispatch => {
                if self.dispatch(out, cx) && from_user {
                    self.clear_draft();
                }
                Where::Dispatch
            }
            Where::Take(kind) => {
                if from_user {
                    self.clear_draft();
                }
                self.thread.client.as_ref().unwrap().enqueue(&out.queued_text(), kind);
                self.thread.queue.push(Queued { out, kind: Some(kind) });
                cx.notify();
                Where::Take(kind)
            }
            Where::Queue => {
                if from_user {
                    self.clear_draft();
                }
                self.thread.queue.push(Queued { out, kind: None });
                cx.notify();
                Where::Queue
            }
        }
    }

    /// Attaches pictures to the draft, read and shrunk off the UI thread. What isn't a picture
    /// is left out.
    fn attach(&mut self, sources: Vec<Source>, cx: &mut Context<Self>) {
        let thread = self.thread.id;
        self.thread.attach_note = None;
        cx.spawn(async move |this, cx| {
            let (made, skipped) = cx
                .background_executor()
                .spawn(async move {
                    let mut made = Vec::new();
                    let mut skipped = 0;
                    for source in sources {
                        let result = match &source {
                            Source::Bytes(bytes) => Attachment::from_bytes(bytes.clone()),
                            Source::Path(path) => Attachment::from_path(path),
                        };
                        match result {
                            Ok(attachment) => made.push(Arc::new(attachment)),
                            Err(error) => {
                                log::info!("attach: {error:#}");
                                skipped += 1;
                            }
                        }
                    }
                    (made, skipped)
                })
                .await;
            // The thread may have been switched away from meanwhile; the pictures still go to it.
            this.update(cx, |reader, cx| {
                reader.with_thread(thread, |reader, _| {
                    let room = attach::MAX_PER_MESSAGE.saturating_sub(reader.thread.attachments.len());
                    let over = made.len().saturating_sub(room);
                    reader.thread.attachments.extend(made.into_iter().take(room));
                    let mut notes = Vec::new();
                    if skipped > 0 {
                        notes.push(format!("{skipped} not attached: only PNG, JPEG, WebP and GIF pictures are taken"));
                    }
                    if over > 0 {
                        notes.push(format!("{over} not attached: a message holds at most {} pictures", attach::MAX_PER_MESSAGE));
                    }
                    reader.thread.attach_note = (!notes.is_empty()).then(|| notes.join("; "));
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sets the draft to `text` from the app itself (a cleared draft after a send, a taken-back
    /// queue row), marking the change so the screen the conversation draws picks the text up.
    fn update_draft(&mut self, text: &str) {
        self.chat_reset.set(self.chat_reset.get() + 1);
        self.thread.draft = text.to_string();
    }

    /// Clears the draft, and the pictures attached to it.
    fn clear_draft(&mut self) {
        self.thread.attachments.clear();
        self.thread.attach_note = None;
        self.update_draft("");
    }

    /// Takes a queued message back into the draft to edit.
    fn recall(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.thread.queue.len() {
            return;
        }
        let item = self.thread.queue.remove(ix);
        if let (Some(kind), Some(client)) = (item.kind, &self.thread.client) {
            client.recall(&item.out.queued_text(), kind);
        }
        let draft = self.thread.draft.clone();
        self.thread.attachments.splice(0..0, item.out.images.iter().cloned());
        self.thread.attachments.truncate(attach::MAX_PER_MESSAGE);
        let text = if draft.trim().is_empty() { item.out.text } else { format!("{}\n{draft}", item.out.text) };
        self.update_draft(&text);
        cx.notify();
    }

    /// If no turn is running, sends the oldest queued message that waits here rather than in
    /// the daemon. A message that can't be sent goes back to the draft with the rest.
    fn send_next(&mut self, cx: &mut Context<Self>) {
        if self.thread.busy {
            return;
        }
        let Some(at) = self.thread.queue.iter().position(|q| q.kind.is_none()) else { return };
        let mut next = self.thread.queue.remove(at);
        if !self.dispatch(std::mem::replace(&mut next.out, Outgoing::user(String::new(), vec![])), cx) {
            self.thread.queue.insert(0, next);
            self.restore_queue(cx);
        }
    }

    /// Moves everything queued back into the draft, taking back what the daemon still holds.
    /// The user's own queued messages are theirs to edit again; text sent through the API is
    /// dropped, and reported to its sender (phase 2's notifications) rather than typed over.
    fn restore_queue(&mut self, cx: &mut Context<Self>) {
        if self.thread.queue.is_empty() {
            return;
        }
        let items: Vec<Queued> = std::mem::take(&mut self.thread.queue);
        let mut text = Vec::new();
        for item in items {
            if let (Some(kind), Some(client)) = (item.kind, &self.thread.client) {
                client.recall(&item.out.queued_text(), kind);
            }
            if matches!(item.out.origin, Origin::User) {
                self.thread.attachments.splice(0..0, item.out.images.iter().cloned());
                self.thread.attachments.truncate(attach::MAX_PER_MESSAGE);
                text.push(item.out.text);
            } else {
                // Kept for `thread.get` to report; phase 2 notifies the sender as well.
                let dropped = &mut self.thread.dropped;
                dropped.push(item.out.text);
                if dropped.len() > 8 {
                    dropped.drain(..dropped.len() - 8);
                }
            }
        }
        let draft = self.thread.draft.clone();
        if !draft.trim().is_empty() {
            text.push(draft);
        }
        let joined = text.join("\n\n");
        self.update_draft(&joined);
        cx.notify();
    }

    /// Sends `out` as a new turn; false if it could not be sent (the error is in the thread).
    fn dispatch(&mut self, out: Outgoing, cx: &mut Context<Self>) -> bool {
        if !self.thread.ready || !self.send_to_agent(&out) {
            let why = if self.thread.client.is_some() && self.thread.ready { "The agent has no session yet." } else { "The agent is not ready yet." };
            self.thread.fail(why.into());
            cx.notify();
            return false;
        }
        self.begin_turn(&out);
        self.request_save(cx);
        cx.notify();
        true
    }

    /// Hands `out` to the agent, labelled by where it came from: the user's messages say what
    /// file is in view (the thread on screen's project; that is where they type), a thread's
    /// messages say which thread sent them, and Po's say they are Po's.
    fn send_to_agent(&self, out: &Outgoing) -> bool {
        let Some(client) = &self.thread.client else { return false };
        let context = match (&self.project.current, &out.origin) {
            (Some(file), Origin::User) => format!("[The user has `{file}` open in the viewer.]\n\n{}", out.text),
            (_, Origin::Thread { id, title }) => format!("[From thread \"{title}\" (#{id}), sent through Po.]\n\n{}", out.text),
            (_, Origin::Po) => format!("[Po]\n\n{}", out.text),
            (None, Origin::User) => out.text.clone(),
        };
        client.prompt(&context, &out.images)
    }

    /// Shows `prompt` as the start of a turn the agent is working on.
    fn begin_turn(&mut self, out: &Outgoing) {
        self.thread.blank = false;
        self.thread.busy = true;
        self.thread.failed = false;
        self.thread.last_error = None;
        self.thread.touched = Instant::now();
        self.thread.active_at = now_ms();
        // The message that was last stops being so: it loses its bottom padding and folds its
        // finished runs of tool calls, so it is laid out again too.
        let was_last = self.thread.messages.len().saturating_sub(1);
        self.thread.messages.push(Msg::User(out.shown(), out.images.clone()));
        self.thread.messages.push(Msg::Agent(vec![]));
        self.thread.relist(was_last);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
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
        self.restore_queue(cx);
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
        let mut from = None;
        for (ix, msg) in self.thread.messages.iter_mut().enumerate() {
            let Msg::Agent(blocks) = msg else { continue };
            for start in folded_runs(blocks) {
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
    /// The file tree beside the viewer, drawn by NSH from what it publishes. `lead` and `trail` are
    /// the window's own controls, when this is the leftmost or rightmost pane.
    fn explorer(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> AnyElement {
        let diff = self.mode == Mode::Diff;
        let tree = if diff { &self.project.changes_tree } else { &self.project.files_tree };
        let body = if diff && self.project.changes.is_empty() {
            nsh::explorer::Body::Empty("No changes".into())
        } else {
            let marks = &self.project.marks;
            nsh::explorer::Body::Rows(
                tree.rows()
                    .into_iter()
                    .map(|row| nsh::explorer::Entry { mark: diff.then(|| marks.get(row.id.as_str()).copied()).flatten(), row })
                    .collect(),
            )
        };
        nsh::strip::publish(nsh::strip::Snapshot {
            worktree: self.project.worktree.as_ref().map(|wt| nsh::strip::Worktree {
                branch: wt.spec.branch.clone(),
                confirm_discard: self.confirm_discard,
            }),
            pr: self
                .project
                .pr
                .as_ref()
                .map(|pr| nsh::strip::Pr::Opened { label: format!("PR #{} · {} files", pr.number, self.project.changes.len()) })
                .or_else(|| diff.then(|| nsh::strip::Pr::Asking { status: self.pr_status.clone(), opening: self.opening })),
            pr_reset: self.pr_serial,
        });
        nsh::explorer::publish(nsh::explorer::Snapshot { glass: cx.global::<Palette>().glass.is_some(), changes: diff, lead, trail, body });
        div().size_full().child(self.explorer_pane.clone()).into_any_element()
    }

    /// Opens, or shows the diff of, the file selected in the explorer's tree, as clicking it does.
    fn open_selected(&mut self, by_user: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Source => {
                let Some(path) = self.project.files_tree.selected_file().map(str::to_string) else { return };
                // A file picked in the tree is what the viewer shows, not an agent's view.
                if by_user {
                    self.thread.shown = None;
                }
                if self.project.current.as_deref() != Some(&path) {
                    self.open(&path, window, cx);
                }
            }
            Mode::Diff => {
                let Some(path) = self.project.changes_tree.selected_file().map(str::to_string) else { return };
                if by_user {
                    self.thread.shown = None;
                }
                if self.project.picked.as_deref() != Some(&path) {
                    self.pick(&path, cx);
                }
            }
        }
        if by_user {
            self.reveal_side(cx);
        }
    }

    /// Switches a narrow viewer pane between the explorer and the file.
    fn set_browsing(&mut self, browse: bool, cx: &mut Context<Self>) {
        self.browse = browse;
        cx.notify();
    }

    /// Whether a thread answers the sidebar's filter: its title or its latest line says it.
    fn matches_query(&self, thread: &Thread, query: &str) -> bool {
        self.thread_label(thread).to_lowercase().contains(query) || thread.preview().to_lowercase().contains(query)
    }

    /// The left side: every project with its threads, drawn by NSH from what [`Self::sidebar_snapshot`]
    /// publishes.
    fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        nsh::sidebar::publish(self.sidebar_snapshot(cx));
        floating(div().size_full().flex().flex_col().child(self.sidebar.clone()), cx)
    }

    /// What the sidebar shows: the projects and threads that answer its filter, what waits on the
    /// user, and what agents have suggested.
    fn sidebar_snapshot(&self, cx: &App) -> nsh::sidebar::Snapshot {
        use nsh::sidebar as side;
        let query = self.side_query.trim().to_lowercase();
        let searching = !query.is_empty();
        let views = self.views();
        let many = views.len() > 1;
        let (active_project, active_thread) = (self.project_id, self.thread.id);
        let thread_row = |pid: u64, thread: &Thread| side::Thread {
            project: pid,
            id: thread.id,
            label: self.thread_label(thread),
            mark: match thread.activity() {
                Activity::Idle => side::Mark::Idle,
                Activity::Unread => side::Mark::Unread,
                Activity::Running => side::Mark::Running,
                Activity::Asking => side::Mark::Asking,
            },
            selected: (pid, thread.id) == (active_project, active_thread),
            controlled: self.grants.get(&thread.id).is_some_and(|grant| *grant != command::Grant::default()),
            day: None,
        };
        let today = date::today();
        // Filtering keeps a project whose name matches, with all its threads, and any project with a
        // thread that does. A thread waiting on the user is in Needs you instead.
        let project_hit = |view: &View<'_>| view.project.name().to_lowercase().contains(&query);
        // Questions come first, the rest in the order they were made, so rows don't reshuffle as turns end.
        let mut waiting: Vec<(&View<'_>, &Thread)> = views
            .iter()
            .flat_map(|view| view.threads.iter().map(move |thread| (view, *thread)))
            .filter(|(view, thread)| self.waits_on_user(view.id, thread))
            .filter(|(view, thread)| !searching || project_hit(view) || self.matches_query(thread, &query))
            .collect();
        waiting.sort_by_key(|(_, thread)| (thread.activity() != Activity::Asking, thread.id));
        let waiting = waiting.into_iter().map(|(view, thread)| side::Waiting { thread: thread_row(view.id, thread), project: view.project.name() }).collect();

        let projects = views
            .iter()
            .filter(|view| !searching || project_hit(view) || view.threads.iter().any(|t| !self.waits_on_user(view.id, t) && self.matches_query(t, &query)))
            .map(|view| {
                let pid = view.id;
                let collapsed = self.projects_collapsed.contains(&pid) && !searching;
                // Most recently active first. Only the active ones show; the rest fold under "more".
                let mut recent: Vec<&Thread> = view.threads.iter().copied().filter(|t| (!t.blank || t.id == view.shown) && !self.waits_on_user(pid, t)).collect();
                recent.sort_by_key(|t| (std::cmp::Reverse(t.active_at), std::cmp::Reverse(t.id)));
                if searching && !project_hit(view) {
                    recent.retain(|t| self.matches_query(t, &query));
                }
                let expanded = self.threads_expanded.contains(&pid) || searching;
                // Active: running, finished unseen with its parent told, or an agent still alive; the one
                // on screen too, whatever it is doing.
                let active = |t: &Thread| t.id == view.shown || t.activity() != Activity::Idle || t.client.is_some();
                let shown_count = recent.iter().filter(|t| active(t)).count();
                let foldable = !searching && recent.len() > shown_count.max(1);
                let hidden = if expanded || !foldable {
                    0
                } else {
                    let keep: Vec<&Thread> = recent.iter().copied().filter(|t| active(t)).collect();
                    let hidden = recent.len() - keep.len().max(1);
                    recent = if keep.is_empty() { recent[..1].to_vec() } else { keep };
                    hidden
                };
                if collapsed {
                    recent.clear();
                }
                let head = self.heads.get(&view.project.root).filter(|_| view.project.shows_branch());
                side::Project {
                    id: pid,
                    name: view.project.name(),
                    branch: head.map(|head| side::Branch { name: head.name.to_string(), dirty: head.dirty }),
                    active: pid == active_project,
                    collapsed,
                    closable: many,
                    threads: {
                        let mut last = None;
                        recent
                            .into_iter()
                            .map(|t| {
                                let day = match today.days_since(date::Date::of_ms(t.active_at)) {
                                    ..=0 => "",
                                    1 => "Yesterday",
                                    2..=6 => "Previous 7 days",
                                    _ => "Older",
                                };
                                let mut row = thread_row(pid, t);
                                row.day = (last != Some(day) && !day.is_empty()).then_some(day);
                                last = Some(day);
                                row
                            })
                            .collect()
                    },
                    fold: (foldable && !collapsed && (hidden > 0 || expanded)).then_some((expanded, hidden)),
                }
            })
            .collect();

        let suggestions = self
            .suggestions
            .iter()
            .map(|suggestion| {
                let (text, yes) = match &suggestion.what {
                    Proposal::Go(project) => {
                        let name = views.iter().find(|view| view.id == *project).map_or_else(|| "a project".to_string(), |view| view.project.root.file_name().map_or_else(|| "a project".into(), |name| name.to_string_lossy().into_owned()));
                        (format!("“{}” suggests {name}", suggestion.by), "Go")
                    }
                    Proposal::Open(root) => (format!("“{}” wants to open {}", suggestion.by, root.display()), "Open"),
                    Proposal::Pr { spec, .. } => (format!("“{}” wants to check out PR {spec}", suggestion.by), "Check out"),
                    Proposal::Merge(project) => {
                        let what = views.iter().find(|view| view.id == *project).and_then(|view| view.project.worktree.as_ref()).map_or_else(|| "a worktree".to_string(), |wt| format!("{} into {}", wt.spec.branch, wt.spec.repo.display()));
                        (format!("“{}” wants to merge {what}", suggestion.by), "Merge")
                    }
                };
                side::Suggestion { id: suggestion.id, text, yes }
            })
            .collect();

        side::Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            waiting,
            projects,
            suggestions,
            status: self.worktree_status.clone(),
            searching,
            issues_open: self.issues_open,
            issues: {
                self.follow_issues();
                issues::shared::summary()
            },
        }
    }

    /// The conversation, drawn by NSH from what [`Self::chat_snapshot`] publishes; published here in place
    /// of the pane, as the viewer's panes are.
    fn chat_pane(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> gpui::Div {
        nsh::chat::publish(self.chat_snapshot(lead, trail, cx));
        div().size_full().flex().flex_col().child(self.chat.clone())
    }

    /// What the conversation shows: the header, the transcript, and the composer.
    fn chat_snapshot(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &App) -> nsh::chat::Snapshot {
        use nsh::chat as chat;
        let thread = &self.thread;
        // Bump the transcript's follow (the pane scrolls to its end) when the thread the pane
        // shows got louder: a message came or went, or the turn started or stopped.
        let busy = thread.busy;
        let blocks = match thread.messages.last() {
            Some(Msg::Agent(blocks)) => blocks.len(),
            _ => 0,
        };
        // A thread other than the one last shown starts from its own draft and its own end.
        let shown = (self.project_id, thread.id);
        if self.chat_shown.replace(shown) != shown {
            self.chat_reset.set(self.chat_reset.get() + 1);
        }
        let growth = match thread.messages.last() {
            Some(Msg::Agent(blocks)) => match blocks.last() {
                Some(Block::Text(text)) | Some(Block::Thought(text, _)) => text.len(),
                _ => 0,
            },
            _ => 0,
        };
        let louder = (shown, thread.messages.len(), blocks, busy, growth);
        let before = self.chat_marks.get();
        let (follow, follow_soft) = if louder == before {
            (self.chat_follow.get(), self.chat_follow_soft.get())
        } else if (louder.0, louder.1) != (before.0, before.1) {
            // Another thread, or a message came or went: the end is where to be.
            self.chat_follow.set(self.chat_follow.get() + 1);
            (self.chat_follow.get(), self.chat_follow_soft.get())
        } else {
            // Only the turn going on: whoever scrolled up to read stays there.
            self.chat_follow_soft.set(self.chat_follow_soft.get() + 1);
            (self.chat_follow.get(), self.chat_follow_soft.get())
        };
        self.chat_marks.set(louder);
        let mut statuses = Vec::new();
        if let Some(line) = self.children_status(thread.id) {
            statuses.push(line);
        }
        if !thread.status.is_empty() {
            statuses.push(thread.status.clone());
        }
        let picking = thread.picking.then(|| match &thread.sessions {
            Some(sessions) if !thread.loading && !sessions.is_empty() => chat::Picking::Sessions(sessions.iter().map(|session| chat::Session {
                id: session.id.clone(),
                title: session.title.clone(),
                when: session.updated.get(..16).unwrap_or_default().replace('T', " "),
            }).collect()),
            Some(_) if !thread.loading => chat::Picking::Note(format!("No sessions to resume in {}", self.project.root.display())),
            _ => chat::Picking::Note(thread.status.clone()),
        });
        let grant = self.grants.get(&thread.id).copied().unwrap_or_default();
        chat::Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            title: thread.title(),
            lead,
            trail,
            statuses,
            blank: thread.blank,
            busy,
            steer: thread.client.as_ref().is_some_and(|client| client.queues()),
            agents: self.agents.iter().map(|agent| (agent.name.clone(), agent.id.clone())).collect(),
            chosen: thread.agent.clone().or_else(|| self.chosen_agent.clone()),
            settings: Thread::ordered(thread.options.iter().collect()).into_iter().map(|option| chat::Setting {
                id: option.id.clone(),
                name: option.name.clone(),
                current: option.current.clone(),
                choices: option.choices.clone(),
            }).collect(),
            isolate: worktree::in_repo(&self.project.root).then(|| thread.isolate),
            grant: Some(grant).map(|g| chat::Grant {
                scope: match g.scope {
                    command::Scope::Viewer => chat::Scope::Viewer,
                    command::Scope::Orchestrate => chat::Scope::Orchestrate,
                    command::Scope::Global => chat::Scope::Global,
                },
                read: g.read_project,
                approvals: g.approvals,
            }),
            queue: thread.queue.iter().enumerate().map(|(at, queued)| chat::Queued {
                at,
                kind: match queued.kind {
                    Some(Queue::Steer) => "Steer".into(),
                    Some(Queue::Followup) => "Follow-up".into(),
                    None => String::new(),
                },
                text: queued.out.text.clone(),
                pictures: queued.out.images.len(),
            }).collect(),
            pictures: thread.attachments.iter().map(|a| chat::Picture { uri: a.data_uri() }).collect(),
            note: thread.attach_note.clone(),
            items: chat_items(&thread.messages),
            picking,
            draft: thread.draft.clone(),
            draft_reset: self.chat_reset.get(),
            focus: self.chat_focus.get(),
            follow,
            follow_soft,
        }
    }

    /// Does what the user asked of on the conversation the NSH pane draws.
    fn chat_cmd(&mut self, cmd: nsh::chat::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use nsh::chat::{Cmd as Chat, GrantChange, Scope as ChatScope};
        match cmd {
            Chat::Draft(text) => self.thread.draft = text,
            Chat::Send { later } => self.send(later, window, cx),
            Chat::Stop => self.stop(cx),
            Chat::Drop(uris) => self.attach(uris.iter().filter_map(|uri| file_of(uri)).map(|path| Source::Path(PathBuf::from(path))).collect(), cx),
            Chat::Paste(pictures) => self.attach(pictures.into_iter().map(Source::Bytes).collect(), cx),
            Chat::Detach(at) => {
                if at < self.thread.attachments.len() {
                    self.thread.attachments.remove(at);
                }
                cx.notify();
            }
            Chat::Recall(at) => self.recall(at, cx),
            Chat::Answer { message, block, at } => self.answer(message, block, at, cx),
            Chat::Approve { message, block, yes } => self.answer_approval(message, block, yes, window, cx),
            Chat::ToggleTool { message, block } => self.toggle_tool(message, block, cx),
            Chat::ToggleThought { message, block } => self.toggle_thought(message, block, cx),
            Chat::Fold { message, block, folded } => self.fold_run(message, block, folded, cx),
            Chat::Open(path) => self.reveal(&path, window, cx),
            Chat::Resume(id) => self.resume_session(&id, window, cx),
            Chat::History => self.resume_thread(window, cx),
            Chat::PickAgent(id) => self.pick_agent(id, window, cx),
            Chat::Setting { id, value } => self.choose_option(&id, &value, cx),
            Chat::ClosePicker => {
                self.thread.drop_agent();
                cx.notify();
            }
            Chat::DismissNote => {
                self.thread.attach_note = None;
                cx.notify();
            }
            Chat::Isolate(isolate) => {
                self.thread.isolate = isolate;
                cx.notify();
            }
            Chat::Grant(change) => {
                let id = self.thread.id;
                let grant = self.grants.get(&id).copied().unwrap_or_default();
                match change {
                    GrantChange::Set(set) => {
                        let scope = match set.scope {
                            ChatScope::Viewer => command::Scope::Viewer,
                            ChatScope::Orchestrate => command::Scope::Orchestrate,
                            ChatScope::Global => command::Scope::Global,
                        };
                        self.set_grant(id, scope, set.read, set.approvals, cx);
                    }
                    GrantChange::Read => self.set_grant(id, grant.scope, !grant.read_project, grant.approvals, cx),
                    GrantChange::Approvals => self.set_grant(id, grant.scope, grant.read_project, !grant.approvals, cx),
                    GrantChange::Stop => self.stop_agent_control(id, cx),
                }
            }
        }
    }

    /// Does what the user asked of a screen drawn by NSH.
    fn run(&mut self, cmd: nsh::bus::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use nsh::bus::Cmd;
        match cmd {
            Cmd::SideQuery(query) => {
                self.side_query = query;
                cx.notify();
            }
            Cmd::ToggleProject(pid) => {
                let expanding = !self.projects_collapsed.insert(pid);
                if expanding {
                    self.projects_collapsed.remove(&pid);
                }
                // Folding is only a redraw; the project is switched to only when it opens another one.
                if expanding && pid != self.project_id {
                    self.show_project(pid, window, cx);
                } else {
                    self.request_save(cx);
                    cx.notify();
                }
            }
            Cmd::StartThread(pid) => self.start_thread_in(pid, window, cx),
            Cmd::CloseProject(pid) => self.close_project(pid, window, cx),
            Cmd::ToggleMore(pid) => {
                if !self.threads_expanded.insert(pid) {
                    self.threads_expanded.remove(&pid);
                }
                cx.notify();
            }
            Cmd::OpenThread(pid, tid) => self.open_thread(pid, tid, window, cx),
            Cmd::CloseThread(pid, tid) => self.close_any_thread(pid, tid, window, cx),
            Cmd::AddProject => self.pick_project(window, cx),
            Cmd::NewThread => self.new_blank_thread(window, cx),
            Cmd::OpenSettings => self.toggle_settings(window, cx),
            Cmd::ToggleIssues => self.toggle_issues(window, cx),
            Cmd::CloseIssues => self.hide_issues(window, cx),
            Cmd::WorkOnIssue(number) => self.work_on_issue(number, window, cx),
            Cmd::ToggleLeft => self.toggle_left(cx),
            Cmd::ToggleRight => self.toggle_right(cx),
            Cmd::ToggleFocus => self.toggle_focus(cx),
            Cmd::Browse(browse) => self.set_browsing(browse, cx),
            Cmd::ToggleRendered => self.toggle_rendered(cx),
            Cmd::SetSplit(split) => self.set_split(split, cx),
            Cmd::ShowHome => {
                self.thread.shown = None;
                cx.notify();
            }
            Cmd::ShowView(id) => self.show_agent_view(&id, cx),
            Cmd::CloseView(id) => self.close_agent_view(&id, cx),
            Cmd::SetMode(diff) => self.set_mode(if diff { Mode::Diff } else { Mode::Source }, cx),
            Cmd::TreeClick(id) => {
                let tree = if self.mode == Mode::Diff { &mut self.project.changes_tree } else { &mut self.project.files_tree };
                tree.click(&id);
                self.open_selected(true, window, cx);
                cx.notify();
            }
            Cmd::TreeStep(step) => {
                let tree = if self.mode == Mode::Diff { &mut self.project.changes_tree } else { &mut self.project.files_tree };
                tree.step(step);
                // Moving the selection with the keyboard opens the file too, like a click.
                self.open_selected(true, window, cx);
                cx.notify();
            }
            Cmd::OpenUrl(url) => cx.open_url(&url),
            Cmd::FinderQuery(query) => {
                if query != self.finder_query {
                    self.finder_query = query;
                    self.refind();
                }
            }
            Cmd::FinderPick(path) => self.accept_path(&path, window, cx),
            Cmd::CloseFinder => self.hide_finder(window, cx),
            Cmd::CloseSettings => self.hide_settings(window, cx),
            Cmd::SettingsSaved => self.apply_settings(cx),
            Cmd::Answer(id, yes) => self.answer_suggestion(id, yes, window, cx),
            Cmd::SubmitForm(id, values) => self.form_submitted(&id, values, cx),
            Cmd::OpenPr(spec) => self.open_pr(&spec, window, cx),
            Cmd::ClosePr => self.close_pr(window, cx),
            Cmd::MergeWorktree => self.finish_worktree(true, window, cx),
            Cmd::DiscardWorktree => self.finish_worktree(false, window, cx),
            Cmd::Chat(chat) => self.chat_cmd(chat, window, cx),
        }
    }

    /// Whether thread `thread` of project `pid` waits on the user, which takes it out of its
    /// project and into NEEDS YOU, so a folded project can't hide it. The one on screen is
    /// already in view, so it stays where it is.
    fn waits_on_user(&self, pid: u64, thread: &Thread) -> bool {
        let parent_hears = thread.parent.is_some() && thread.notify != Notify::Never;
        (pid, thread.id) != (self.project_id, self.thread.id) && thread.activity().needs_you(parent_hears)
    }

    /// The file tree is drawn beside the viewer: the pane is wide enough for both.
    fn tree_visible(&self) -> bool {
        !self.viewer_narrow.get()
    }

    fn pick_hint(&self, what: &str) -> String {
        pick_hint(what, self.tree_visible())
    }

    /// The viewer or the diff viewer, as the project's mode says, with the file tree beside it
    /// unless that is hidden or the agent's own view is shown, which has no files to pick.
    fn pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let tabs = self.view_tabs();
        let has_tabs = tabs.is_some();
        let shown_form = self.thread.shown.as_deref().and_then(|id| self.thread.forms.get(id));
        let shown = self
            .thread
            .shown
            .as_deref()
            .and_then(|id| self.thread.views.list().iter().find(|view| view.id == id));
        let showing_agent = shown.is_some() || shown_form.is_some();
        let with_tree = !showing_agent && self.tree_visible();
        // The window's own control goes in the leftmost header: the tree's, or the viewer's.
        // A pane too narrow for the tree beside the file takes turns: the explorer fills it until a
        // file is picked, then the file does, with a button back to the explorer.
        let stacked = !showing_agent && self.viewer_narrow.get();
        let nothing_picked = if self.mode == Mode::Diff { self.project.picked.is_none() } else { self.project.current.is_none() };
        let explorer_only = stacked && (self.browse || nothing_picked);
        // A pane wide enough shows the explorer beside what it shows, with the lead controls in
        // the file's header; stacked, the file's alone gets them, with a way back to the explorer
        // tacked on.
        // The same split of the window's controls, as descriptors, for a body NSH draws.
        let (lead_controls, trail_controls) = self.pane_controls.borrow().clone();
        let tree_lead = if with_tree || explorer_only { lead_controls.clone() } else { vec![] };
        let body_controls = |lead: Vec<nsh::header::Control>| if with_tree || explorer_only { (vec![], trail_controls.clone()) } else if stacked { ([lead, vec![nsh::header::Control::Explorer]].concat(), trail_controls.clone()) } else { (lead, trail_controls.clone()) };
        let body = match (shown, shown_form, self.mode) {
            _ if explorer_only => {
                let background = cx.global::<Palette>().background;
                div().size_full().when(!has_tabs, |d| d.bg(background)).child(self.explorer(tree_lead.clone(), trail_controls.clone(), cx)).into_any_element()
            }
            (Some(view), _, _) => self.agent_view(view, body_controls(lead_controls), cx).into_any_element(),
            (_, Some(form), _) => self.form_view(form, body_controls(lead_controls), cx).into_any_element(),
            (None, None, Mode::Source) => {
                let (lead, trail) = body_controls(lead_controls);
                self.viewer(lead, trail, cx).into_any_element()
            }
            (None, None, Mode::Diff) => {
                let (lead, trail) = body_controls(lead_controls);
                self.diff_viewer(lead, trail, cx).into_any_element()
            }
        };
        let background = cx.global::<Palette>().background;
        let body = if with_tree {
            // The viewers paint their own background; the column paints its own too, unless the
            // pane does for both, which would lay a second translucent layer under glass.
            let column = div().size_full().when(!has_tabs, |d| d.bg(background)).child(self.explorer(tree_lead, vec![], cx));
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

    /// A view the agent made, as a markdown document; the pane NSH draws from what is published.
    fn agent_view(&self, view: &command::AgentView, (lead, trail): (Vec<nsh::header::Control>, Vec<nsh::header::Control>), cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        nsh::agent::publish(nsh::agent::Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            title: view.title.clone(),
            lead,
            trail,
            body: nsh::agent::Body::Markdown(nsh::source::Shared(Arc::from(view.body.as_str()))),
        });
        div().size_full().child(self.agent_pane.clone())
    }

    /// A form the agent put on screen: what it is for, then the fields the user answers, drawn
    /// and answered by NSH.
    fn form_view(&self, form: &command::AgentForm, (lead, trail): (Vec<nsh::header::Control>, Vec<nsh::header::Control>), cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let fields = form
            .fields
            .iter()
            .map(|field| nsh::agent::Field {
                name: field.name.clone(),
                label: field.label.clone(),
                options: field.options.clone(),
                value: field.value.clone(),
            })
            .collect();
        nsh::agent::publish(nsh::agent::Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            title: form.title.clone(),
            lead,
            trail,
            body: nsh::agent::Body::Form {
                id: form.id.clone(),
                fields,
                submit: form.submit.clone(),
                answered: form.answered.is_some(),
                doc: form.body.as_deref().map(|body| nsh::source::Shared(Arc::from(body))),
            },
        });
        div().size_full().child(self.agent_pane.clone())
    }

    /// Files (or changes) and the agent's views as tabs, with a prompt for one it added while
    /// the user was busy. None while the agent has made no views.
    fn view_tabs(&self) -> Option<AnyElement> {
        if self.thread.views.is_empty() && self.thread.forms.is_empty() {
            return None;
        }
        use nsh::tabs::{Snapshot, Tab};
        let titled = |id: &String| {
            self.thread
                .views
                .list()
                .iter()
                .find(|view| view.id == *id)
                .map(|view| Tab { id: view.id.clone(), title: view.title.clone() })
                .or_else(|| self.thread.forms.list().iter().find(|form| form.id == *id).map(|form| Tab { id: form.id.clone(), title: form.title.clone() }))
        };
        nsh::tabs::publish(Snapshot {
            home: if self.mode == Mode::Diff { "Changes" } else { "Files" }.into(),
            shown: self.thread.shown.clone(),
            tabs: self
                .thread
                .views
                .list()
                .iter()
                .map(|view| Tab { id: view.id.clone(), title: view.title.clone() })
                .chain(self.thread.forms.list().iter().map(|form| Tab { id: form.id.clone(), title: form.title.clone() }))
                .collect(),
            offer: self.thread.offered.as_ref().and_then(titled),
        });
        Some(div().flex_none().h(TABS_HEIGHT).child(self.tabs.clone()).into_any_element())
    }

    fn close_agent_view(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.thread.forms.remove(id) {
            let owner = self.thread.id;
            self.drop_form(owner, id);
        } else {
            self.thread.views.apply(command::Command::ViewClose { id: id.to_string() }).ok();
        }
        self.sync_view_docs(cx);
        cx.notify();
    }

    /// The user answered form `id`: keep the answers for waits that start later, and wake
    /// whoever waits on it.
    fn form_submitted(&mut self, id: &str, values: Value, cx: &mut Context<Self>) {
        let reply = serde_json::json!({ "form": id, "values": values });
        let owner = self.thread.id;
        self.thread.forms.answered(id, reply.clone());
        self.wake_form(owner, id, reply);
        cx.notify();
    }

    /// The picked file's changes, drawn by NSH from what [`Self::diff_snapshot`] publishes.
    fn diff_viewer(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        nsh::diff::publish(self.diff_snapshot(lead, trail, cx));
        div().size_full().child(self.diff.clone())
    }

    fn diff_snapshot(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &App) -> nsh::diff::Snapshot {
        use nsh::diff::{Body, Snapshot};
        let project = &self.project;
        let (title, body) = match (&project.picked, &project.versions) {
            (None, _) => ("Changes".to_string(), Body::Hint(self.pick_hint("a changed file"))),
            (Some(path), _) if project.rows.is_empty() && project.diffing.is_some() => (path.clone(), Body::Loading),
            (Some(path), None) => (path.clone(), Body::NoChanges),
            (Some(path), Some((old, new))) => (path.clone(), Body::Versions { path: path.clone(), old: old.clone(), new: new.clone() }),
        };
        Snapshot { glass: cx.global::<Palette>().glass.is_some(), title, lead, trail, split: self.split, counts: project.counts, body }
    }

    /// The open file in the viewer, drawn by NSH from what [`Self::source_snapshot`] publishes.
    fn viewer(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        nsh::source::publish(self.source_snapshot(lead, trail, cx));
        div().size_full().child(self.source.clone())
    }

    fn source_snapshot(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &App) -> nsh::source::Snapshot {
        use nsh::source::{Body, Snapshot};
        let current = self.project.current.as_deref();
        let markdown = self.project.is_markdown;
        let rendered = markdown && self.rendered;
        let body = match (&self.project.code, &self.project.notice) {
            (_, Some(notice)) => Body::Notice(notice.clone()),
            (Some(code), None) if rendered => Body::Markdown(code.clone()),
            (Some(code), None) => Body::Code { path: current.unwrap_or_default().to_string(), text: code.clone() },
            (None, None) => Body::Hint(self.pick_hint("a file")),
        };
        Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            title: current.map_or_else(|| "No file open".to_string(), str::to_string),
            lead,
            trail,
            language: current.map(language).unwrap_or("").to_string(),
            preview: markdown.then_some(self.rendered),
            body,
            reveal: self.reveal,
        }
    }

    fn toggle_rendered(&mut self, cx: &mut Context<Self>) {
        self.rendered = !self.rendered;
        cx.notify();
    }

    /// Sets what thread `id`'s agent may do through Po's API. Held in memory only, so it is
    /// lost on restart and is not shared between windows, and set only here, from the UI —
    /// never from anything an agent could write to. A raise applies when the agent next starts
    /// (its tool list is fixed then); a lowering, on its next request.
    fn set_grant(&mut self, id: u64, scope: command::Scope, read_project: bool, approvals: bool, cx: &mut Context<Self>) {
        let grant = command::Grant { scope, read_project, approvals };
        // Any reach taken away, not only the scope: the waits asked under it would answer what
        // the thread can no longer ask.
        let lowered = self.grants.get(&id).is_some_and(|before| before.scope > grant.scope || (before.read_project && !grant.read_project) || (before.approvals && !grant.approvals));
        if self.grants.get(&id) == Some(&grant) {
            return;
        }
        self.grants.insert(id, grant);
        // A lowered grant ends the thread's waits; they would answer what it can no longer ask.
        if lowered {
            self.drop_waiters(command::Caller::Thread(id), "the grant was lowered");
        }
        log::info!("control: thread {id} granted {:?} read_project={read_project} approvals={approvals}", grant.scope);
        cx.notify();
    }

    /// Ends what thread `id`'s agent may do: back to Viewer, its waiters gone (none until
    /// phase 2), and its children left running as ordinary threads.
    fn stop_agent_control(&mut self, id: u64, cx: &mut Context<Self>) {
        self.set_grant(id, command::Scope::Viewer, false, false, cx);
        // Its children go on as ordinary threads, and no longer start turns of its.
        let quiet = |thread: &mut Thread| {
            if thread.parent == Some(id) {
                thread.notify = Notify::Never;
            }
        };
        quiet(&mut self.thread);
        self.background_threads().for_each(quiet);
    }


    /// What a thread is called in the lists: a child shows under the thread that started it.
    fn thread_label(&self, thread: &Thread) -> String {
        match thread.parent.and_then(|parent| self.title_of(parent)) {
            Some(parent) => format!("↳ {parent}"),
            None => thread.title(),
        }
    }

    /// What the parent of children says in its header: `3 threads · 1 asking · 1 failed`.
    fn children_status(&self, id: u64) -> Option<String> {
        let children: Vec<&Thread> = self.threads_all().filter(|thread| thread.parent == Some(id)).collect();
        if children.is_empty() {
            return None;
        }
        let in_state = |state: crate::state::ThreadState| children.iter().filter(|t| crate::state::state(&t.facts()) == state).count();
        let mut line = format!("{} thread{}", children.len(), if children.len() == 1 { "" } else { "s" });
        let asking = in_state(crate::state::ThreadState::Asking);
        if asking > 0 {
            line.push_str(&format!(" · {asking} asking"));
        }
        let failed = in_state(crate::state::ThreadState::Failed);
        if failed > 0 {
            line.push_str(&format!(" · {failed} failed"));
        }
        Some(line)
    }
}

/// Whether a run of tool calls shows as one line. A run the user hasn't clicked starts folded,
/// whether its calls are still going or not.
fn run_folded(run: &[Block]) -> bool {
    let Some(Block::Tool(first)) = run.first() else { return false };
    first.fold.unwrap_or(true)
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
        if len >= 3 && run_folded(&blocks[n..n + len]) {
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
            pictures: pictures.iter().map(|a| nsh::chat::Picture { uri: a.data_uri() }).collect(),
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

/// One agent message's blocks for the conversation drawn by NSH. Runs of three or more tool calls
/// also carry the row that folds them; the calls stay in place either way, so the indexes their
/// answers are told by hold.
fn chat_blocks(blocks: &[Block]) -> Vec<nsh::chat::Block> {
    let mut out: Vec<nsh::chat::Block> = Vec::with_capacity(blocks.len());
    let mut n = 0;
    while n < blocks.len() {
        let run = n > 0 && matches!(&blocks[n - 1], Block::Tool(_));
        let len = if matches!(blocks[n], Block::Tool(_)) && !run { blocks[n..].iter().take_while(|b| matches!(b, Block::Tool(_))).count() } else { 0 };
        if len >= 3 {
            let run = &blocks[n..n + len];
            let phase = if run.iter().any(|b| matches!(b, Block::Tool(t) if t.status == Step::Failed)) {
                Step::Failed
            } else if run.iter().all(|b| matches!(b, Block::Tool(t) if t.status == Step::Done)) {
                Step::Done
            } else {
                Step::Working
            };
            out.push(nsh::chat::Block::Run { index: n, count: len, phase: chat_phase(phase), folded: run_folded(run) });
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
            KeyBinding::new("escape", CloseSettings, Some("Settings")),
            // The Input binds up/down itself and swallows them when single-line, so these name it.
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
    use super::{Queue, Where, route};

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
        assert_eq!(toggle_all(&mut vec![Msg::User("x".into(), vec![])], thought_flag), None);
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

    fn tool(title: &str, status: Step) -> Block {
        Block::Tool(ToolBlock { id: String::new(), title: title.into(), status, open: false, fold: None, arguments: String::new(), result: String::new(), path: None })
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
    fn only_runs_of_three_or_more_are_found_folded() {
        let blocks = vec![
            tool("a", Step::Done),
            tool("b", Step::Done),
            Block::Text("x".into()),
            tool("c", Step::Done),
            tool("d", Step::Done),
            tool("e", Step::Done),
        ];
        assert_eq!(folded_runs(&blocks), vec![3]);
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
