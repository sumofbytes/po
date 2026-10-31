//! What a caller of the control API sees of Po's threads and projects: snapshots, with the
//! state of a thread derived in one place. Nothing here touches GPUI — the window builds these
//! from its own state, and the tests check the derivation without one.
use serde_json::{Value, json};

/// Where a thread is, as an outsider can tell: an orchestrator reads this to know "done" from
/// "crashed" and from "waiting for the user", which a busy flag alone can't say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    /// Its agent process is live but the session isn't open yet (starting, listing sessions,
    /// replaying a resumed session, or taking its first settings).
    Starting,
    /// It wants to run something the user hasn't approved yet: the agent itself, or an MCP
    /// server. Only a click answers it.
    Approval,
    /// The session is open and idle.
    Ready,
    /// A turn is running.
    Running,
    /// It asked a permission question and waits for an answer.
    Asking,
    /// Its last turn ended in an error, or its agent stopped or exited mid-turn.
    Failed,
    /// Its agent is stopped; its session is kept to be resumed.
    Asleep,
    /// It is gone. It is only ever reported, never read: a closed thread has nothing to read.
    Closed,
}

impl ThreadState {
    /// The name callers see, and filter `thread.list` by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Approval => "approval",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Asking => "asking",
            Self::Failed => "failed",
            Self::Asleep => "asleep",
            Self::Closed => "closed",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "starting" => Self::Starting,
            "approval" => Self::Approval,
            "ready" => Self::Ready,
            "running" => Self::Running,
            "asking" => Self::Asking,
            "failed" => Self::Failed,
            "asleep" => Self::Asleep,
            "closed" => Self::Closed,
            _ => return None,
        })
    }
}

/// The bare facts the window reports about one of its threads, from which the state is derived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// An approval card waits for the user (`Block::Approve`, unanswered).
    pub approval: bool,
    /// A permission ask waits for an answer (`Block::Permission`, unanswered).
    pub asking: bool,
    /// Its last turn ended in an error, or its agent stopped or exited mid-turn.
    pub failed: bool,
    /// Its agent is stopped; its session is kept to be resumed.
    pub asleep: bool,
    /// Its agent process is live.
    pub started: bool,
    /// The session is open.
    pub ready: bool,
    /// A turn is running.
    pub busy: bool,
}

/// [`ThreadState`] from [`Facts`]. The order is the read that wins: a question outranks a
/// failure (the user is being asked *now*), a failure outranks a sleep (what happened must not
/// be hidden by the tidy "asleep"), and a live agent that isn't ready is still starting even
/// when nothing else is the matter.
pub fn state(facts: &Facts) -> ThreadState {
    if facts.approval {
        ThreadState::Approval
    } else if facts.asking {
        ThreadState::Asking
    } else if facts.failed {
        ThreadState::Failed
    } else if facts.asleep {
        ThreadState::Asleep
    } else if facts.started && !facts.ready {
        ThreadState::Starting
    } else if facts.busy {
        ThreadState::Running
    } else {
        ThreadState::Ready
    }
}

/// A permission ask of a thread: what it wants and the ways it may be answered. Approval cards
/// are never here: they wait for the user's click, and no API answers them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskInfo {
    pub title: String,
    pub options: Vec<AskOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// One thread, as its state was when the snapshot was taken.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadInfo {
    pub id: u64,
    /// The project it belongs to, by the window's id.
    pub project: u64,
    pub title: String,
    /// The agent's configured id.
    pub agent: String,
    pub state: ThreadState,
    pub unread: bool,
    pub busy: bool,
    /// What the thread says it is doing, as the thread list shows it.
    pub status: String,
    /// Why it failed, the last time it did.
    pub last_error: Option<String>,
    pub session: Option<String>,
    /// The last thing it said or did, in a line.
    pub preview: String,
    /// Messages waiting to be sent when the turn is over.
    pub queued: usize,
    /// Text sent through the API that a stopped turn dropped.
    pub dropped: Vec<String>,
    /// What it is asking, when [`ThreadState::Asking`].
    pub asking: Option<AskInfo>,
    /// The thread that started it through the API, if any.
    pub parent: Option<u64>,
    /// The threads it started, if any.
    pub children: Vec<u64>,
    /// When it last did anything, in milliseconds since the Unix epoch.
    pub active_at: u64,
    /// Whether it is the one on screen.
    pub on_screen: bool,
    /// How much of its context window the session uses, when its agent says.
    pub usage: Option<Usage>,
}

