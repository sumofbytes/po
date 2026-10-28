//! What can be asked of Po from outside the UI: the one vocabulary agents and scripts use.
//! Nothing here touches GPUI, so a request is parsed and checked without a window.
use serde_json::{Value, json};

/// Who is asking. Decides what a command may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// A thread, by the id the window gives it: the `po mcp` server Po started under that
    /// thread's agent.
    Thread(u64),
    /// The user, from a process outside any agent Po started. `po ctl` speaks as this; an agent
    /// running it in its shell is refused by the process check, not by anything here.
    User,
}

/// How much of Po a caller has been granted. Guardrails, not a sandbox: they stop a confused or
/// prompt-injected model from doing damage through Po's tools, not an agent acting through its
/// own shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Its own views, the viewer while its thread is on screen, and its own state and options.
    Viewer,
    /// Also start, steer, read, wait on and close threads it started.
    Orchestrate,
    /// Also open, close and offer projects.
    Global,
}

impl Scope {
    fn covers(self, needed: Scope) -> bool {
        self >= needed
    }
}

/// What a caller has been granted, held in memory only: it is lost on restart and is not shared
/// between windows, so nothing an agent can read or write raises its own reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub scope: Scope,
    /// Read (never write) any thread of its own project, including its worktrees.
    pub read_project: bool,
    /// Answer permission asks of threads it started.
    pub approvals: bool,
}

impl Default for Grant {
    fn default() -> Self {
        Self { scope: Scope::Viewer, read_project: false, approvals: false }
    }
}

impl Grant {
    /// What the user has, when `po ctl` asks: everything. Nothing here may approve an agent
    /// command or an MCP server through the API; `approvals` answers permission asks only.
    pub fn user() -> Self {
        Self { scope: Scope::Global, read_project: true, approvals: true }
    }

    /// Whether a caller with this grant may run `command` on a target standing in `relation`.
    /// Pure: the tests check every command against every grant and every relation.
    pub fn allows(&self, command: &Command, relation: Relation) -> bool {
        if command.needs_approvals() && !self.approvals {
            return false;
        }
        if !self.scope.covers(command.scope()) {
            return false;
        }
        match relation {
            Relation::NoThread | Relation::Own => true,
            // A child is only ever touched by its parent, and starting one takes Orchestrate.
            Relation::Child => self.scope >= Scope::Orchestrate,
            // Sibling and further reach is reading, and reading the project's other threads is
            // a separate opt-in. Another project is read at Global with `read_project` (which
            // the user's grant has); writes never reach past the caller's own.
            Relation::Sibling => self.read_project && !command.writes(),
            Relation::Elsewhere => self.scope == Scope::Global && self.read_project && !command.writes(),
        }
    }
}

/// How a command's target stands to the caller, judged by the window, which knows the threads
/// and their parents. `NoThread` joins in phase 4, with the window family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Relation {
    /// The caller's own thread, or no thread at all (a view, the window).
    Own,
    /// A thread the caller started.
    Child,
    /// Another thread of the caller's project.
    Sibling,
    /// A thread of another project.
    Elsewhere,
    /// Not about a thread.
    NoThread,
}

/// The part of Po a command talks to; `run_command` splits by it. `Window` joins in phase 4
/// (`window.info`, `po ctl`'s discovery).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Family {
    View,
    Thread,
    Project,
    Control,
    Agent,
    Window,
}

/// A document an agent put on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentView {
    pub id: String,
    pub title: String,
    /// Markdown.
    pub body: String,
}

/// The kind of a form's field: text typed in, or one of a list of choices picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Choice,
}

/// One field of a form: what the agent calls it, what the user sees, and what it starts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub kind: Kind,
    /// A choice's options, in order; empty for text.
    pub options: Vec<String>,
    /// The initial text, or the choice picked to begin with.
    pub value: Option<String>,
}

impl Field {
    /// Reads one field of a `form.show` request.
    fn parse(field: &Value) -> Result<Self, String> {
        let name = field["name"].as_str().unwrap_or_default().trim();
        if name.is_empty() {
            return Err("every field needs a `name`".into());
        }
        let label = field["label"].as_str().unwrap_or(name).trim();
        let value = field["value"].as_str().map(str::trim).filter(|value| !value.is_empty()).map(String::from);
        match field["kind"].as_str().unwrap_or("text") {
            "text" => Ok(Self { name: name.into(), label: label.into(), kind: Kind::Text, options: vec![], value }),
            "choice" => {
                let bad = || format!("the choice `{name}` needs a list of `options`");
                let options: Vec<String> = field["options"]
                    .as_array()
                    .ok_or_else(bad)?
                    .iter()
                    .map(|option| option.as_str().map(str::trim).filter(|option| !option.is_empty()).map(String::from).ok_or_else(bad))
                    .collect::<Result<_, _>>()?;
                if options.is_empty() {
                    return Err(bad());
                }
                if let Some(value) = &value
                    && !options.contains(value)
                {
                    return Err(format!("`{value}` is not one of `{name}`'s options"));
                }
                Ok(Self { name: name.into(), label: label.into(), kind: Kind::Choice, options, value })
            }
            kind => Err(format!("`{kind}` is not a kind of field; `text` or `choice`")),
        }
    }
}

/// A form an agent put on screen: fields the user fills in, and a button that answers it.
#[derive(Clone, Debug)]
pub struct AgentForm {
    pub id: String,
    pub title: String,
    /// Markdown, shown above the fields.
    pub body: Option<String>,
    pub fields: Vec<Field>,
    pub submit: String,
    /// What the user answered, once they have.
    pub answered: Option<Value>,
}

