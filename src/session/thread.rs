use crate::reader::state::*;
use crate::session::chat::*;
use crate::*;

/// How much longer a parent must wait before it may be told something, given how long ago it
/// last was; `None` when it may be told now (never told, or long enough ago). A wait of nothing
/// is not a wait: it would put the notice back for ever.
pub(crate) fn notice_wait(since_last: Option<Duration>) -> Option<Duration> {
    NOTICE_RATE
        .checked_sub(since_last?)
        .filter(|left| !left.is_zero())
}

/// What starts a message of Po's in a transcript, so that it isn't taken for the user's.
pub(crate) const PO_MARK: &str = "[Po] ";

/// Where a picture to attach comes from.
pub(crate) enum Source {
    Bytes(Vec<u8>),
    Path(PathBuf),
}

/// A message headed for an agent, with where it came from.
pub(crate) struct Outgoing {
    pub(crate) text: String,
    pub(crate) origin: Origin,
    pub(crate) images: Vec<Arc<Attachment>>,
}

impl Outgoing {
    /// How the transcript shows it: Po's own messages say so, and never name the thread.
    pub(crate) fn shown(&self) -> String {
        match self.origin {
            Origin::Po => format!("{PO_MARK}{}", self.text),
            _ => self.text.clone(),
        }
    }

    pub(crate) fn user(text: String, images: Vec<Arc<Attachment>>) -> Self {
        Self {
            text,
            origin: Origin::User,
            images,
        }
    }

    /// What a daemon that takes only text is sent, and later tells back as accepted.
    pub(crate) fn queued_text(&self) -> String {
        Client::queued_text(&self.text, &self.images)
    }
}

/// How a thread reports to the thread that started it, when one did. Every report is a turn of
/// the parent's, so they are coalesced and rate limited (see [`Reader::flush_notice`]).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Notify {
    /// A turn's end, a failure, an ask, a sleep or a close.
    #[default]
    Done,
    /// Also that a turn started.
    All,
    Never,
}

impl Notify {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "done" => Self::Done,
            "all" => Self::All,
            "never" => Self::Never,
            _ => return None,
        })
    }
}

/// A message typed while the agent was working.
pub(crate) struct Queued {
    pub(crate) out: Outgoing,
    /// Where the daemon holds it; `None` when it waits here for the turn to end.
    pub(crate) kind: Option<Queue>,
}