/// Whether `a` and `b` are threads of one project, given each project's thread ids.
pub fn share_project(projects: &[Vec<u64>], a: u64, b: u64) -> bool {
    projects.iter().any(|threads| threads.contains(&a) && threads.contains(&b))
}

/// A session's context use, as an agent's `usage_update` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub used: u64,
    pub size: u64,
}

impl Usage {
    /// The usage a `session/update` payload reports, if it is a `usage_update`.
    pub fn from_update(update: &Value) -> Option<Self> {
        if update["sessionUpdate"] != "usage_update" {
            return None;
        }
        Some(Self { used: update["used"].as_u64()?, size: update["size"].as_u64()? })
    }
}

impl ThreadInfo {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "project": self.project,
            "title": self.title,
            "agent": self.agent,
            "state": self.state.name(),
            "unread": self.unread,
            "busy": self.busy,
            "status": self.status,
            "last_error": self.last_error,
            "session": self.session,
            "preview": self.preview,
            "queued": self.queued,
            "dropped": self.dropped,
            "asking": self.asking.as_ref().map(|ask| json!({
                "title": ask.title,
                "options": ask.options.iter().map(|o| json!({ "id": o.id, "name": o.name, "kind": o.kind })).collect::<Vec<_>>(),
            })),
            "parent": self.parent,
            "children": self.children,
            "active_at": self.active_at,
            "on_screen": self.on_screen,
            "usage": self.usage.map(|usage| json!({ "used": usage.used, "size": usage.size })),
        })
    }
}

/// One of a thread's settings, as the thread's own row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionInfo {
    pub id: String,
    pub name: String,
    /// `model`, `mode`, `thought_level`, or empty.
    pub category: String,
    pub current: String,
    /// Each choice's value and name.
    pub choices: Vec<(String, String)>,
}

impl OptionInfo {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "category": self.category,
            "current": self.current,
            "current_name": self.choices.iter().find(|(value, _)| *value == self.current).map_or(self.current.as_str(), |(_, name)| name.as_str()),
            "choices": self.choices.iter().map(|(value, name)| json!({ "value": value, "name": name })).collect::<Vec<_>>(),
        })
    }
}

/// One agent Po can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    /// The user has approved this agent's command (or named it themselves).
    pub trusted: bool,
    /// It is a daemon to connect to, rather than a process per thread.
    pub daemon: bool,
}

impl AgentInfo {
    pub fn json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "trusted": self.trusted, "daemon": self.daemon })
    }
}

/// One project (or worktree) open in the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectInfo {
    pub id: u64,
    pub root: String,
    pub name: String,
    /// The branch checked out, when there is one to say.
    pub branch: Option<String>,
    /// Tracked files have changes against the base.
    pub dirty: bool,
    /// The PR being reviewed, if this is one.
    pub pr: Option<u64>,
    /// The project this is a worktree of, if it is one.
    pub worktree_of: Option<u64>,
    pub on_screen: bool,
    pub threads: usize,
}

impl ProjectInfo {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "root": self.root,
            "name": self.name,
            "branch": self.branch,
            "dirty": self.dirty,
            "pr": self.pr,
            "worktree_of": self.worktree_of,
            "on_screen": self.on_screen,
            "threads": self.threads,
        })
    }
}

/// One tool call in a transcript, as listed without its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub title: String,
    /// `waiting`, `working`, `done` or `failed`.
    pub status: String,
    /// With `full`, what the agent called it with.
    pub arguments: Option<String>,
    /// With `full`, what the call came back with.
    pub result: Option<String>,
}

/// One message of a transcript, as `thread.messages` returns it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Its place in the whole transcript, from 0. A cursor is `<generation>:<index>`.
    pub index: usize,
    /// `user` or `agent`.
    pub role: &'static str,
    /// What it said: the user's text, or the agent's text and thoughts.
    pub text: String,
    pub tools: Vec<ToolCall>,
    /// The agent's to-do list, if it keeps one.
    pub plan: Option<Vec<(String, String)>>,
}

impl Entry {
    pub fn json(&self) -> Value {
        json!({
            "index": self.index,
            "role": self.role,
            "text": self.text,
            "tools": self.tools.iter().map(|tool| json!({
                "title": tool.title,
                "status": tool.status,
                "arguments": tool.arguments,
                "result": tool.result,
            })).collect::<Vec<_>>(),
            "plan": self.plan.as_ref().map(|plan| plan.iter().map(|(text, state)| json!({ "text": text, "state": state })).collect::<Vec<_>>()),
        })
    }
}

