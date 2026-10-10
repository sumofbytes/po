#![recursion_limit = "1024"]
mod acp;
mod config;
mod diff;
mod theme;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use acp::{Client, Event, PermissionOption};
use diff::{Change, Kind, Row};
use futures::StreamExt;
use serde_json::Value;

use gpui_kit::component::{
    ActiveTheme, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, EditorState, InputEvent, Textarea, TextareaState},
    list::ListItem,
    menu::{DropdownMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel},
    text::TextView,
    tree::{TreeEvent, TreeItem, TreeState, tree},
};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FontWeight, ScrollHandle, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, WindowBounds, WindowOptions, div, prelude::*,
    px, size,
};

const SKIP: [&str; 5] = ["target", "node_modules", "dist", "build", "__pycache__"];
const MAX_FILES: usize = 5000;

fn ignored(name: &str) -> bool {
    name.starts_with('.') || SKIP.contains(&name)
}

/// Watches `dir` and its subfolders one by one, so skipped folders such as
/// `target` never take up watches.
fn watch_dirs(watcher: &mut impl notify::Watcher, root: &Path, dir: &Path) {
    if watcher.watch(dir, notify::RecursiveMode::NonRecursive).is_err() {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() && !ignored(&entry.file_name().to_string_lossy()) {
            watch_dirs(watcher, root, &path);
        }
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if path.is_dir() {
            if !ignored(&name) {
                walk(root, &path, out);
            }
        } else if let Ok(rel) = path.strip_prefix(root) {
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
        "c" | "h" => "C",
        "sh" => "Shell",
        _ => "Plain text",
    }
}

/// The highlighter's name for a file: its extension, which the editor maps to a grammar.
fn syntax(path: &str) -> SharedString {
    path.rsplit_once('.').map_or("", |(_, ext)| ext).to_lowercase().into()
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
            Some("diff") => out.push(format!("edit {}", item["path"].as_str().unwrap_or("?"))),
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
    rows: Rc<Vec<Row>>,
    root: PathBuf,
    files: Vec<String>,
    current: Option<String>,
    stamp: Option<Stamp>,
    editor: Option<Entity<EditorState>>,
    notice: Option<String>,
    tree: Entity<TreeState>,
    expanded: HashSet<SharedString>,
    draft: Entity<TextareaState>,
    thread: Vec<Msg>,
    busy: bool,
    ready: bool,
    status: String,
    client: Option<Client>,
    agent: Option<String>,
    agents: Vec<config::Agent>,
    generation: u64,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Reader {
    fn new(root: PathBuf, command: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut files = Vec::new();
        walk(&root, &root, &mut files);
        let draft = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Message the agent")
        });
        let tree = cx.new(|cx| TreeState::new(cx));
        let changes_tree = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
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
            root,
            files,
            current: None,
            stamp: None,
            editor: None,
            notice: None,
            tree,
            expanded: HashSet::new(),
            draft,
            thread: vec![],
            busy: false,
            ready: false,
            status: String::new(),
            client: None,
            agent: None,
            agents,
            generation: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        reader.refresh_tree(cx);
        if let Some(command) = command {
            reader.connect(&command, window, cx);
        }
        reader.watch(window, cx);
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

    /// Takes a fresh list of changed files; in diff mode it also reloads the picked file's diff.
    fn apply_changes(&mut self, changes: Vec<Change>, rebuild: bool, cx: &mut Context<Self>) {
        let listed = rebuild || self.changes != changes;
        self.changes = changes;
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
        match keep.or_else(|| self.changes.first().map(|c| c.path.clone())) {
            Some(path) => self.pick(&path, cx),
            None => {
                self.picked = None;
                self.rows = Rc::new(vec![]);
            }
        }
        cx.notify();
    }

    /// Shows `path`'s diff in the viewer.
    fn pick(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(change) = self.changes.iter().find(|c| c.path == path) else { return };
        self.rows = Rc::new(diff::file_diff(&self.root, change));
        if self.picked.as_deref() != Some(path) {
            self.picked = Some(path.to_string());
            let id = SharedString::from(path.to_string());
            self.changes_tree.update(cx, |tree, cx| {
                if let Some(ix) = tree.index_of(&id) {
                    tree.set_selected_index(Some(ix), cx);
                }
            });
        }
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Diff {
            let fresh = diff::changes(&self.root);
            self.apply_changes(fresh, true, cx);
        }
        cx.notify();
    }

    /// Watches the workspace: refreshes the explorer when files come or go, and
    /// reloads the open file when it changes on disk.
    fn watch(&self, window: &Window, cx: &mut Context<Self>) {
        use notify::Watcher;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                tx.unbounded_send(event).ok();
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => return log::error!("watch: {error}"),
        };
        let root = self.root.clone();
        watch_dirs(&mut watcher, &root, &root);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            // Moved in so the watcher lives as long as this task.
            let mut watcher = watcher;
            while let Some(first) = rx.next().await {
                // Let a burst of events settle, then handle them together.
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let mut events = vec![first];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                let mut changed = false;
                for path in events.iter().filter(|e| !e.kind.is_access()).flat_map(|e| &e.paths) {
                    let Ok(rel) = path.strip_prefix(&root) else { continue };
                    if rel.components().any(|c| ignored(&c.as_os_str().to_string_lossy())) {
                        continue;
                    }
                    changed = true;
                    if path.is_dir() {
                        watch_dirs(&mut watcher, &root, path);
                    }
                }
                if !changed {
                    continue;
                }
                let Ok(current) = this.read_with(cx, |reader, _| reader.current.clone()) else { break };
                let scan_root = root.clone();
                let (files, seen, changes) = cx
                    .background_executor()
                    .spawn(async move {
                        let mut files = Vec::new();
                        walk(&scan_root, &scan_root, &mut files);
                        let seen = current.as_ref().and_then(|path| stamp(&scan_root.join(path)));
                        (files, seen, diff::changes(&scan_root))
                    })
                    .await;
                let alive = handle.update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        if reader.files != files {
                            reader.files = files;
                            reader.refresh_tree(cx);
                            cx.notify();
                        }
                        reader.apply_changes(changes, false, cx);
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
        })
        .detach();
    }

    /// Replaces the running agent with `key`'s, with a fresh thread.
    fn connect(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.client = None;
        self.agent = Some(key.to_string());
        self.thread.clear();
        self.busy = false;
        self.ready = false;
        let command = self.agents.iter().find(|agent| agent.id == key).map_or(key.to_string(), |agent| agent.command.clone());
        self.status = "starting…".into();
        match Client::spawn(&command, &self.root) {
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
        self.connect(key, window, cx);
    }

    fn agent_label(&self) -> Option<&str> {
        let key = self.agent.as_ref()?;
        self.agents.iter().find(|agent| agent.id == *key).map(|agent| agent.name.as_str())
    }

    fn fail(&mut self, text: String) {
        self.busy = false;
        self.thread.push(Msg::Agent(vec![Block::Text(format!("**Error:** {text}"))]));
    }

    fn blocks(&mut self) -> &mut Vec<Block> {
        if !matches!(self.thread.last(), Some(Msg::Agent(_))) {
            self.thread.push(Msg::Agent(vec![]));
        }
        match self.thread.last_mut() {
            Some(Msg::Agent(blocks)) => blocks,
            _ => unreachable!("an agent message was just pushed"),
        }
    }

    fn on_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Ready { agent } => {
                self.ready = true;
                log::info!("agent: {agent} ready");
                self.status = "ready".into();
            }
            Event::Update(update) => {
                if let Some((path, finished)) = self.update(&update) {
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
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn update(&mut self, update: &Value) -> Option<(String, bool)> {
        let text = update["content"]["text"].as_str().unwrap_or_default();
        let mut known: Option<String> = None;
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => match self.blocks().last_mut() {
                Some(Block::Text(body)) => body.push_str(text),
                _ => self.blocks().push(Block::Text(text.to_string())),
            },
            Some("agent_thought_chunk") => match self.blocks().last_mut() {
                Some(Block::Thought(body)) => body.push_str(text),
                _ => self.blocks().push(Block::Thought(text.to_string())),
            },
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
            self.files.clear();
            walk(&self.root, &self.root, &mut self.files);
            self.refresh_tree(cx);
        }
        if self.current.as_deref() != Some(&relative) || finished {
            log::info!("agent: showing {relative}");
            self.open(&relative, window, cx);
        }
        if self.mode == Mode::Diff {
            let fresh = diff::changes(&self.root);
            self.apply_changes(fresh, false, cx);
            self.pick(&relative, cx);
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
        cx.notify();
    }

    fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let full = self.root.join(path);
        let too_big = std::fs::metadata(&full).map_or(false, |m| m.len() > 4 * 1024 * 1024);
        match std::fs::read_to_string(&full) {
            Ok(_) if too_big => self.notice = Some(format!("{path} is too large to show")),
            Ok(text) if self.current.as_deref() == Some(path) && self.editor.is_some() => {
                // Same file: swap the text in place so the scroll position survives.
                if let Some(editor) = &self.editor {
                    editor.update(cx, |state, cx| {
                        let (offset, cursor) = (state.scroll_offset(), state.cursor_position());
                        state.set_value(text, window, cx);
                        state.set_cursor_position(cursor, window, cx);
                        state.set_scroll_offset(offset, cx);
                    });
                }
                self.notice = None;
            }
            Ok(text) => {
                self.editor = Some(cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(syntax(path))
                        .line_number(true)
                        .soft_wrap(true)
                        .default_value(text)
                }));
                self.notice = None;
            }
            Err(error) => {
                log::warn!("open {path}: {error}");
                self.editor = None;
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
        self.scroll.scroll_to_bottom();
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
        cx.notify();
    }

    fn toggle_tool(&mut self, msg: usize, block: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.get_mut(msg) else { return };
        let Some(Block::Tool(tool)) = blocks.get_mut(block) else { return };
        tool.open = !tool.open;
        cx.notify();
    }

    fn explorer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
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
                    .tooltip("Source")
                    .selected(mode == Mode::Source)
                    .on_click(move |_, _, cx| source.update(cx, |reader, cx| reader.set_mode(Mode::Source, cx))),
            )
            .child(
                Button::new("mode-diff")
                    .icon(IconName::Replace)
                    .ghost()
                    .small()
                    .tooltip("Changes")
                    .selected(mode == Mode::Diff)
                    .on_click(move |_, _, cx| changes.update(cx, |reader, cx| reader.set_mode(Mode::Diff, cx))),
            );
        let title = match mode {
            Mode::Source => "Explorer",
            Mode::Diff => "Changes",
        };
        let state = if mode == Mode::Diff { &self.changes_tree } else { &self.tree };
        let marks: HashMap<String, char> = self.changes.iter().map(|c| (c.path.clone(), c.status)).collect();
        let marks = if mode == Mode::Diff { marks } else { HashMap::new() };
        // Themes may lack a green (Rosé Pine's success is teal), so added files get their own.
        let (muted, danger) = (colors.muted_foreground, colors.danger);
        let kinds = [colors.primary, colors.warning, colors.info, gpui::hsla(0.37, 0.5, 0.5, 1.0)];
        let body: AnyElement = if mode == Mode::Diff && self.changes.is_empty() {
            centered("No changes".into(), cx)
        } else {
            tree(state, move |ix, entry, selected, _, _| {
                let item = entry.item();
                let path = item.id.clone();
                let name = item.label.to_string();
                let lead: AnyElement = if entry.is_folder() {
                    div()
                        .w(px(10.0))
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(if entry.is_expanded() { "▾" } else { "▸" })
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
                    div().w(px(7.0)).h(px(7.0)).flex_none().rounded(px(2.0)).bg(tint).into_any_element()
                };
                let mark = marks.get(path.as_ref()).map(|status| {
                    let color = match status {
                        'A' | 'U' => kinds[3],
                        'D' => danger,
                        _ => kinds[1],
                    };
                    div().w(px(10.0)).flex_none().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(color).child(status.to_string())
                });
                let label = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(lead)
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .children(mark);
                let folder = entry.is_folder();
                let this = this.clone();
                ListItem::new(ix)
                    .selected(selected)
                    .pl(px(8.0 + 14.0 * entry.depth() as f32))
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
                    .child(status_text(self.root.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string()), cx))
                    .child(modes),
                cx,
            ))
            .child(div().flex_1().min_h_0().p_1().child(body))
    }

    fn diff_viewer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
        let Some(path) = self.picked.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .bg(colors.background)
                .child(header("Changes", String::new(), cx))
                .child(div().flex_1().min_h_0().child(centered("Pick a changed file".into(), cx)));
        };
        let rows = self.rows.clone();
        let (added, removed) = rows.iter().fold((0, 0), |(a, r), row| match row.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            _ => (a, r),
        });
        // Themes may lack a green (Rosé Pine's success is teal), so added lines get their own.
        let (success, danger, muted, border) =
            (gpui::hsla(0.37, 0.5, 0.5, 1.0), colors.danger, colors.muted_foreground, colors.border);
        let font = cx.theme().mono_font_family.clone();
        let body: AnyElement = if rows.is_empty() {
            centered("No textual changes".into(), cx)
        } else {
            let count = rows.len();
            // The list sizes itself from one row, so measure the widest.
            let widest = (0..count).max_by_key(|&ix| rows[ix].text.chars().count());
            gpui::uniform_list("diff-rows", count, move |range, _, _| {
                range
                    .map(|ix| {
                        let row = &rows[ix];
                        let number = |n: Option<u32>| {
                            div().w(px(44.0)).flex_none().text_right().text_color(muted).child(n.map_or(String::new(), |n| n.to_string()))
                        };
                        let (tint, sign) = match row.kind {
                            Kind::Added => (Some(success.opacity(0.16)), "+"),
                            Kind::Removed => (Some(danger.opacity(0.16)), "-"),
                            Kind::Hunk => (Some(muted.opacity(0.12)), ""),
                            Kind::Context => (None, " "),
                        };
                        let line = div().flex().min_w_full().h(px(20.0)).items_center().gap_2().font_family(font.clone()).text_sm();
                        let line = match tint {
                            Some(tint) => line.bg(tint),
                            None => line,
                        };
                        if row.kind == Kind::Hunk {
                            return line.px_2().text_color(muted).child(row.text.clone());
                        }
                        line.child(number(row.old))
                            .child(number(row.new))
                            .child(div().w(px(10.0)).flex_none().text_color(muted).child(sign))
                            .child(div().flex_1().min_w_0().whitespace_nowrap().child(row.text.clone()))
                    })
                    .collect()
            })
            .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(widest)
            .size_full()
            .into_any_element()
        };
        let right = div()
            .flex()
            .gap_2()
            .text_xs()
            .child(div().text_color(success).child(format!("+{added}")))
            .child(div().text_color(danger).child(format!("-{removed}")));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .child(header_with(&path, right, cx))
            .child(div().flex_1().min_h_0().border_color(border).child(body))
    }

    fn viewer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
        let title = self.current.clone().unwrap_or_else(|| "No file open".into());
        let body: AnyElement = match (&self.editor, &self.notice) {
            (Some(editor), _) => div()
                .size_full()
                .child(Editor::new(editor).readonly(true).bordered(false).h_full())
                .into_any_element(),
            (None, Some(notice)) => centered(notice.clone(), cx),
            _ => centered("Pick a file in the explorer".into(), cx),
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
        let colors = &cx.theme().colors;
        let this = cx.entity();
        let messages = self.thread.iter().enumerate().map(|(ix, msg)| render_msg(ix, msg, &this, cx));
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
                        Button::new("agent-select")
                            .label("New Thread")
                            .small()
                            .outline()
                            .dropdown_caret(true)
                            .dropdown_menu(move |menu, _, _| {
                                agents.iter().fold(menu, |menu, agent| {
                                    let (picker, key) = (picker.clone(), agent.id.clone());
                                    menu.item(PopupMenuItem::new(agent.name.clone()).on_click(move |_, window, cx| {
                                        picker.update(cx, |reader, cx| reader.new_thread(&key, window, cx))
                                    }))
                                })
                            }),
                    ),
                cx,
            ))
            .child(
                div()
                    .id("thread")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_3()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(messages)
                    .children(self.agent.is_none().then(|| {
                        div().flex_1().child(centered("Choose an agent from New Thread".into(), cx))
                    })),
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
                        .child(Textarea::new(&self.draft).appearance(false).bordered(false))
                        .child(div().flex().justify_end().child(action)),
                ),
            )
    }
}