/// One conversation with an agent: its messages, the process behind it and what is waiting to
/// be sent to it.
pub(crate) struct Thread {
    /// Documents the agent put on screen through Po's MCP server.
    pub(crate) views: crate::ipc::command::Views,
    /// Forms the agent put on screen through Po's MCP server, and the form of each.
    pub(crate) forms: crate::ipc::command::Forms,
    /// The view the viewer shows in place of a file or the changes.
    pub(crate) shown: Option<String>,
    /// A view the agent added while the user was busy, waiting for a click.
    pub(crate) offered: Option<String>,
    /// Names the thread to the events of its agent while it isn't the one on screen.
    pub(crate) id: u64,
    /// The thread that started this one through the API, if any. Only its parent may write to
    /// it, and it is never written to by a grandparent: a child starts at Viewer, so a
    /// grandchild can't exist. Saved as the parent's *session* (phase 4), never as its id.
    pub(crate) parent: Option<u64>,
    /// Its last turn ended in an error, or its agent stopped or exited mid-turn. Cleared when
    /// anything starts again, so the state reads Failed only while nothing newer has happened.
    pub(crate) failed: bool,
    /// Why it failed, the last time it did.
    pub(crate) last_error: Option<String>,
    /// Text sent through the API that a stopped turn dropped, for `thread.get` to report.
    pub(crate) dropped: Vec<String>,
    /// How much of its context the agent says the session uses.
    pub(crate) usage: Option<crate::session::state::Usage>,
    /// How it tells the thread that started it (its parent) what happened to it. `Done`: when
    /// its turn ends, it fails, it asks, or it is closed or put to sleep.
    pub(crate) notify: Notify,
    /// Settings the API asked for, applied between turns and never remembered as the user's
    /// choice: they are read before the carried-over choices, and nothing here writes to them.
    pub(crate) overrides: HashMap<String, String>,
    /// The ids of settings set through the API, so their answers are not saved as choices.
    pub(crate) api_options: HashSet<String>,
    /// It finished a turn while another thread was on screen.
    pub(crate) unread: bool,
    /// The agent's session, once it has one; what a thread without a running agent is resumed from.
    pub(crate) session: Option<String>,
    /// A session to load as soon as the agent has listed its sessions, instead of showing them.
    pub(crate) restore: Option<String>,
    /// What the thread was about before it was loaded back, since it has no messages yet.
    pub(crate) saved_title: Option<String>,
    /// A name the user gave it, which stands in place of the one made from the first message.
    pub(crate) renamed: Option<String>,
    /// When the thread last did anything; an idle one in the background is put to sleep.
    pub(crate) touched: Instant,
    /// When the thread last did anything, in milliseconds since the Unix epoch. Unlike
    /// `touched` it is saved, so the thread list keeps its order across restarts.
    pub(crate) active_at: u64,
    /// When the user last sent this thread a message (or it was made), in milliseconds since the
    /// Unix epoch. The sidebar orders by it, so a running thread doesn't jump about as it works.
    pub(crate) used_at: u64,
    /// A first message waiting for the agent to be ready, already shown in the thread.
    pub(crate) pending: Option<Outgoing>,
    /// Start the thread in a git worktree of its own. Only asked before it has started.
    pub(crate) isolate: bool,
    /// Nothing has been sent in it and no session was picked for it. Its agent runs only to
    /// offer the session's settings, so its session is never kept or resumed.
    pub(crate) blank: bool,
    /// The agent it was switched from, until the new one has finished a turn.
    pub(crate) switch: Option<Switch>,
    /// The settings being carried over from earlier threads, while the agent takes them one at
    /// a time: a model can change what the other settings offer. A first message waits for them.
    pub(crate) applying: Option<Applying>,
    /// Settings the agent was asked to carry over and gave up on answering; a late answer is
    /// not a choice of the user's.
    pub(crate) unanswered: HashSet<String>,
    pub(crate) messages: Vec<Msg>,
    pub(crate) busy: bool,
    /// Messages typed during a turn, shown above the input until they are taken.
    pub(crate) queue: Vec<Queued>,
    pub(crate) ready: bool,
    /// The agent's settings for the session, like its model, once it has said what they are.
    pub(crate) options: Vec<ConfigOption>,
    /// A resumed session's history is being replayed.
    pub(crate) loading: bool,
    /// Choosing a session to resume: the agent's list once it arrives.
    pub(crate) picking: bool,
    pub(crate) sessions: Option<Vec<SessionInfo>>,
    pub(crate) status: String,
    pub(crate) client: Option<Client>,
    pub(crate) agent: Option<String>,
    pub(crate) generation: u64,
    /// What the agent was granted when it started: its tools were listed under that, so a raise
    /// since takes a restart to reach it.
    pub(crate) started_grant: Option<crate::ipc::command::Grant>,
    pub(crate) draft: String,
    /// Pictures attached to the draft, sent with it.
    pub(crate) attachments: Vec<Arc<Attachment>>,
    /// Why some of what was pasted or dropped was left out, until the draft changes.
    pub(crate) attach_note: Option<String>,
}

impl Thread {
    pub(crate) fn fail(&mut self, text: String) {
        self.busy = false;
        self.failed = true;
        self.last_error = Some(text.clone());
        self.messages
            .push(Msg::Agent(vec![Block::Text(format!("**Error:** {text}"))]));
        self.relist(self.messages.len() - 1);
    }

    /// The bare facts the control API's read model is derived from.
    pub(crate) fn facts(&self) -> crate::session::state::Facts {
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
        crate::session::state::Facts {
            approval,
            asking,
            failed: self.failed,
            asleep: self.asleep(),
            started: self.client.is_some(),
            ready: self.ready,
            busy: self.busy,
        }
    }

    /// The conversation the screen draws takes the whole list of messages in every
    /// publish, so there is nothing to tell a list piecewise.
    pub(crate) fn relist(&mut self, _from: usize) {}