/// The most fields a form may have, and the most options a choice may offer.
pub const MAX_FIELDS: usize = 12;
pub const MAX_OPTIONS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Adds a markdown view, or replaces the caller's view with the same `id`.
    ViewShow { id: Option<String>, title: String, body: String },
    ViewUpdate { id: String, title: Option<String>, body: Option<String> },
    ViewClose { id: String },
    /// Adds a form — fields and a submit button the user answers at their pace — or replaces
    /// the caller's form with the same `id`.
    FormShow { id: Option<String>, title: String, body: Option<String>, fields: Vec<Field>, submit: Option<String> },
    /// Waits for the user to submit one of the caller's forms, or for the timeout.
    FormWait { id: String, timeout: Option<u64> },
    /// Opens a file of the project in the viewer, at a line (from 1) if one is given.
    FileOpen { path: String, line: Option<usize> },
    /// Shows the changes in the viewer, at one file's diff if `path` is given.
    DiffShow { path: Option<String> },
    /// Closes the viewer, leaving the conversation the middle.
    ViewerClose,

    /// Who the caller is to this window: its grant and its thread.
    ControlInfo,
    /// The agents Po can run.
    AgentList,
    /// The projects open in the window. Another project's needs Global.
    ProjectList,
    /// An issue or epic command, with the request it came in; [`crate::issues::api`] reads it.
    Issues { op: crate::issues::api::Op, args: Value },
    /// The changed files of a project, or one file's diff with `path`.
    ProjectChanges { project: Option<u64>, path: Option<String> },
    /// The threads a caller may read, and where each stands.
    ThreadList { project: Option<u64>, state: Option<String> },
    /// One thread, in full.
    ThreadGet { thread: Option<u64> },
    /// A thread's settings, as its row shows them.
    ThreadOptions { thread: Option<u64> },
    /// A thread's transcript, from a cursor.
    ThreadMessages { thread: Option<u64>, cursor: Option<String>, limit: Option<usize>, full: Option<bool> },
    /// What a thread's agent last said, with its state.
    ThreadLastReply { thread: Option<u64> },

    /// Starts a thread of the caller's own, with `prompt` as its first message. It is parked,
    /// not shown; its grant is Viewer; it answers to the caller alone.
    ThreadCreate { prompt: String, agent: Option<String>, options: Vec<(String, String)>, notify: Option<String> },
    /// Sends `text` to a thread the caller started. `later` puts it after the turn instead of
    /// into it, where the agent takes messages mid-turn.
    ThreadSend { thread: u64, text: String, later: Option<bool> },
    /// Stops a thread the caller started. Text of its own that was still queued comes back.
    ThreadCancel { thread: u64 },
    /// Asks a thread to change one of its settings. On itself, a Viewer may do this; on a
    /// thread it started, an orchestrator.
    ThreadSetOption { thread: Option<u64>, id: String, value: String },
    /// Ends a thread the caller started.
    ThreadClose { thread: u64 },
    /// Waits for one or more of the caller's threads to reach a state, or for the timeout.
    ThreadWait { threads: Option<Vec<u64>>, until: Option<String>, timeout: Option<u64> },

    /// Opens the folder at `path` as a project, in the background. One already open is returned
    /// as it is; a new one is asked of the user first.
    /// With `pr`, a PR (number, branch or URL, as `gh pr view` takes it) checked out for review.
    ProjectOpen { path: Option<String>, pr: Option<String> },
    /// Closes a project the API opened, once the user has no thread of their own in it.
    ProjectClose { project: u64 },
    /// Suggests a project to the user, who clicks to go there. Never switches the screen.
    ProjectOffer { project: u64 },
    /// Like `thread.create`, in a git worktree of the caller's project, shown as a project of
    /// its own. The worktree is made off the UI thread: the reply says it is being made, and
    /// the caller is told when the thread exists.
    ThreadCreateIsolated { prompt: String, agent: Option<String>, options: Vec<(String, String)>, notify: Option<String> },
    /// The thread with this saved session, which survives a restart where the id does not.
    ThreadFind { session: String },
    /// Answers a permission ask of a thread the caller started, with one of its options.
    /// Asks for the user's approval of an agent command or an MCP server are not reachable.
    ThreadAnswer { thread: u64, option: String },
    /// This window: its process, projects and what is on screen. For `po ctl`, which picks the
    /// window to talk to with it.
    WindowInfo,
    /// What the window was asked lately, by whom, and how it went; the newest `limit` (50 by
    /// default). For the user, to see what agents have been doing through Po.
    AuditList { limit: Option<usize> },
    /// Streams a line for every change of a thread's state, until the caller hangs up. Only
    /// the user's connection can; the first reply says it has begun.
    EventsSubscribe,
    /// Asks the user to merge a worktree project into the checkout it came from. Discarding
    /// one is never the API's.
    WorktreeMerge { project: u64 },
}

/// A command's answer: a JSON value, or why it was refused.
pub type Reply = Result<Value, String>;

/// What the window says to a request: the answer now, or that it will send one later. A wait
/// answers later: the pump holds the reply and a waiter sends it when the wait is over.
pub enum Answer {
    Now(Reply),
    /// A wait on `threads` (empty: everything the caller may wait on) until one reaches a
    /// state in `until`, or by `deadline`.
    Later(Wait),
}

/// A wait, as the window's pump holds it.
pub struct Wait {
    pub threads: Vec<u64>,
    pub until: Vec<&'static str>,
    /// When to stop waiting; the timeout is capped by [`Command::timeout`].
    pub deadline: std::time::Instant,
    /// The form to wait for, for a `form.wait`.
    pub form: Option<String>,
}

/// The most views a thread may keep, and the longest body one may have.
pub const MAX_VIEWS: usize = 16;
pub const MAX_BODY: usize = 256 * 1024;
/// The longest title, which has to fit a tab.
pub const MAX_TITLE: usize = 120;

fn text<'a>(request: &'a Value, key: &str) -> Option<&'a str> {
    request[key].as_str()
}

/// A line number from 1, as a number or a string holding one.
fn line(request: &Value) -> Result<Option<usize>, String> {
    let line = match &request["line"] {
        Value::Null => return Ok(None),
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    };
    match line {
        Some(line) if line >= 1 => Ok(Some(line as usize)),
        _ => Err("`line` is a number from 1".into()),
    }
}

/// `path` as a path inside the project at `root`, with `/` between its parts. An absolute path
/// is taken as one under `root`; anything that would reach outside it is refused.
pub fn project_path(root: &std::path::Path, path: &str) -> Result<String, String> {
    use std::path::{Component, Path};
    let full = Path::new(path);
    let relative = if full.is_absolute() { full.strip_prefix(root).map_err(|_| format!("`{path}` is not in the project"))? } else { full };
    let mut parts = vec![];
    for part in relative.components() {
        match part {
            Component::Normal(part) => parts.push(part.to_string_lossy()),
            Component::CurDir => {}
            _ => return Err(format!("`{path}` is not in the project")),
        }
    }
    if parts.is_empty() {
        return Err("no path".into());
    }
    Ok(parts.join("/"))
}

impl Command {
    /// Whether the command moves what the viewer shows, which is the user's to decide while
    /// they are busy, rather than only changing the caller's own views.
    pub fn moves_viewer(&self) -> bool {
        matches!(self, Self::FileOpen { .. } | Self::DiffShow { .. } | Self::ViewerClose)
    }

