//! What the conversation screen is given and what it sends back: the types [`Snapshot`] is made of,
//! the channel [`crate::reader::state::Reader`] publishes it through, and the commands the user's actions become.
//! No view code, so a screen on any toolkit can use it.
use std::sync::{Arc, LazyLock};

use tokio::sync::watch;

use super::header::Control;

/// Whether a step of the agent's is waiting, going, done or failed: a tool call, the plan's
/// entry, a whole run of tool calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Waiting,
    Working,
    Done,
    Failed,
}

/// How far a thread's agent may reach through Po's API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Viewer,
    Orchestrate,
    Global,
}

/// What the thread's agent may do through Po's API: its scope, and the reach it has beyond
/// seeing this project's threads and answering their asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grant {
    pub scope: Scope,
    pub read: bool,
    pub approvals: bool,
}

impl Default for Grant {
    fn default() -> Self {
        Self {
            scope: Scope::Viewer,
            read: false,
            approvals: false,
        }
    }
}

/// One of the thread's session settings, like its model.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Setting {
    /// The id the agent gave it.
    pub id: String,
    /// What it is called, as the agent named it.
    pub name: String,
    /// The choice's value in effect.
    pub current: String,
    /// Each choice, `(value, name)` as the agent gave it.
    pub choices: Vec<(String, String)>,
}

/// One picture attached to the draft, as a data URI so the screen needs no file access. The URI
/// is shared: it is asked for on every frame a picture shows, and it never changes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    pub uri: Arc<str>,
    /// Its pixels, which give the box it is drawn in its shape.
    pub size: (u32, u32),
}

/// What waits to be sent when the turn is over: `kind` says how it got there (nothing is the
/// user's own; "Steer" or "Follow-up" is what the agent's daemon holds for later).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Queued {
    pub at: usize,
    pub kind: String,
    pub text: String,
    /// How many pictures go with it, when there is no text to show.
    pub pictures: usize,
}

impl Setting {
    /// The name of the choice in effect, or its value when the agent gave no name for it.
    pub fn current_name(&self) -> String {
        self.choices
            .iter()
            .find(|(value, _)| *value == self.current)
            .map_or(self.current.clone(), |(_, name)| name.clone())
    }
}

/// A session of the agent's, offered to resume.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Session {
    /// The id it is resumed by.
    pub id: String,
    /// The name the agent gave it, or nothing if it never said.
    pub title: String,
    /// When it was last at work, human-readable.
    pub when: String,
    /// It was closed here lately, so it is listed first.
    pub closed: bool,
}

/// What a tool call does, for the line a folded stretch of work says about itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Read,
    Search,
    Edit,
    Command,
    Fetch,
    Other,
}

impl Kind {
    /// The order the line counts them in.
    pub(crate) const ALL: [Kind; 6] = [
        Kind::Read,
        Kind::Search,
        Kind::Edit,
        Kind::Command,
        Kind::Fetch,
        Kind::Other,
    ];

    /// `3 reads`, `1 search`: the count and what it counts.
    pub(crate) fn counted(self, n: usize) -> String {
        let (one, many) = match self {
            Kind::Read => ("read", "reads"),
            Kind::Search => ("search", "searches"),
            Kind::Edit => ("edit", "edits"),
            Kind::Command => ("command", "commands"),
            Kind::Fetch => ("fetch", "fetches"),
            Kind::Other => ("other", "other"),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// A tool call of the agent's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tool {
    /// Which card of the message it is, to press.
    pub index: usize,
    pub title: String,
    pub phase: Phase,
    /// Whether its call and result are shown; the title's click leaves them.
    pub open: bool,
    /// What the call went out as.
    pub arguments: String,
    /// What it came back with.
    pub result: String,
    /// A file the call wrote or changed, which opens in the other pane.
    pub path: Option<String>,
}

/// One entry of the plan.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub text: String,
    pub phase: Phase,
}

/// One way of answering a permission request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AskOption {
    /// Which of the request's options it is, to press.
    pub at: usize,
    pub label: String,
    /// The first way of saying yes, drawn filled.
    pub primary: bool,
}