    pub(crate) fn blocks(&mut self) -> &mut Vec<Block> {
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
    pub(crate) fn tool_message(&self, update: &Value) -> Option<usize> {
        if update["sessionUpdate"].as_str() != Some("tool_call_update") {
            return None;
        }
        let id = update["toolCallId"].as_str().unwrap_or_default();
        self.messages.iter().rposition(|msg| match msg {
            Msg::Agent(blocks) => blocks
                .iter()
                .any(|b| matches!(b, Block::Tool(tool) if tool.id == id)),
            Msg::User(..) => false,
        })
    }

    pub(crate) fn update(&mut self, update: &Value) {
        let text = update["content"]["text"].as_str().unwrap_or_default();
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => match self.blocks().last_mut() {
                Some(Block::Text(body)) => body.push_str(text),
                _ => self.blocks().push(Block::Text(text.to_string())),
            },
            Some("user_message_chunk") if self.loading || !self.busy => {
                let text = match crate::agent::handoff::split(text) {
                    Some((said, message)) => {
                        self.messages.extend(said.into_iter().map(Msg::from));
                        message
                    }
                    None => text,
                };
                let text = without_context(text);
                let picture = (update["content"]["type"] == "image")
                    .then(|| Attachment::from_block(&update["content"]))
                    .flatten()
                    .map(Arc::new);
                match self.messages.last_mut() {
                    Some(Msg::User(body, pictures)) => {
                        body.push_str(text);
                        pictures.extend(picture);
                    }
                    _ => {
                        self.messages
                            .push(Msg::User(text.to_string(), picture.into_iter().collect()));
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
                    .map(|e| {
                        (
                            e["content"].as_str().unwrap_or_default().to_string(),
                            step(e["status"].as_str().unwrap_or("pending")),
                        )
                    })
                    .collect();
                let blocks = self.blocks();
                match blocks.iter_mut().find(|b| matches!(b, Block::Plan(_))) {
                    Some(block) => *block = Block::Plan(entries),
                    None => blocks.push(Block::Plan(entries)),
                }
            }
            Some("tool_call") => {
                let task = task_in_progress(&self.messages);
                let status = step(update["status"].as_str().unwrap_or("pending"));
                let now = Instant::now();
                self.blocks().push(Block::Tool(ToolBlock {
                    id: update["toolCallId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    title: update["title"].as_str().unwrap_or("Tool").to_string(),
                    status,
                    kind: update["kind"].as_str().unwrap_or_default().to_string(),
                    started: now,
                    ended: matches!(status, Step::Done | Step::Failed).then_some(now),
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
                        if let Some(kind) = update["kind"].as_str() {
                            tool.kind = kind.to_string();
                        }
                        if let Some(status) = update["status"].as_str() {
                            tool.status = step(status);
                            if matches!(tool.status, Step::Done | Step::Failed) {
                                tool.ended.get_or_insert_with(Instant::now);
                            } else {
                                tool.ended = None;
                            }
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
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

/// What an agent's session update changes in its thread: the usage, the earlier message a late
/// tool-call update touches (the tell of what to relist), and the message itself. The caller
/// records every update whoever owns the screen: a resumed session replays its history as updates
/// while it loads, and a turn on a background thread records while nothing of it shows.
pub(crate) fn record(thread: &mut Thread, update: &Value) -> Option<usize> {
    if let Some(usage) = crate::session::state::Usage::from_update(update) {
        thread.usage = Some(usage);
    }
    let earlier = thread.tool_message(update);
    thread.update(update);
    earlier
}

/// What a thread is up to, for the thread list.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Activity {
    Idle,
    /// Finished a turn nobody has looked at.
    Unread,
    Running,
    /// Waiting for an answer to a permission request.
    Asking,
    /// Its agent failed, until something is sent again.
    Failed,
}

impl Activity {
    /// Waiting on the user: a question to answer, or a finished turn nobody has looked at, unless
    /// `parent_hears` of it, since then the parent agent picks it up.
    pub(crate) fn needs_you(self, parent_hears: bool) -> bool {
        self == Activity::Asking || (self == Activity::Unread && !parent_hears)
    }
}

/// How long ago `then` (ms since the epoch) was, as the sidebar says it: `5m`, `3h`, `2d`, `6w`,
/// `4mo`. Nothing for the last minute, and for a time that was never recorded.
pub(crate) fn ago(now: u64, then: u64) -> Option<String> {
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

/// How long a thread that does nothing keeps its row in the sidebar; after that, search and the
/// history find it.
pub(crate) const STALE_MS: u64 = 24 * 3_600_000;

/// `then` (ms since the epoch) is more than [`STALE_MS`] before `now`. A counter from before the
/// time was kept is older than that.
pub(crate) fn stale(now: u64, then: u64) -> bool {
    now.saturating_sub(then) > STALE_MS
}

/// A thread that keeps its row in a project's list: the one on screen, whatever it is doing, its agent
/// running or asking, or something it did within the last day. One idle for longer only shows when
/// it is searched for; the history and the fold find the rest.
pub(crate) fn listed(now: u64, shown: u64, thread: &Thread) -> bool {
    thread.id == shown
        || matches!(thread.activity(), Activity::Running | Activity::Asking)
        || (!stale(now, thread.active_at)
            && (thread.activity() != Activity::Idle || thread.client.is_some()))
}

/// `title` with its first letter in capitals when it opens with a plain word, as "fix the build"
/// does. One that opens with a path, a command or an identifier (`src/main.rs`, `cargo test`,
/// `snake_case`, `v2`) is left as typed.
pub(crate) fn capitalized(title: String) -> String {
    let word = title
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([',', ':', ';', '.', '?', '!']);
    // A command (`cargo test`, `curl localhost`) opens with a plain word too, but is followed by
    // arguments: a flag, a path, a URL or a number. Prose is words and punctuation only.
    let prose = title.split_whitespace().skip(1).all(|w| {
        w.chars()
            .all(|c| c.is_alphabetic() || "'’,:;.?!-".contains(c))
    });
    // Bare subcommands (`cargo test`, `git status`) read as prose, so the usual tools are named.
    let tools = [
        "cargo", "git", "npm", "npx", "pnpm", "yarn", "bun", "make", "cd", "ls", "cat", "rm",
        "grep", "sudo", "gh", "docker", "kubectl", "rustc", "python", "pip", "node", "go", "curl",
        "ssh",
    ];
    let plain = !word.is_empty()
        && word.chars().all(|c| c.is_lowercase())
        && prose
        && !tools.contains(&word);
    match title.chars().next() {
        Some(_) if plain => crate::session::chat::sentence(&title),
        _ => title,
    }
}

/// The time now in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The most threads one thread may start (§5). With `max_live` at its default of 8, an
/// orchestrator and four children leave room for the user's own threads.
pub(crate) const MAX_CHILDREN: usize = 4;

/// The most `thread.wait`s the window holds, beside the socket's connections, so waits can't
/// push the viewer tools out.
pub(crate) const MAX_WAITS: usize = 8;

/// How long a child's changes may pile up before its parent is told.
pub(crate) const NOTICE_COALESCE: Duration = Duration::from_secs(2);

/// How often a parent may be told: every notice is a turn of the parent's, which costs tokens.
pub(crate) const NOTICE_RATE: Duration = Duration::from_secs(10);

impl Thread {
    /// A thread with no session yet and nothing said.
    pub(crate) fn blank(id: u64) -> Self {
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
            switch: None,
            restore: None,
            saved_title: None,
            renamed: None,
            touched: Instant::now(),
            active_at: now_ms(),
            used_at: now_ms(),
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
    pub(crate) fn preview(&self) -> String {
        preview_of(&self.messages, self.busy)
    }

    pub(crate) fn activity(&self) -> Activity {
        let asking = match self.messages.last() {
            Some(Msg::Agent(blocks)) => blocks.iter().any(|b| {
                matches!(
                    b,
                    Block::Permission(Ask { answered: None, .. })
                        | Block::Approve { answered: None, .. }
                )
            }),
            _ => false,
        };
        if asking {
            Activity::Asking
        } else if self.busy {
            Activity::Running
        } else if self.failed {
            Activity::Failed
        } else if self.unread {
            Activity::Unread
        } else {
            Activity::Idle
        }
    }

    /// The first thing asked, or a placeholder while there is none.
    pub(crate) fn title(&self) -> String {
        if let Some(name) = &self.renamed {
            return name.clone();
        }
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
                if short.len() < line.len() {
                    format!("{short}…")
                } else {
                    short
                }
            }
            None => self
                .saved_title
                .clone()
                .unwrap_or_else(|| "New thread".into()),
        }
    }

    /// An agent was started for it and then stopped (or never started after a restart), so it
    /// can be resumed from its session.
    pub(crate) fn asleep(&self) -> bool {
        self.client.is_none() && self.agent.is_some() && self.session.is_some()
    }

    /// Session settings in the order they are shown and carried over: the model first, as it
    /// can change what the others offer, then the others in the agent's order.
    pub(crate) fn ordered(mut options: Vec<&ConfigOption>) -> Vec<&ConfigOption> {
        options.sort_by_key(|option| option.category != "model");
        options
    }

    /// It was started with a first message, but its agent stopped before opening a session.
    pub(crate) fn unstarted(&self) -> bool {
        !self.blank && self.client.is_none() && self.session.is_none() && !self.picking
    }

    /// The agent and session it would come back to after a restart: not those of a switch the
    /// new agent has yet to hear of, as its session holds nothing of the thread.
    pub(crate) fn resumes_from(&self) -> Option<(&str, &str)> {
        match &self.switch {
            Some(switch) => Some((&switch.agent, &switch.session)),
            None => Some((self.agent.as_deref()?, self.session.as_deref()?)),
        }
    }

    /// Whether its agent can change now: nothing is under way that the change would lose, and
    /// its conversation is here to pass on.
    pub(crate) fn can_switch_agent(&self) -> bool {
        !self.blank
            && !self.at_risk()
            && self.pending.is_none()
            && !self.loading
            && !self.picking
            && self.restore.is_none()
            && self.resumes_from().is_some()
            && self.messages.iter().any(|msg| matches!(msg, Msg::User(..)))
    }

    /// Puts what an agent taking over was told back as the messages it was, in a first message
    /// that arrived in pieces and so was not seen whole as it came.
    pub(crate) fn unfold_handoffs(&mut self) {
        let mut messages = Vec::with_capacity(self.messages.len());
        for msg in std::mem::take(&mut self.messages) {
            match msg {
                Msg::User(text, pictures) => match crate::agent::handoff::split(&text) {
                    Some((said, message)) => {
                        messages.extend(said.into_iter().map(Msg::from));
                        messages.push(Msg::User(without_context(message).to_string(), pictures));
                    }
                    None => messages.push(Msg::User(text, pictures)),
                },
                other => messages.push(other),
            }
        }
        self.messages = messages;
        self.relist(0);
    }

    /// What was said in it, as an agent taking over is told.
    pub(crate) fn said(&self) -> Vec<crate::agent::handoff::Said> {
        let mut said = vec![];
        for msg in &self.messages {
            match msg {
                Msg::User(text, pictures) => {
                    let pictures =
                        (!pictures.is_empty()).then(|| format!("({} pictures)", pictures.len()));
                    said.push(crate::agent::handoff::Said::User(
                        pictures.map_or(text.clone(), |note| {
                            format!("{text}\n{note}").trim().to_string()
                        }),
                    ));
                }
                Msg::Agent(blocks) => {
                    let lines: Vec<String> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            Block::Text(text) => Some(text.clone()),
                            Block::Tool(tool) => Some(format!("`{}`", tool.title)),
                            _ => None,
                        })
                        .collect();
                    if !lines.is_empty() {
                        said.push(crate::agent::handoff::Said::Agent(lines.join("\n\n")));
                    }
                }
            }
        }
        said
    }

    /// Whether to keep it once another thread is on screen: a blank thread, or a session listing
    /// nothing was picked from, has nothing to come back to.
    pub(crate) fn worth_keeping(&self) -> bool {
        !self.blank && !self.is_listing()
    }

    /// It is showing an agent's sessions to pick one from, and nothing has been picked.
    pub(crate) fn is_listing(&self) -> bool {
        self.agent.is_some()
            && self.picking
            && !self.loading
            && self.messages.is_empty()
            && self.session.is_none()
    }

    /// Whether stopping its agent would lose nothing: no turn, no question and nothing queued.
    pub(crate) fn restful(&self) -> bool {
        self.client.is_some()
            && self.session.is_some()
            && self.activity() != Activity::Asking
            && !self.busy
            && self.queue.is_empty()
    }

    /// Whether closing it would lose something: a turn is running, a question waits or
    /// messages are queued. A blank thread has nothing to lose.
    pub(crate) fn at_risk(&self) -> bool {
        !self.blank && (self.busy || self.activity() == Activity::Asking || !self.queue.is_empty())
    }

    /// Lets a closed thread go. A running turn is asked to stop first and its agent given a
    /// moment to wind down before the process is killed, off the UI thread.
    pub(crate) fn retire(mut thread: Self) {
        /// How long a stopped agent has to wind down before its process is killed.
        const WIND_DOWN: Duration = Duration::from_secs(1);
        let Some(client) = thread.client.take() else {
            return;
        };
        if thread.busy {
            client.cancel();
            std::thread::spawn(move || {
                std::thread::sleep(WIND_DOWN);
                drop(client);
            });
        }
    }

    /// What is kept of it once closed, if it has a session to come back from.
    pub(crate) fn closed_state(&self) -> Option<crate::session::workspace::ThreadState> {
        if self.blank {
            return None;
        }
        let (agent, session) = self.resumes_from()?;
        Some(crate::session::workspace::ThreadState {
            agent: agent.to_string(),
            session: session.to_string(),
            title: self.title(),
            active_at: self.active_at,
            parent_session: None,
            renamed: self.renamed.is_some(),
        })
    }

    /// Stops the agent, keeping what has been said so it can be shown and resumed.
    pub(crate) fn sleep(&mut self) {
        self.drop_agent();
        self.status = if self.blank {
            String::new()
        } else {
            "asleep".into()
        };
    }

    /// Stops the agent and forgets what it was doing. A blank thread's session was never used,
    /// so it goes too, and the thread starts a new one when it is shown.
    pub(crate) fn drop_agent(&mut self) {
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