fn render_msg(ix: usize, msg: &Msg, this: &Entity<Reader>, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
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
            Block::Text(text) => column.child(TextView::markdown(("answer", id), text.clone()).selectable(true)),
            Block::Thought(text) => column.child(
                div()
                    .italic()
                    .opacity(0.7)
                    .child(TextView::markdown(("thought", id), text.clone()).selectable(true)),
            ),
            Block::Tool(tool) => column.child(tool_card(ix, n, tool, this, cx)),
            Block::Permission(ask) => column.child(permission(ix, n, ask, this)),
        };
    }
    column.into_any_element()
}

fn tool_card(ix: usize, n: usize, tool: &ToolBlock, this: &Entity<Reader>, cx: &App) -> impl IntoElement {
    let colors = &cx.theme().colors;
    let (mark, tint) = match tool.status {
        Step::Waiting => ("○", colors.muted_foreground),
        Step::Working => ("◐", colors.primary),
        Step::Done => ("✓", colors.success),
        Step::Failed => ("✕", colors.danger),
    };
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
                        .max_h(px(220.0))
                        .overflow_hidden()
                        .font_family(cx.theme().mono_font_family.clone())
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
    div().flex_none().truncate().text_xs().text_color(cx.theme().colors.muted_foreground).child(text)
}