/// One block of the agent's message. `index` is the block's place in the message, to press.
#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    /// The agent's text, as markdown. Compared by its text: a piece that did not change must
    /// compare equal to the one drawn, or it is drawn again with each chunk of another. `stitch`
    /// says the piece above it is the same text, which the two rows keep the room of a paragraph
    /// in (see [`crate::main`]'s paragraph splits of long text) instead of the room between the
    /// blocks the agent put side by side.
    Markdown {
        text: Arc<str>,
        stitch: bool,
    },
    /// Its reasoning, with whether it is expanded.
    Thought {
        index: usize,
        text: String,
        open: bool,
    },
    Tool(Tool),
    /// A stretch of the agent's work (its tool calls and thoughts between two pieces of text) folded to one line:
    /// how many blocks it keeps, how many of them are tool calls, how many of those failed, how it went, and where
    /// it sits in the message.
    /// `task` is the plan entry it worked towards, which the row says in place of `Worked`; `took` is how long it
    /// took once it is over, `latest` the call it is at, `kinds` how many calls of each kind it made, and `edits` the
    /// files its edits touched.
    Run {
        index: usize,
        count: usize,
        calls: usize,
        failed: usize,
        phase: Phase,
        folded: bool,
        task: Option<String>,
        took: Option<std::time::Duration>,
        latest: Option<String>,
        kinds: Vec<(Kind, usize)>,
        edits: Vec<String>,
    },
    Plan(Vec<Entry>),
    /// A permission request, with the ways it may be answered; answered, it says what went.
    Ask {
        index: usize,
        title: String,
        options: Vec<AskOption>,
        answered: Option<String>,
    },
    /// Po's command, offered to be run: what would run, and the commands of it, both below the
    /// plain word of the user.
    Approve {
        index: usize,
        question: String,
        lines: Vec<String>,
        answered: Option<String>,
    },
}

/// One message: the user's, or the agent's column of blocks. The agent's blocks are shared: a
/// message that did not change keeps what the screen holds while it streams (see `po`'s
/// `chat_items`), and the engine's own comparisons of a message's blocks are bumps, not copies.
#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    User {
        text: String,
        pictures: Vec<Picture>,
    },
    Agent {
        index: usize,
        blocks: Arc<Vec<Block>>,
    },
}

/// What resuming a session shows: the sessions on offer, or why there are none.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Picking {
    Sessions(Vec<Session>),
    Note(String),
    /// The agent is waking or the sessions loading: the header says so, so the bar shows nothing.
    Loading,
}

/// What the viewer shows, for the header's buttons to say which is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Place {
    /// The viewer is closed, or shows something of the agent's.
    #[default]
    Elsewhere,
    Files,
    Changes,
    Terminal,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub title: String,
    /// The project and id of the thread on screen, which its title can rename.
    pub thread: (u64, u64),
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    /// The status lines of the header's right, like `3 threads · 1 asking`.
    pub statuses: Vec<String>,
    /// What the viewer shows.
    pub place: Place,
    /// Nothing on screen yet: the agent is picked below, and the thread starts when something
    /// is sent.
    pub blank: bool,
    /// A turn is going: the send button turns into a stop beside it.
    pub busy: bool,
    /// A started thread's agent can be changed now.
    pub switchable: bool,
    /// The agent takes messages mid-turn, so a send steers rather than queues.
    pub steer: bool,
    /// The agents a new thread can be started with, `(name, id)`.
    pub agents: Vec<(String, String)>,
    /// The agent picked for a thread that hasn't started.
    pub chosen: Option<String>,
    pub settings: Vec<Setting>,
    /// Whether the thread can start in a git worktree of its own (`None`: not in a repository).
    pub isolate: Option<bool>,
    /// What the thread's agent may do through Po: the Control menu reads and changes it.
    pub grant: Option<Grant>,
    /// The grant was raised after the agent started, so it has not reached the agent yet.
    pub grant_pending: bool,
    pub queue: Vec<Queued>,
    pub pictures: Vec<Picture>,
    /// Why some of what was pasted or dropped was left out.
    pub note: Option<String>,
    /// The conversation, in order.
    pub items: Vec<Item>,
    /// The sessions to resume, or why there are none to show.
    pub picking: Option<Picking>,
    /// What the draft holds, as the app sees it; the screen keeps what the user types.
    pub draft: String,
    /// Bumped when the app wrote the draft itself (taking a queued message back, clearing it),
    /// so the screen takes that text over.
    pub draft_reset: u64,
    /// Bumped when the field should take the keyboard (a new thread opened for a message).
    pub focus: u64,
    /// Bumped when the transcript should follow its end (the user sent something).
    pub follow: u64,
    /// Bumped as the turn goes on; the transcript follows it only when it is already at its end.
    pub follow_soft: u64,
}