    /// The name the request carries, as it came in.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ViewShow { .. } => "view.show",
            Self::ViewUpdate { .. } => "view.update",
            Self::ViewClose { .. } => "view.close",
            Self::FormShow { .. } => "form.show",
            Self::FormWait { .. } => "form.wait",
            Self::FileOpen { .. } => "file.open",
            Self::DiffShow { .. } => "diff.show",
            Self::ViewerClose => "viewer.close",
            Self::ControlInfo => "control.info",
            Self::AgentList => "agent.list",
            Self::ProjectList => "project.list",
            Self::Issues { op, .. } => op.name(),
            Self::ProjectChanges { .. } => "project.changes",
            Self::ThreadList { .. } => "thread.list",
            Self::ThreadGet { .. } => "thread.get",
            Self::ThreadOptions { .. } => "thread.options",
            Self::ThreadMessages { .. } => "thread.messages",
            Self::ThreadLastReply { .. } => "thread.last_reply",
            Self::ThreadCreate { .. } => "thread.create",
            Self::ThreadSend { .. } => "thread.send",
            Self::ThreadCancel { .. } => "thread.cancel",
            Self::ThreadSetOption { .. } => "thread.set_option",
            Self::ThreadClose { .. } => "thread.close",
            Self::ThreadWait { .. } => "thread.wait",
            Self::ProjectOpen { .. } => "project.open",
            Self::ProjectClose { .. } => "project.close",
            Self::ProjectOffer { .. } => "project.offer",
            Self::ThreadCreateIsolated { .. } => "thread.create_isolated",
            Self::WorktreeMerge { .. } => "worktree.merge",
            Self::ThreadFind { .. } => "thread.find",
            Self::ThreadAnswer { .. } => "thread.answer",
            Self::WindowInfo => "window.info",
            Self::AuditList { .. } => "audit.list",
            Self::EventsSubscribe => "events.subscribe",
        }
    }

    pub fn family(&self) -> Family {
        match self {
            Self::ViewShow { .. } | Self::ViewUpdate { .. } | Self::ViewClose { .. } | Self::FormShow { .. } | Self::FileOpen { .. } | Self::DiffShow { .. } | Self::ViewerClose => Family::View,
            // A form wait answers later, like a thread wait, so it is pumped with those.
            Self::FormWait { .. } => Family::Thread,
            Self::ControlInfo => Family::Control,
            Self::WindowInfo | Self::AuditList { .. } | Self::EventsSubscribe => Family::Window,
            Self::AgentList => Family::Agent,
            Self::ProjectList | Self::Issues { .. } | Self::ProjectChanges { .. } | Self::ProjectOpen { .. } | Self::ProjectClose { .. } | Self::ProjectOffer { .. } | Self::WorktreeMerge { .. } => Family::Project,
            Self::ThreadList { .. }
            | Self::ThreadGet { .. }
            | Self::ThreadOptions { .. }
            | Self::ThreadMessages { .. }
            | Self::ThreadLastReply { .. }
            | Self::ThreadCreate { .. }
            | Self::ThreadCreateIsolated { .. }
            | Self::ThreadFind { .. }
            | Self::ThreadAnswer { .. }
            | Self::ThreadSend { .. }
            | Self::ThreadCancel { .. }
            | Self::ThreadSetOption { .. }
            | Self::ThreadClose { .. }
            | Self::ThreadWait { .. } => Family::Thread,
        }
    }

    /// The smallest grant that may run this command at all, before reach is judged.
    pub fn scope(&self) -> Scope {
        match self {
            Self::ViewShow { .. } | Self::ViewUpdate { .. } | Self::ViewClose { .. } | Self::FormShow { .. } | Self::FormWait { .. } | Self::FileOpen { .. } | Self::DiffShow { .. } | Self::ViewerClose => Scope::Viewer,
            Self::ControlInfo | Self::AgentList | Self::ProjectList | Self::ProjectChanges { .. } | Self::ThreadList { .. } | Self::ThreadGet { .. } | Self::ThreadOptions { .. } | Self::ThreadMessages { .. } | Self::ThreadLastReply { .. } => Scope::Viewer,
            // Setting a thread's own option is a viewer's; the reach table refuses a child of
            // anyone below Orchestrate.
            Self::ThreadSetOption { .. } => Scope::Viewer,
            Self::ThreadCreate { .. } | Self::ThreadSend { .. } | Self::ThreadCancel { .. } | Self::ThreadClose { .. } | Self::ThreadWait { .. } | Self::ThreadAnswer { .. } => Scope::Orchestrate,
            Self::ThreadFind { .. } => Scope::Viewer,
            // Reading the issues is a viewer's; changing them takes the orchestrator's reach.
            // Starting a worktree thread is the reach `thread.create_isolated` has.
            Self::Issues { op, .. } => {
                if op.starts_thread() {
                    Scope::Global
                } else if op.writes() {
                    Scope::Orchestrate
                } else {
                    Scope::Viewer
                }
            }
            Self::WindowInfo | Self::AuditList { .. } | Self::EventsSubscribe => Scope::Global,
            Self::ProjectOpen { .. } | Self::ProjectClose { .. } | Self::ProjectOffer { .. } | Self::ThreadCreateIsolated { .. } | Self::WorktreeMerge { .. } => Scope::Global,
        }
    }

    /// The thread this command names, if it names one; the commands that don't act on the
    /// caller's own thread, or on the window when the caller is the user.
    pub fn target(&self) -> Option<u64> {
        match self {
            Self::ThreadGet { thread } | Self::ThreadOptions { thread } | Self::ThreadMessages { thread, .. } | Self::ThreadLastReply { thread } | Self::ThreadSetOption { thread, .. } => *thread,
            Self::ThreadSend { thread, .. } | Self::ThreadCancel { thread } | Self::ThreadClose { thread } | Self::ThreadAnswer { thread, .. } => Some(*thread),
            _ => None,
        }
    }

    /// Whether the command answers a thread's permission asks, rather than doing anything else.
    pub fn needs_approvals(&self) -> bool {
        matches!(self, Self::ThreadAnswer { .. })
    }

    /// Whether only the user may run it, from `po ctl`: what a thread has no business asking.
    pub fn user_only(&self) -> bool {
        matches!(self, Self::WindowInfo | Self::AuditList { .. } | Self::EventsSubscribe)
    }

    /// Whether it acts as a parent (starts, steers or waits on a thread of the caller's own),
    /// which the user's connection, with no thread of its own, cannot.
    pub fn needs_parent(&self) -> bool {
        matches!(self, Self::ThreadCreate { .. } | Self::ThreadCreateIsolated { .. } | Self::ThreadSend { .. } | Self::ThreadWait { .. }) || matches!(self, Self::Issues { op, .. } if op.starts_thread())
    }

    /// Whether running it changes something, as opposed to only reading. Reading another
    /// thread's state needs `read_project`; writing it is refused at any grant.
    pub fn writes(&self) -> bool {
        match self {
            // Putting something on screen changes what the caller's views are and may move the
            // viewer; the reads don't.
            Self::ViewShow { .. } | Self::ViewUpdate { .. } | Self::ViewClose { .. } | Self::FormShow { .. } | Self::FileOpen { .. } | Self::DiffShow { .. } | Self::ViewerClose => true,
            Self::ThreadCreate { .. } | Self::ThreadSend { .. } | Self::ThreadCancel { .. } | Self::ThreadSetOption { .. } | Self::ThreadClose { .. } | Self::ThreadAnswer { .. } => true,
            Self::ProjectOpen { .. } | Self::ProjectClose { .. } | Self::ProjectOffer { .. } | Self::ThreadCreateIsolated { .. } | Self::WorktreeMerge { .. } => true,
            Self::Issues { op, .. } => op.writes(),
            Self::ControlInfo
            | Self::AgentList
            | Self::ProjectList
            | Self::ProjectChanges { .. }
            | Self::ThreadList { .. }
            | Self::ThreadGet { .. }
            | Self::ThreadOptions { .. }
            | Self::ThreadMessages { .. }
            | Self::ThreadLastReply { .. }
            | Self::ThreadFind { .. }
            | Self::WindowInfo
            | Self::AuditList { .. }
            | Self::EventsSubscribe
            | Self::FormWait { .. }
            | Self::ThreadWait { .. } => false,
        }
    }

    /// How long the window may take to answer. A wait is the one command that answers later,
    /// not slower: its timeout is capped here, under a client tool limit of 60 s.
    pub fn timeout(&self) -> std::time::Duration {
        const ANSWER: std::time::Duration = std::time::Duration::from_secs(10);
        match self {
            Self::ThreadWait { timeout, .. } | Self::FormWait { timeout, .. } => std::time::Duration::from_secs(timeout.unwrap_or(25).min(50)),
            _ => ANSWER,
        }
    }

    /// Reads a request such as `{"command": "view.show", "title": "Plan", "body": "…"}`.
    pub fn parse(request: &Value) -> Result<Self, String> {
        let name = text(request, "command").ok_or("no command")?;
        let need = |key: &str| text(request, key).map(String::from).ok_or_else(|| format!("{name} needs `{key}`"));
        let opt = |key: &str| text(request, key).map(String::from);
        // A number, as a number or a string holding one.
        let number = |key: &str| -> Result<Option<u64>, String> {
            match &request[key] {
                Value::Null => Ok(None),
                Value::Number(n) => Ok(n.as_u64()),
                Value::String(text) => text.parse().map(Some).map_err(|_| format!("`{key}` is a number")),
                _ => Err(format!("`{key}` is a number")),
            }
        };
        let command = match name {
            "view.show" => Self::ViewShow { id: opt("id"), title: need("title")?, body: need("body")? },
            "view.update" => {
                if opt("title").is_none() && opt("body").is_none() {
                    return Err("view.update needs `title` or `body`".into());
                }
                Self::ViewUpdate { id: need("id")?, title: opt("title"), body: opt("body") }
            }
            "view.close" => Self::ViewClose { id: need("id")? },
            "form.show" => {
                let fields = match &request["fields"] {
                    Value::Null => vec![],
                    Value::Array(fields) => fields.iter().map(Field::parse).collect::<Result<Vec<_>, _>>()?,
                    _ => return Err("`fields` is a list of fields".into()),
                };
                Self::FormShow { id: opt("id"), title: need("title")?, body: opt("body"), fields, submit: opt("submit") }
            }
            "form.wait" => Self::FormWait { id: need("id")?, timeout: number("timeout")? },
            "file.open" => Self::FileOpen { path: need("path")?, line: line(request)? },
            "diff.show" => Self::DiffShow { path: opt("path") },
            "viewer.close" => Self::ViewerClose,
            "control.info" => Self::ControlInfo,
            "agent.list" => Self::AgentList,
            "project.list" => Self::ProjectList,
            issue if crate::issues::api::Op::from_name(issue).is_some() => Self::Issues { op: crate::issues::api::Op::from_name(issue).expect("checked"), args: request.clone() },
            "project.changes" => Self::ProjectChanges { project: number("project")?, path: opt("path") },
            "thread.list" => Self::ThreadList { project: number("project")?, state: opt("state") },
            "thread.get" => Self::ThreadGet { thread: number("thread")? },
            "thread.options" => Self::ThreadOptions { thread: number("thread")? },
            "thread.messages" => Self::ThreadMessages { thread: number("thread")?, cursor: opt("cursor"), limit: number("limit")?.map(|n| n as usize), full: request["full"].as_bool() },
            "thread.last_reply" => Self::ThreadLastReply { thread: number("thread")? },
            "thread.create" | "thread.create_isolated" => {
                let options = request["options"]
                    .as_object()
                    .map(|options| options.iter().filter_map(|(id, value)| value.as_str().map(|value| (id.clone(), value.to_string()))).collect::<Vec<_>>())
                    .unwrap_or_default();
                let (prompt, agent, notify) = (need("prompt")?, opt("agent"), opt("notify"));
                if name == "thread.create" {
                    Self::ThreadCreate { prompt, agent, options, notify }
                } else {
                    Self::ThreadCreateIsolated { prompt, agent, options, notify }
                }
            }
            "thread.send" => Self::ThreadSend { thread: number("thread")?.ok_or("thread.send needs `thread`")?, text: need("text")?, later: request["later"].as_bool() },
            "thread.cancel" => Self::ThreadCancel { thread: number("thread")?.ok_or("thread.cancel needs `thread`")? },
            "thread.set_option" => Self::ThreadSetOption { thread: number("thread")?, id: need("id")?, value: need("value")? },
            "thread.close" => Self::ThreadClose { thread: number("thread")?.ok_or("thread.close needs `thread`")? },
            "thread.wait" => Self::ThreadWait {
                threads: request["threads"].as_array().map(|threads| threads.iter().filter_map(|thread| thread.as_u64()).collect()),
                until: opt("until"),
                timeout: number("timeout")?,
            },
            "project.open" => match (opt("path"), opt("pr")) {
                (Some(path), None) => Self::ProjectOpen { path: Some(path), pr: None },
                (None, Some(pr)) => Self::ProjectOpen { path: None, pr: Some(pr) },
                _ => return Err("project.open needs `path` or `pr`, one of them".into()),
            },
            "thread.find" => Self::ThreadFind { session: need("session")? },
            "thread.answer" => Self::ThreadAnswer { thread: number("thread")?.ok_or("thread.answer needs `thread`")?, option: need("option")? },
            "window.info" => Self::WindowInfo,
            "events.subscribe" => Self::EventsSubscribe,
            "audit.list" => Self::AuditList { limit: number("limit")?.map(|n| n as usize) },
            "worktree.merge" => Self::WorktreeMerge { project: number("project")?.ok_or("worktree.merge needs `project`")? },
            "project.close" => Self::ProjectClose { project: number("project")?.ok_or("project.close needs `project`")? },
            "project.offer" => Self::ProjectOffer { project: number("project")?.ok_or("project.offer needs `project`")? },
            other => return Err(format!("no command `{other}`")),
        };
        command.check()?;
        Ok(command)
    }

    fn check(&self) -> Result<(), String> {
        if let Self::ThreadList { state: Some(state), .. } = self
            && crate::state::ThreadState::from_name(state).is_none()
        {
            return Err(format!("`{state}` is not a thread state"));
        }
        if let Self::ThreadCreate { notify: Some(notify), .. } | Self::ThreadCreateIsolated { notify: Some(notify), .. } = self
            && !matches!(notify.as_str(), "done" | "all" | "never")
        {
            return Err(format!("`{notify}` is not a way to be notified; `done`, `all` or `never`"));
        }
        if let Self::ThreadWait { until: Some(until), .. } = self
            && until != "any"
            && until != "done"
            && crate::state::ThreadState::from_name(until).is_none()
        {
            return Err(format!("`{until}` is not a state to wait for; a state name, `done` or `any`"));
        }
        let (title, body) = match self {
            Self::ViewShow { title, body, .. } => (Some(title), Some(body)),
            Self::ViewUpdate { title, body, .. } => (title.as_ref(), body.as_ref()),
            Self::FormShow { title, body, .. } => (Some(title), body.as_ref()),
            _ => (None, None),
        };
        if title.is_some_and(|title| title.chars().count() > MAX_TITLE) {
            return Err(format!("a title is at most {MAX_TITLE} characters"));
        }
        if body.is_some_and(|body| body.len() > MAX_BODY) {
            return Err(format!("a view is at most {} KiB", MAX_BODY / 1024));
        }
        if let Self::FormShow { fields, submit, .. } = self {
            if fields.len() > MAX_FIELDS {
                return Err(format!("a form has at most {MAX_FIELDS} fields"));
            }
            let mut names: Vec<&str> = vec![];
            for field in fields {
                if names.contains(&field.name.as_str()) {
                    return Err(format!("two fields are named `{}`", field.name));
                }
                names.push(&field.name);
                if field.options.len() > MAX_OPTIONS {
                    return Err(format!("`{}` offers at most {MAX_OPTIONS} options", field.name));
                }
            }
            if submit.as_ref().is_some_and(|submit| submit.chars().count() > MAX_TITLE) {
                return Err(format!("a submit label is at most {MAX_TITLE} characters"));
            }
        }
        Ok(())
    }
}