/// The most entries `thread.messages` returns when no limit is given.
pub const DEFAULT_MESSAGES: usize = 200;

/// How much of one file's diff `project.changes` returns before it stops.
pub const MAX_DIFF: usize = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(
        approval: bool,
        asking: bool,
        failed: bool,
        asleep: bool,
        started: bool,
        ready: bool,
        busy: bool,
    ) -> Facts {
        Facts { approval, asking, failed, asleep, started, ready, busy }
    }

    // The derivation order is what an orchestrator needs: a question outranks a failure, a
    // failure outranks a sleep, and a live agent that isn't ready is starting.
    #[test]
    fn the_state_of_a_thread_is_derived_once() {
        let idle = facts(false, false, false, false, true, true, false);
        assert_eq!(state(&idle), ThreadState::Ready);
        assert_eq!(state(&Facts { busy: true, ..idle }), ThreadState::Running);
        assert_eq!(state(&Facts { ready: false, ..idle }), ThreadState::Starting);
        // Nothing is running at all (never started): it reads ready, which a caller can tell
        // from a live one by `started`'s facts; only a thread an agent started is ever blank, and it
        // has a client while it waits.
        assert_eq!(state(&facts(false, false, false, false, false, false, false)), ThreadState::Ready);
        // A question outranks the failure that may sit behind it in the transcript.
        assert_eq!(state(&Facts { asking: true, failed: true, ..idle }), ThreadState::Asking);
        // An approval outranks a question: the user has to say yes before anything else.
        assert_eq!(state(&Facts { approval: true, asking: true, failed: true, ..idle }), ThreadState::Approval);
        // A failure outranks a sleep, so a crash is not tidied away as "asleep".
        assert_eq!(state(&Facts { failed: true, asleep: true, ..idle }), ThreadState::Failed);
        assert_eq!(state(&Facts { asleep: true, ..idle }), ThreadState::Asleep);
        // A busy turn with a question open is asking, not running.
        assert_eq!(state(&Facts { asking: true, busy: true, ..idle }), ThreadState::Asking);
    }

    #[test]
    fn siblings_share_a_project_not_just_existence() {
        let projects = vec![vec![1, 2], vec![3]];
        assert!(share_project(&projects, 1, 2));
        // Both exist, in different projects: not siblings, whatever else is true of them.
        assert!(!share_project(&projects, 1, 3));
        assert!(!share_project(&projects, 1, 9));
    }

    #[test]
    fn usage_is_read_from_a_usage_update_only() {
        let update = json!({ "sessionUpdate": "usage_update", "used": 53000, "size": 200000 });
        assert_eq!(Usage::from_update(&update), Some(Usage { used: 53000, size: 200_000 }));
        assert_eq!(Usage::from_update(&json!({ "sessionUpdate": "agent_message_chunk" })), None);
        // A usage update missing a number is not guessed at.
        assert_eq!(Usage::from_update(&json!({ "sessionUpdate": "usage_update", "used": 1 })), None);
    }

    #[test]
    fn a_state_name_round_trips() {
        for state in [ThreadState::Starting, ThreadState::Approval, ThreadState::Ready, ThreadState::Running, ThreadState::Asking, ThreadState::Failed, ThreadState::Asleep, ThreadState::Closed] {
            assert_eq!(ThreadState::from_name(state.name()), Some(state));
        }
        assert_eq!(ThreadState::from_name("gone"), None);
    }

    #[test]
    fn a_snapshot_says_what_an_outsider_wants_to_know() {
        let info = ThreadInfo {
            id: 7,
            project: 1,
            title: "fix flaky test".into(),
            agent: "dex".into(),
            state: ThreadState::Asking,
            unread: true,
            busy: true,
            status: "working".into(),
            last_error: None,
            session: Some("s1".into()),
            preview: "Running tests".into(),
            queued: 1,
            dropped: vec!["hello".into()],
            asking: Some(AskInfo { title: "Run tests".into(), options: vec![AskOption { id: "a".into(), name: "Allow once".into(), kind: "allow_once".into() }] }),
            parent: Some(3),
            children: vec![],
            active_at: 1234,
            on_screen: false,
            usage: Some(Usage { used: 1200, size: 200_000 }),
        };
        let json = info.json();
        assert_eq!(json["usage"]["used"], 1200);
        assert_eq!(json["state"], "asking");
        assert_eq!(json["asking"]["options"][0]["kind"], "allow_once");
        assert_eq!(json["dropped"][0], "hello");
        assert_eq!(json["parent"], 3);
    }
}