pub(crate) static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> =
    LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// Shows `snapshot`; nothing is redrawn when it is what the screen already has.
#[hotpath::measure]
pub fn publish(snapshot: Snapshot) {
    SNAPSHOT.send_if_modified(|now| {
        let changed = *now != snapshot;
        if changed {
            *now = snapshot;
        }
        changed
    });
}

/// The snapshot the screen holds at the moment, for whoever composes the next one: a message
/// that would be built again exactly as it stands is reused as it stands instead (see `po`'s
/// `chat_items`). Only [`publish`] writes, and it waits for this borrow's end.
pub fn shown() -> tokio::sync::watch::Ref<'static, Snapshot> {
    SNAPSHOT.borrow()
}

/// What the user did, for [`crate::reader::state::Reader::run`].
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// The draft changed.
    Draft(String),
    /// Send the draft: now, or after the turn (`later`).
    Send { later: bool },
    /// Stop the agent.
    Stop,
    /// Pictures dropped on the pane, by their `file:` URLs.
    Drop(Vec<String>),
    /// Pictures pasted into the composer, with their bytes.
    Paste(Vec<Vec<u8>>),
    /// A picture comes off the draft.
    Detach(usize),
    /// A queued message goes back into the draft, to edit.
    Recall(usize),
    /// The sessions on offer are put away.
    ClosePicker,
    /// The note under the composer about what was left out is dismissed.
    DismissNote,
    /// The user gave the agent one of the ways of answering a permission request.
    Answer {
        message: usize,
        block: usize,
        at: usize,
    },
    /// Po's command was approved (`yes`) or refused.
    Approve {
        message: usize,
        block: usize,
        yes: bool,
    },
    /// A tool card's call and result are shown or hidden.
    ToggleTool { message: usize, block: usize },
    /// A thought is shown or hidden.
    ToggleThought { message: usize, block: usize },
    /// A run of tool calls folds, or unfolds.
    Fold {
        message: usize,
        block: usize,
        folded: bool,
    },
    /// A tool call wrote or changed a file: it opens in the other pane.
    Open(String),
    /// Which session to resume.
    Resume(String),
    /// The agent a new thread starts with, and the one on screen switches to.
    PickAgent(String),
    /// A setting of the session is changed.
    Setting { id: String, value: String },
    /// Whether a new thread gets a git worktree of its own.
    Isolate(bool),
    /// What the thread's agent may do through Po.
    Grant(GrantChange),
}

/// What the user asked for on the Control menu: a new grant in whole, or one of its reach
/// opt-ins tipped over.
#[derive(Clone, Debug, PartialEq)]
pub enum GrantChange {
    Set(Grant),
    Read,
    Approvals,
    Stop,
}

impl Snapshot {
    /// The snapshot with the shell's chrome: what the core builds has none.
    pub fn with_chrome(mut self, chrome: super::header::Chrome) -> Self {
        (self.lead, self.trail) = (chrome.lead, chrome.trail);
        self
    }
}

/// The snapshot now and after every change, for a screen that draws it.
#[allow(dead_code)] // read by the Rune host only
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}