/// The views of one thread. Agents have no handle on anyone else's.
#[derive(Default)]
pub struct Views {
    list: Vec<AgentView>,
    next: u64,
}

impl Views {
    pub fn list(&self) -> &[AgentView] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Applies a view command. What the viewer shows is the window's, not the views'.
    pub fn apply(&mut self, command: Command) -> Reply {
        match command {
            Command::ViewShow { id, title, body } => {
                let id = id.unwrap_or_else(|| {
                    self.next += 1;
                    format!("v{}", self.next)
                });
                match self.list.iter_mut().find(|view| view.id == id) {
                    Some(view) => {
                        view.title = title;
                        view.body = body;
                    }
                    None => {
                        if self.list.len() >= MAX_VIEWS {
                            return Err(format!("a thread keeps at most {MAX_VIEWS} views; close one first"));
                        }
                        self.list.push(AgentView { id: id.clone(), title, body });
                    }
                }
                Ok(json!({ "id": id }))
            }
            Command::ViewUpdate { id, title, body } => {
                let view = self.list.iter_mut().find(|view| view.id == id).ok_or_else(|| format!("no view `{id}`"))?;
                if let Some(title) = title {
                    view.title = title;
                }
                if let Some(body) = body {
                    view.body = body;
                }
                Ok(json!({ "id": id }))
            }
            Command::ViewClose { id } => {
                let before = self.list.len();
                self.list.retain(|view| view.id != id);
                if self.list.len() == before { Err(format!("no view `{id}`")) } else { Ok(json!({ "id": id })) }
            }
            _ => Err("not a view command".into()),
        }
    }
}