fn header_with(title: &str, right: impl IntoElement, cx: &App) -> impl IntoElement {
    let colors = &cx.theme().colors;
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
        .text_color(cx.theme().colors.muted_foreground)
        .child(text)
        .into_any_element()
}

impl Render for Reader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &cx.theme().colors;
        let (background, foreground) = (colors.background, colors.foreground);
        div().size_full().bg(background).text_color(foreground).child(
            h_resizable("shell")
                .child(resizable_panel().size(px(260.0)).size_range(px(160.0)..px(520.0)).child(self.explorer(cx)))
                .child(resizable_panel().child(match self.mode {
                    Mode::Source => self.viewer(cx).into_any_element(),
                    Mode::Diff => self.diff_viewer(cx).into_any_element(),
                }))
                .child(resizable_panel().size(px(460.0)).size_range(px(300.0)..px(760.0)).child(self.agent_panel(cx))),
        )
    }
}

/// Relaunch ourselves in its own process group with output going to a log file,
/// so the shell gets its prompt back (like `code .`).
fn detach(root: &Path, command: &Option<String>) -> std::io::Result<()> {
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
    child
        .arg(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    Ok(())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut foreground = false;
    let mut command = std::env::var("PO_AGENT").ok();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--foreground" | "-f" => foreground = true,
            "--agent" => command = Some(args.next().expect("--agent needs a command")),
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let root = path.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let root = root.canonicalize().expect("project path");
    if !foreground {
        match detach(&root, &command) {
            Ok(()) => return,
            Err(err) => eprintln!("po: could not detach ({err}), running in foreground"),
        }
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    gpui_platform::application().with_assets(gpui_kit::assets::Assets).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        theme::apply(cx);
        let bounds = Bounds::centered(None, size(px(1360.0), px(840.0)), cx);
        gpui_kit::open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |window, cx| cx.new(|cx| Reader::new(root, command, window, cx)),
        )
        .expect("window");
        cx.activate(true);
    });
}