/// The forms of one thread. Agents have no handle on anyone else's.
#[derive(Default)]
pub struct Forms {
    list: Vec<AgentForm>,
    next: u64,
}

/// The most forms a thread may keep.
pub const MAX_FORMS: usize = 8;

impl Forms {
    pub fn list(&self) -> &[AgentForm] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&AgentForm> {
        self.list.iter().find(|form| form.id == id)
    }

    /// Applies a `form.show`: adds a form, or replaces the caller's with the same `id`.
    pub fn show(&mut self, id: Option<String>, title: String, body: Option<String>, fields: Vec<Field>, submit: Option<String>) -> Reply {
        let id = id.unwrap_or_else(|| {
            self.next += 1;
            format!("f{}", self.next)
        });
        let form = AgentForm { id: id.clone(), title, body, fields, submit: submit.filter(|submit| !submit.is_empty()).unwrap_or_else(|| "Submit".into()), answered: None };
        match self.list.iter_mut().find(|form| form.id == id) {
            Some(at) => *at = form,
            None => {
                if self.list.len() >= MAX_FORMS {
                    return Err(format!("a thread keeps at most {MAX_FORMS} forms; close one first"));
                }
                self.list.push(form);
            }
        }
        Ok(json!({ "id": id }))
    }

    /// Takes `id` out; true if it was there.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|form| form.id != id);
        self.list.len() != before
    }

    /// What the user answered, kept so a wait that starts after the submit still reads it.
    pub fn answered(&mut self, id: &str, values: Value) {
        if let Some(form) = self.list.iter_mut().find(|form| form.id == id) {
            form.answered = Some(values);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(id: Option<&str>, title: &str, body: &str) -> Command {
        Command::ViewShow { id: id.map(String::from), title: title.into(), body: body.into() }
    }

    #[test]
    fn parses_each_command() {
        let request = json!({ "command": "view.show", "title": "Plan", "body": "# Plan" });
        assert_eq!(Command::parse(&request), Ok(show(None, "Plan", "# Plan")));
        let request = json!({ "command": "view.update", "id": "v1", "body": "x" });
        assert_eq!(Command::parse(&request), Ok(Command::ViewUpdate { id: "v1".into(), title: None, body: Some("x".into()) }));
        assert_eq!(Command::parse(&json!({ "command": "view.close", "id": "v1" })), Ok(Command::ViewClose { id: "v1".into() }));
        let open = |line| Ok(Command::FileOpen { path: "src/main.rs".into(), line });
        assert_eq!(Command::parse(&json!({ "command": "file.open", "path": "src/main.rs" })), open(None));
        assert_eq!(Command::parse(&json!({ "command": "file.open", "path": "src/main.rs", "line": "12" })), open(Some(12)));
        assert_eq!(Command::parse(&json!({ "command": "file.open", "path": "src/main.rs", "line": 12 })), open(Some(12)));
        assert_eq!(Command::parse(&json!({ "command": "diff.show" })), Ok(Command::DiffShow { path: None }));
        assert_eq!(Command::parse(&json!({ "command": "diff.show", "path": "a.rs" })), Ok(Command::DiffShow { path: Some("a.rs".into()) }));
        assert_eq!(Command::parse(&json!({ "command": "viewer.close" })), Ok(Command::ViewerClose));
    }

    #[test]
    fn parses_the_read_commands() {
        assert_eq!(Command::parse(&json!({ "command": "control.info" })), Ok(Command::ControlInfo));
        assert_eq!(Command::parse(&json!({ "command": "agent.list" })), Ok(Command::AgentList));
        assert_eq!(Command::parse(&json!({ "command": "project.list" })), Ok(Command::ProjectList));
        // Issue commands carry their request on to be read where the issues are.
        let get = Command::parse(&json!({ "command": "issue.get", "issue": "PO-1" })).unwrap();
        assert_eq!((get.name(), get.family(), get.scope(), get.writes()), ("issue.get", Family::Project, Scope::Viewer, false));
        let update = Command::parse(&json!({ "command": "issue.update", "issue": "PO-1", "status": "done" })).unwrap();
        assert_eq!((update.name(), update.scope(), update.writes()), ("issue.update", Scope::Orchestrate, true));
        // Starting a thread on an issue takes the reach of an isolated thread, and a thread of one's own.
        let work = Command::parse(&json!({ "command": "issue.work", "issue": "PO-1" })).unwrap();
        assert_eq!((work.scope(), work.needs_parent()), (Scope::Global, true));
        let delete = Command::parse(&json!({ "command": "issue.delete", "issue": "PO-1" })).unwrap();
        assert_eq!((delete.scope(), delete.writes()), (Scope::Orchestrate, true));
        assert_eq!(Command::parse(&json!({ "command": "label.list" })).unwrap().scope(), Scope::Viewer);
        let viewer = Grant::default();
        let orchestrator = Grant { scope: Scope::Orchestrate, ..Grant::default() };
        assert!(!orchestrator.allows(&work, Relation::Own) && Grant { scope: Scope::Global, ..Grant::default() }.allows(&work, Relation::Own));
        assert!(viewer.allows(&get, Relation::Own) && !viewer.allows(&update, Relation::Own));
        assert!(orchestrator.allows(&update, Relation::Own) && Grant::user().allows(&update, Relation::NoThread));
        assert!(Command::parse(&json!({ "command": "issue.destroy" })).is_err());
        assert_eq!(Command::parse(&json!({ "command": "project.changes", "path": "a.rs" })), Ok(Command::ProjectChanges { project: None, path: Some("a.rs".into()) }));
        assert_eq!(Command::parse(&json!({ "command": "project.changes", "project": 2 })), Ok(Command::ProjectChanges { project: Some(2), path: None }));
        assert_eq!(Command::parse(&json!({ "command": "thread.get", "thread": 7 })), Ok(Command::ThreadGet { thread: Some(7) }));
        assert_eq!(Command::parse(&json!({ "command": "thread.get", "thread": "7" })), Ok(Command::ThreadGet { thread: Some(7) }));
        assert_eq!(Command::parse(&json!({ "command": "thread.get" })), Ok(Command::ThreadGet { thread: None }));
        assert_eq!(Command::parse(&json!({ "command": "thread.list", "state": "running" })), Ok(Command::ThreadList { project: None, state: Some("running".into()) }));
        assert_eq!(Command::parse(&json!({ "command": "thread.options" })), Ok(Command::ThreadOptions { thread: None }));
        assert_eq!(Command::parse(&json!({ "command": "thread.messages", "cursor": "3:1", "limit": 10, "full": true })), Ok(Command::ThreadMessages { thread: None, cursor: Some("3:1".into()), limit: Some(10), full: Some(true) }));
        assert_eq!(Command::parse(&json!({ "command": "thread.last_reply" })), Ok(Command::ThreadLastReply { thread: None }));
        // A state filter must name a state there is.
        assert!(Command::parse(&json!({ "command": "thread.list", "state": "gone" })).is_err());
        assert!(Command::parse(&json!({ "command": "thread.get", "thread": "seven" })).is_err());
        // What reaches is what the command names.
        assert_eq!(Command::parse(&json!({ "command": "thread.get", "thread": 7 })).unwrap().target(), Some(7));
        assert_eq!(Command::parse(&json!({ "command": "view.close", "id": "v" })).unwrap().target(), None);
    }

    #[test]
    fn parses_the_orchestration_commands() {
        assert_eq!(
            Command::parse(&json!({ "command": "thread.create", "prompt": "go", "agent": "dex", "notify": "all", "options": { "model": "fast" } })),
            Ok(Command::ThreadCreate { prompt: "go".into(), agent: Some("dex".into()), options: vec![("model".into(), "fast".into())], notify: Some("all".into()) })
        );
        // The prompt is required; a promptless thread would be dropped as blank.
        assert!(Command::parse(&json!({ "command": "thread.create" })).is_err());
        assert!(Command::parse(&json!({ "command": "thread.create", "notify": "sometimes" })).is_err());
        assert_eq!(
            Command::parse(&json!({ "command": "thread.send", "thread": 7, "text": "onwards", "later": true })),
            Ok(Command::ThreadSend { thread: 7, text: "onwards".into(), later: Some(true) })
        );
        assert!(Command::parse(&json!({ "command": "thread.send", "text": "x" })).is_err());
        assert_eq!(Command::parse(&json!({ "command": "thread.cancel", "thread": 7 })), Ok(Command::ThreadCancel { thread: 7 }));
        assert_eq!(Command::parse(&json!({ "command": "thread.close", "thread": 7 })), Ok(Command::ThreadClose { thread: 7 }));
        assert_eq!(
            Command::parse(&json!({ "command": "thread.set_option", "id": "model", "value": "opus" })),
            Ok(Command::ThreadSetOption { thread: None, id: "model".into(), value: "opus".into() })
        );
        assert_eq!(
            Command::parse(&json!({ "command": "thread.wait", "threads": [7, 8], "until": "asking", "timeout": 40 })),
            Ok(Command::ThreadWait { threads: Some(vec![7, 8]), until: Some("asking".into()), timeout: Some(40) })
        );
        assert_eq!(Command::parse(&json!({ "command": "thread.wait" })), Ok(Command::ThreadWait { threads: None, until: None, timeout: None }));
        assert!(Command::parse(&json!({ "command": "thread.wait", "until": "gone" })).is_err());
        // A wait of 90 s is capped by the command's own timeout, under a client's 60 s limit.
        assert_eq!(Command::parse(&json!({ "command": "thread.wait", "timeout": 90 })).unwrap().timeout(), std::time::Duration::from_secs(50));
        assert_eq!(Command::parse(&json!({ "command": "thread.wait" })).unwrap().timeout(), std::time::Duration::from_secs(25));
    }

    #[test]
    fn parses_the_project_commands() {
        assert_eq!(Command::parse(&json!({ "command": "project.open", "path": "/work/x" })), Ok(Command::ProjectOpen { path: Some("/work/x".into()), pr: None }));
        assert_eq!(Command::parse(&json!({ "command": "project.open", "pr": "12" })), Ok(Command::ProjectOpen { path: None, pr: Some("12".into()) }));
        assert!(Command::parse(&json!({ "command": "project.open" })).is_err());
        assert!(Command::parse(&json!({ "command": "project.open", "path": "/x", "pr": "1" })).is_err());
        assert_eq!(Command::parse(&json!({ "command": "worktree.merge", "project": 4 })), Ok(Command::WorktreeMerge { project: 4 }));
        let isolated = Command::parse(&json!({ "command": "thread.create_isolated", "prompt": "go" })).unwrap();
        assert_eq!((isolated.family(), isolated.scope()), (Family::Thread, Scope::Global));
        assert!(Command::parse(&json!({ "command": "thread.create_isolated" })).is_err());
        assert_eq!(Command::parse(&json!({ "command": "project.close", "project": "3" })), Ok(Command::ProjectClose { project: 3 }));
        assert_eq!(Command::parse(&json!({ "command": "project.offer", "project": 3 })), Ok(Command::ProjectOffer { project: 3 }));
        assert!(Command::parse(&json!({ "command": "project.offer" })).is_err());
        // Only Global may touch the projects, and none of it reaches a thread.
        let open = Command::ProjectOpen { path: Some("/x".into()), pr: None };
        assert!(Grant { scope: Scope::Global, ..Grant::default() }.allows(&open, Relation::Own));
        assert!(!Grant { scope: Scope::Orchestrate, read_project: true, approvals: true }.allows(&open, Relation::Own));
        assert_eq!(open.family(), Family::Project);
    }

    #[test]
    fn parses_the_phase_four_commands() {
        assert_eq!(Command::parse(&json!({ "command": "thread.find", "session": "abc" })), Ok(Command::ThreadFind { session: "abc".into() }));
        assert!(Command::parse(&json!({ "command": "thread.find" })).is_err());
        let answer = Command::parse(&json!({ "command": "thread.answer", "thread": 7, "option": "allow" })).unwrap();
        assert_eq!(answer, Command::ThreadAnswer { thread: 7, option: "allow".into() });
        // Answering needs `approvals` on top of Orchestrate, and only reaches a child.
        let orchestrate = Grant { scope: Scope::Orchestrate, ..Grant::default() };
        assert!(!orchestrate.allows(&answer, Relation::Child));
        assert!(Grant { approvals: true, ..orchestrate }.allows(&answer, Relation::Child));
        assert!(!Grant { approvals: true, read_project: true, ..orchestrate }.allows(&answer, Relation::Sibling));
        assert!(Grant::user().allows(&answer, Relation::Own));
        let audit = Command::parse(&json!({ "command": "audit.list", "limit": 5 })).unwrap();
        assert_eq!(audit, Command::AuditList { limit: Some(5) });
        assert!(audit.user_only() && !audit.writes());
        let events = Command::parse(&json!({ "command": "events.subscribe" })).unwrap();
        assert!(events.user_only() && !events.writes());
        let info = Command::parse(&json!({ "command": "window.info" })).unwrap();
        assert!(info.user_only() && info.family() == Family::Window);
        assert!(!answer.user_only() && answer.target() == Some(7));
    }

    #[test]
    fn refuses_what_it_cannot_read() {        assert!(Command::parse(&json!({})).is_err());
        assert!(Command::parse(&json!({ "command": "rm" })).is_err());
        assert!(Command::parse(&json!({ "command": "view.show", "title": "t" })).is_err());
        assert!(Command::parse(&json!({ "command": "view.update", "id": "v1" })).is_err());
        assert!(Command::parse(&json!({ "command": "layout.preset", "name": "review" })).is_err());
        assert!(Command::parse(&json!({ "command": "file.open" })).is_err());
        for line in [json!(0), json!("0"), json!("ten"), json!(-3), json!(true)] {
            assert!(Command::parse(&json!({ "command": "file.open", "path": "a", "line": line })).is_err());
        }
        let huge = "x".repeat(MAX_BODY + 1);
        assert!(Command::parse(&json!({ "command": "view.show", "title": "t", "body": huge })).is_err());
    }

    #[test]
    fn a_path_stays_inside_the_project() {
        let root = std::path::Path::new("/work/po");
        assert_eq!(project_path(root, "src/main.rs"), Ok("src/main.rs".into()));
        assert_eq!(project_path(root, "./src/./main.rs"), Ok("src/main.rs".into()));
        assert_eq!(project_path(root, "/work/po/src/main.rs"), Ok("src/main.rs".into()));
        assert!(project_path(root, "../secret").is_err());
        assert!(project_path(root, "src/../../secret").is_err());
        assert!(project_path(root, "/etc/passwd").is_err());
        assert!(project_path(root, "/work/po").is_err());
        assert!(project_path(root, "").is_err());
    }

    #[test]
    fn a_title_has_to_fit_a_tab() {
        let long = "t".repeat(MAX_TITLE + 1);
        assert!(Command::parse(&json!({ "command": "view.show", "title": long, "body": "" })).is_err());
        assert!(Command::parse(&json!({ "command": "view.update", "id": "v1", "title": long })).is_err());
        assert!(Command::parse(&json!({ "command": "view.show", "title": "t".repeat(MAX_TITLE), "body": "" })).is_ok());
    }

    #[test]
    fn views_are_added_replaced_updated_and_closed() {
        let mut views = Views::default();
        assert_eq!(views.apply(show(None, "Plan", "a")), Ok(json!({ "id": "v1" })));
        assert_eq!(views.apply(show(Some("v1"), "Plan 2", "b")), Ok(json!({ "id": "v1" })));
        assert_eq!(views.list().len(), 1);
        assert_eq!(views.list()[0].title, "Plan 2");
        views.apply(Command::ViewUpdate { id: "v1".into(), title: None, body: Some("c".into()) }).unwrap();
        assert_eq!(views.list()[0].body, "c");
        assert!(views.apply(Command::ViewUpdate { id: "nope".into(), title: None, body: Some("c".into()) }).is_err());
        views.apply(Command::ViewClose { id: "v1".into() }).unwrap();
        assert!(views.is_empty());
        assert!(views.apply(Command::ViewClose { id: "v1".into() }).is_err());
    }

    #[test]
    fn a_thread_keeps_a_bounded_number_of_views() {
        let mut views = Views::default();
        for n in 0..MAX_VIEWS {
            views.apply(show(None, &format!("v{n}"), "")).unwrap();
        }
        assert!(views.apply(show(None, "one more", "")).is_err());
        // Replacing one is not adding one.
        assert!(views.apply(show(Some("v1"), "again", "")).is_ok());
    }

    #[test]
    fn forms_are_read_and_kept() {
        let form = |fields| json!({ "command": "form.show", "title": "Ship it", "fields": fields });
        let parsed = Command::parse(&form(json!([
            { "name": "name", "label": "Your name", "value": "Ada" },
            { "name": "size", "kind": "choice", "options": ["small", "large"], "value": "small" },
        ])));
        assert_eq!(
            parsed,
            Ok(Command::FormShow {
                id: None,
                title: "Ship it".into(),
                body: None,
                submit: None,
                fields: vec![
                    Field { name: "name".into(), label: "Your name".into(), kind: Kind::Text, options: vec![], value: Some("Ada".into()) },
                    Field { name: "size".into(), label: "size".into(), kind: Kind::Choice, options: vec!["small".into(), "large".into()], value: Some("small".into()) },
                ],
            })
        );
        // What a form may not be.
        assert!(Command::parse(&form(json!([{}]))).is_err(), "a field needs a name");
        assert!(Command::parse(&form(json!([{ "name": "a", "kind": "magic" }]))).is_err());
        assert!(Command::parse(&form(json!([{ "name": "a", "kind": "choice" }]))).is_err(), "a choice needs options");
        assert!(Command::parse(&form(json!([{ "name": "a", "kind": "choice", "options": [""] }]))).is_err());
        assert!(Command::parse(&form(json!([{ "name": "a", "kind": "choice", "options": ["x"], "value": "y" }]))).is_err());
        assert!(Command::parse(&form(json!("nope"))).is_err());
        assert!(Command::parse(&form(json!([{ "name": "a" }, { "name": "a" }]))).is_err(), "names are unique");
        let many: Vec<_> = (0..=MAX_FIELDS).map(|n| json!({ "name": format!("f{n}") })).collect();
        assert!(Command::parse(&form(json!(many))).is_err());
        assert!(Command::parse(&json!({ "command": "form.wait" })).is_err());
        assert_eq!(Command::parse(&json!({ "command": "form.wait", "id": "f1", "timeout": 90 })).unwrap().timeout(), std::time::Duration::from_secs(50));
        let show = Command::parse(&form(json!([]))).unwrap();
        assert_eq!((show.name(), show.scope(), show.writes()), ("form.show", Scope::Viewer, true));
        assert_eq!(show.family(), Family::View);
    }

    #[test]
    fn forms_are_shown_replaced_answered_and_removed() {
        let mut forms = Forms::default();
        let field = Field { name: "a".into(), label: "A".into(), kind: Kind::Text, options: vec![], value: None };
        assert_eq!(forms.show(None, "One".into(), None, vec![field.clone()], None), Ok(json!({ "id": "f1" })));
        assert_eq!(forms.show(Some("f1".into()), "Two".into(), None, vec![], None), Ok(json!({ "id": "f1" })));
        assert_eq!(forms.list().len(), 1);
        assert_eq!(forms.list()[0].title, "Two");
        assert!(forms.get("f1").is_some() && forms.get("nope").is_none());
        // The answer of a form is kept for a wait that starts later.
        forms.answered("f1", json!({ "a": "b" }));
        assert_eq!(forms.get("f1").unwrap().answered.as_ref(), Some(&json!({ "a": "b" })));
        assert!(forms.remove("f1"));
        assert!(forms.is_empty() && !forms.remove("f1"));
        for n in 0..MAX_FORMS {
            forms.show(None, format!("f{n}"), None, vec![], None).unwrap();
        }
        assert!(forms.show(None, "one more".into(), None, vec![], None).is_err());
        // The ids handed out so far are f2..f9: the first show spent f1, the replace spent none.
        assert!(forms.show(Some("f2".into()), "again".into(), None, vec![], None).is_ok(), "replacing one is not adding one");
    }

    // Every command against every grant and every target relation: the reach table of §4.2 in
    // the control plan, so a new command can't quietly widen what a grant allows.
    #[test]
    fn grants_allow_commands_only_within_their_reach() {
        use super::Relation::{Child, Elsewhere, NoThread, Own, Sibling};
        let viewer = Grant::default();
        let orchestrate = Grant { scope: Scope::Orchestrate, ..Grant::default() };
        let orchestrate_read = Grant { read_project: true, ..orchestrate };
        let global = Grant { scope: Scope::Global, ..orchestrate_read };
        let user = Grant::user();
        let relations = [Own, Child, Sibling, Elsewhere, NoThread];
        for command in [
            Command::ViewShow { id: None, title: "t".into(), body: String::new() },
            Command::ViewUpdate { id: "v".into(), title: None, body: Some(String::new()) },
            Command::ViewClose { id: "v".into() },
            Command::FormShow { id: None, title: "t".into(), body: None, fields: vec![], submit: None },
            Command::FileOpen { path: "a.rs".into(), line: None },
            Command::DiffShow { path: None },
            Command::ViewerClose,
        ] {
            for (grant, name) in [(&viewer, "viewer"), (&orchestrate, "orchestrate"), (&orchestrate_read, "orchestrate+read"), (&global, "global"), (&user, "user")] {
                for relation in relations {
                    let allowed = grant.allows(&command, relation);
                    // What every grant says, whatever the command: its own thread and the
                    // window are its own; a child needs Orchestrate; a sibling or another
                    // project's thread is read-only and needs `read_project` (sibling) or
                    // Global (elsewhere). Every command there is today reaches only itself.
                    let expected = match relation {
                        Own | NoThread => grant.scope.covers(command.scope()),
                        Child => grant.scope >= Scope::Orchestrate,
                        Sibling => grant.read_project && !command.writes(),
                        Elsewhere => grant.scope == Scope::Global && grant.read_project && !command.writes(),
                    };
                    assert_eq!(allowed, expected, "{name} {:?} {relation:?}", command.name());
                }
            }
        }
        // The user's grant reaches everything the API may ever do.
        for command in [Command::ViewerClose, Command::ViewShow { id: None, title: "t".into(), body: String::new() }] {
            assert!(user.allows(&command, Relation::Own) && user.allows(&command, Relation::Child) && user.allows(&command, Relation::NoThread));
        }
        // Every command there is today acts on the caller's own thread, so its grant's reach
        // beyond that is never exercised; the window's authorize judges the relation, and
        // target None always means Own.
        assert!(Command::parse(&json!({ "command": "view.show", "title": "t", "body": "" })).unwrap().target().is_none());
    }
}
