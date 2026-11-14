use crate::files::watch::*;
use crate::session::project::*;
use crate::session::thread::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agent::dex::Queue;
use crate::config;
use crate::files::watch::Prefetch;
use crate::host::rt::{Context, Window};

/// Where a message for the agent goes, judged by where its thread is: the decision
/// [`Reader::submit`] carries out, pure, so every path is testable without a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Where {
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

pub(crate) fn route(
    blank: bool,
    unstarted: bool,
    asleep: bool,
    starting: bool,
    busy: bool,
    takes_messages: bool,
    later: bool,
) -> Where {
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
pub(crate) fn find_thread<'a>(
    views: &'a [View<'a>],
    id: u64,
) -> Option<(&'a View<'a>, &'a Thread)> {
    views.iter().find_map(|view| {
        view.threads
            .iter()
            .find(|thread| thread.id == id)
            .map(|thread| (view, *thread))
    })
}

/// One `thread.wait`: who is waiting, on which threads (empty: its children), until a state in
/// `until` (empty: any change), and where the answer goes. A `form.wait` instead carries the
/// form's id and nothing else.
pub(crate) struct Waiter {
    pub(crate) token: u64,
    pub(crate) caller: crate::ipc::command::Caller,
    pub(crate) threads: Vec<u64>,
    pub(crate) until: Vec<&'static str>,
    pub(crate) form: Option<String>,
    pub(crate) reply: std::sync::mpsc::Sender<crate::ipc::command::Reply>,
}

/// What a parent hasn't heard about its children yet.
pub(crate) struct Notice {
    pub(crate) text: String,
}

/// A thread's change of agent, until the new agent has what was said. Until then the thread comes
/// back to the agent it left, and the new agent's first message carries `earlier`.
pub(crate) struct Switch {
    pub(crate) agent: String,
    pub(crate) session: String,
    pub(crate) earlier: String,
}

/// How far a new session has got in taking the settings last chosen for its agent.
#[derive(Default)]
pub(crate) struct Applying {
    /// The settings dealt with: carried over, already as chosen, or chosen in this thread since.
    pub(crate) settled: HashSet<String>,
    /// The setting the agent was asked to carry over and hasn't answered for yet.
    pub(crate) asked: Option<String>,
    /// Counts the questions put to the agent, so a timer can tell whether its own is still the one waiting.
    pub(crate) asks: u64,
    /// Whether the session is also taking the choices of earlier threads. False when it was
    /// woken only to apply what the API asked for.
    pub(crate) carrying: bool,
}

/// A project and its threads, wherever they are kept.
pub(crate) struct View<'a> {
    pub(crate) id: u64,
    pub(crate) mode: Mode,
    pub(crate) project: &'a Project,
    /// The thread that is, or would be, on screen.
    pub(crate) shown: u64,
    pub(crate) threads: Vec<&'a Thread>,
}

/// A project that isn't on screen, with its threads and view settings. They are swapped with
/// the same fields of [`Reader`] when it is shown.
pub(crate) struct Slot {
    pub(crate) id: u64,
    pub(crate) mode: Mode,
    pub(crate) project: Project,
    pub(crate) thread: Thread,
    pub(crate) parked: Vec<Thread>,
}

/// What a thread with the Global grant put to the user, to click or turn away: nothing it asks
/// for happens until they do.
pub(crate) struct Suggestion {
    pub(crate) id: u64,
    /// The thread that suggested it, and its title as it was.
    pub(crate) from: u64,
    pub(crate) by: String,
    pub(crate) what: Proposal,
}

pub(crate) enum Proposal {
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
pub(crate) struct Audit {
    pub(crate) at: u64,
    pub(crate) caller: crate::ipc::command::Caller,
    pub(crate) command: &'static str,
    pub(crate) outcome: String,
    /// How many times in a row it was this, for the same caller.
    pub(crate) count: u32,
}

/// How many commands the audit log keeps, newest last.
pub(crate) const MAX_AUDIT: usize = 500;

/// How many suggestions may wait at once, so a loop can't fill the sidebar.
pub(crate) const MAX_SUGGESTIONS: usize = 8;

/// How long the user must have been still before an agent may put a view in front of them.
pub(crate) const QUIET: Duration = Duration::from_secs(4);

/// A command that moves the viewer, held back until the user is still.
pub(crate) struct PendingMove {
    pub(crate) thread: u64,
    pub(crate) command: crate::ipc::command::Command,
    /// What was on screen when it was asked for. If that is not the same when the pause comes,
    /// the user moved the viewer themselves, and theirs stands.
    pub(crate) from: Screen,
}

/// Which panes are open, as the workspace saves it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Panes {
    pub(crate) viewer: bool,
    pub(crate) agent: bool,
}

/// A layout an agent changed: what it was before, which is the user's, and what the agent left.
/// The workspace saves the user's while the agent's is still on screen, so an agent's move lasts
/// only until `po` is closed; once the user changes it again, what they leave is theirs.
#[derive(Clone, Copy)]
pub(crate) struct AgentChange<T> {
    pub(crate) before: T,
    pub(crate) left: T,
}

impl<T: Copy + PartialEq> AgentChange<T> {
    /// The record after an agent moved `from` to `to`: an earlier move it builds on keeps the
    /// user's layout from before that one.
    pub(crate) fn after(earlier: Option<Self>, from: T, to: T) -> Self {
        let before = earlier
            .filter(|change| change.left == from)
            .map_or(from, |change| change.before);
        Self { before, left: to }
    }

    /// The record if the layout is still what the agent left; once the user has moved on it is
    /// gone, and coming back to the agent's layout by hand is theirs.
    pub(crate) fn settled(change: Option<Self>, now: T) -> Option<Self> {
        change.filter(|change| change.left == now)
    }

    /// What to save of `now`.
    pub(crate) fn saved(change: Option<&Self>, now: T) -> T {
        change
            .filter(|change| change.left == now)
            .map_or(now, |change| change.before)
    }
}

impl PendingMove {
    /// The agent itself changed the layout, not the user: what it touched (`shown`, the viewer
    /// being open) is noted, so its move is not dropped for its own doing. A move of another
    /// thread stays as it was; it is dropped when that thread goes off screen.
    pub(crate) fn follows_agent(&mut self, on_screen: u64, now: &Screen) {
        if self.thread == on_screen {
            self.from.shown.clone_from(&now.shown);
            self.from.side_open = now.side_open;
            self.from.agent_open = now.agent_open;
        }
    }
}

/// What the user sees of the viewer: whether it is open, in which mode, and on what.
#[derive(Clone, PartialEq)]
pub(crate) struct Screen {
    pub(crate) side_open: bool,
    pub(crate) agent_open: bool,
    pub(crate) mode: Mode,
    pub(crate) current: Option<String>,
    pub(crate) picked: Option<String>,
    pub(crate) shown: Option<String>,
}

/// What the chat last showed, to tell what changed since: the thread, the lengths of its lists and
/// whether it was scrolled to the end.
pub(crate) type ChatMarks = ((u64, u64), usize, usize, bool, usize);

pub(crate) struct Reader {
    /// How agents started from this window reach it; none where there is no socket.
    pub(crate) control: Option<crate::ipc::control::Control>,
    /// What each thread's agent may do, held in memory only: it is lost on restart and is not
    /// shared between windows, so nothing an agent can read or write raises its own reach.
    /// A thread without an entry has [`crate::ipc::command::Grant::initial`], and the grants can only be
    /// changed from the UI, never from anything an agent could write to.
    pub(crate) grants: HashMap<u64, crate::ipc::command::Grant>,
    /// Projects the API opened, which it alone may close again (and only if the user has no
    /// thread of their own in them), and what it has put to the user to click.
    pub(crate) api_projects: HashSet<u64>,
    pub(crate) suggestions: Vec<Suggestion>,
    pub(crate) next_suggestion: u64,
    pub(crate) audit: std::collections::VecDeque<Audit>,
    /// `po ctl watch` connections, each told of every change of a thread's state.
    pub(crate) subscribers: Vec<std::sync::mpsc::Sender<crate::ipc::command::Reply>>,
    /// A worktree for `thread.create_isolated` is being made, and the numbers its replies carry.
    pub(crate) isolating: bool,
    pub(crate) next_request: u64,
    /// `thread.wait`s held until a thread reaches a state or the wait times out. At most two
    /// per caller, [`MAX_WAITS`] in all, so waits can't push the viewer tools out.
    pub(crate) waiters: Vec<Waiter>,
    pub(crate) next_waiter: u64,
    /// What a parent hasn't been told about its children yet, one notice per parent: lines
    /// coalesce into it for [`NOTICE_COALESCE`], and the rate limit holds the flush back.
    pub(crate) notices: HashMap<u64, Notice>,
    pub(crate) notice_scheduled: HashSet<u64>,
    pub(crate) notice_sent_at: HashMap<u64, Instant>,
    /// When the user last pressed a key or scrolled; an agent waits for a pause to show something.
    pub(crate) last_input: Instant,
    /// A viewer move the thread on screen asked for while the user was busy, waiting for a pause.
    pub(crate) pending_move: Option<PendingMove>,
    /// The panes an agent last opened or closed, and each project's mode an agent last changed,
    /// by project id; see [`AgentChange`].
    pub(crate) agent_panes: Option<AgentChange<Panes>>,
    pub(crate) agent_modes: HashMap<u64, AgentChange<Mode>>,
    pub(crate) mode: Mode,
    /// The project on screen; its methods act on this one.
    pub(crate) project: Project,
    pub(crate) project_id: u64,
    /// The other projects, with their threads still running.
    pub(crate) slots: Vec<Slot>,
    pub(crate) next_project: u64,
    /// The thread on screen in this project; its methods act on this one.
    pub(crate) thread: Thread,
    /// This project's other threads, still running; swapped in with [`Self::with_thread`] to
    /// handle their events.
    pub(crate) parked: Vec<Thread>,
    pub(crate) next_thread: u64,
    /// The directory `po` was started in: where PRs are checked out from and where Close returns to.
    pub(crate) home: PathBuf,
    /// The sidebar's filter: threads and projects that don't match it are left out of its lists.
    /// What the sidebar's filter holds.
    pub(crate) side_query: String,
    /// A line the viewer was asked to scroll to, and a number that changes with each ask.
    pub(crate) reveal: Option<(u32, u64)>,
    pub(crate) reveal_serial: u64,
    /// What the conversation was last published as, one row for how many messages the transcript
    /// showed and whether a turn went on the thread it shows; a change bumps [`Self::chat_follow`].
    pub(crate) chat_marks: std::cell::Cell<ChatMarks>,
    /// The project and thread the conversation showed last.
    pub(crate) chat_shown: std::cell::Cell<(u64, u64)>,
    /// What the transcript's next publishing carries as [`Snapshot::follow`], bumped when the thread gets louder.
    pub(crate) chat_follow: std::cell::Cell<u64>,
    /// Like [`Self::chat_follow`], for the thread growing under the reader's eyes: the transcript
    /// follows it only while it is at its end.
    pub(crate) chat_follow_soft: std::cell::Cell<u64>,
    /// What [`Snapshot::draft_reset`] carries next, bumped whenever the app writes the draft itself.
    pub(crate) chat_reset: std::cell::Cell<u64>,
    /// What [`model::chat::Snapshot::focus`] carries next.
    pub(crate) chat_focus: std::cell::Cell<u64>,
    pub(crate) issues_open: bool,
    pub(crate) home_open: bool,
    /// An issue "Work on this" is making a worktree for, to link its branch once it exists.
    pub(crate) issue_for_worktree: Option<(PathBuf, u64)>,
    /// The blank thread (by id) whose draft "Work on this" wrote, and the issue it is for: sending
    /// the draft starts the issue, and until then nothing has begun.
    pub(crate) issue_draft: Option<(u64, PathBuf, u64)>,
    /// Progress or failure of the last attempt to open a PR.
    pub(crate) pr_status: Option<String>,
    pub(crate) opening: bool,
    /// The number of times the strip's PR field was emptied: the screen empties its field on
    /// seeing a new one.
    pub(crate) pr_serial: u64,
    /// The last thing that happened to a worktree (made, merged, closed), or why it failed.
    pub(crate) worktree_status: Option<String>,
    /// A merge or discard is running; asking for either again waits.
    pub(crate) finishing: bool,
    /// Discard was asked for once and waits for a second click, since it can't be undone.
    pub(crate) confirm_discard: bool,
    pub(crate) finder_query: String,
    pub(crate) finder_matches: Vec<String>,
    pub(crate) finder_open: bool,
    pub(crate) settings_open: bool,
    pub(crate) agents: Vec<config::Agent>,
    /// Tell the desktop when a thread finishes or asks while it isn't being looked at.
    pub(crate) notifications: bool,
    /// How long a thread in the background can sit idle before its agent is stopped.
    pub(crate) idle_after: Option<Duration>,
    /// Agents that may run at once; beyond it the longest idle in the background are stopped.
    pub(crate) max_live: usize,
    pub(crate) left_open: bool,
    /// The file tree beside the file is hidden (a session choice).
    pub(crate) tree_hidden: bool,
    /// The widths the panes were last dragged to, in points, for the workspace to save.
    pub(crate) widths: Widths,
    /// What the shell and the background of the terminal are told the window's colours are.
    pub(crate) term_theme: (crate::system::terminal::Rgb, crate::system::terminal::Rgb),
    /// Projects whose thread list is shown in full rather than cut to the active ones.
    pub(crate) threads_expanded: HashSet<u64>,
    /// Projects whose threads are folded away in the sidebar.
    pub(crate) projects_collapsed: HashSet<u64>,
    /// The order the user dragged the projects into, first to last; one not in it follows, by id.
    pub(crate) project_order: Vec<u64>,
    /// Each project's checked-out branch, read off the UI thread now and then.
    pub(crate) heads: HashMap<PathBuf, crate::vcs::worktree::Head>,
    /// In a pane too narrow for the tree beside the file, the explorer is what the pane shows.
    pub(crate) browse: bool,
    /// The viewer pane (a file or a diff) on the right is shown.
    pub(crate) side_open: bool,
    /// The viewer is in the middle and the conversation on the right, instead of the reverse.
    /// Only matters while the viewer is open.
    pub(crate) swapped: bool,
    /// The conversation pane is shown while it sits on the right, rather than collapsed. Only
    /// matters while it is there (`side_open` and `swapped`); in the middle it is always shown.
    pub(crate) agent_open: bool,
    /// What was chosen for each agent's settings, such as its model.
    pub(crate) choices: config::Choices,
    /// The agent new threads start with: the one last chosen.
    pub(crate) chosen_agent: Option<String>,
    /// Whether diffs show old and new side by side instead of inline.
    pub(crate) split: bool,
    /// Markdown files show rendered rather than as source.
    pub(crate) rendered: bool,
    /// The workspace is about to be saved, so asking again changes nothing.
    pub(crate) save_pending: bool,
    /// What this window last saved or restored, for saving beside other windows.
    pub(crate) known_projects: crate::session::workspace::Known,
}

impl Reader {
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn new(
        home: PathBuf,
        pr: Option<crate::vcs::pr::Pr>,
        command: Option<String>,
        prefetch: Prefetch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = pr
            .as_ref()
            .map_or_else(|| home.clone(), |pr| pr.worktree.clone());
        let prefetch = Some(prefetch).filter(|prefetch| prefetch.root == root);
        let (files, watch) = match prefetch {
            Some(Prefetch { files, watch, .. }) => (files.join().unwrap_or_default(), Some(watch)),
            None => {
                let mut files = Vec::new();
                walk(&root, &mut files);
                (files, None)
            }
        };
        crate::dev::trace::mark("walk");
        let settings = crate::config::plugin::effective_config();
        let mut agents = config::agents(&settings);
        let idle_minutes = settings["idle_minutes"].as_u64().unwrap_or(15);
        if let Some(command) = command
            .as_ref()
            .filter(|c| !agents.iter().any(|agent| agent.id == **c))
        {
            agents.push(config::Agent {
                id: command.clone(),
                name: "Custom".into(),
                command: command.clone(),
                daemon: None,
                vouched: true,
                plugin: None,
            });
        }
        let project = Self::build_project(root, pr, files);
        let thread = Thread::blank(0);
        let (control, requests) = crate::ipc::control::start().unzip();
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
            last_input: Instant::now()
                .checked_sub(QUIET)
                .unwrap_or_else(Instant::now),
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
            side_query: String::new(),
            reveal: None,
            reveal_serial: 0,
            chat_marks: std::cell::Cell::new(((0, 0), 0, 0, false, 0)),
            chat_shown: std::cell::Cell::new((u64::MAX, u64::MAX)),
            chat_follow: std::cell::Cell::new(0),
            chat_follow_soft: std::cell::Cell::new(0),
            chat_reset: std::cell::Cell::new(0),
            chat_focus: std::cell::Cell::new(0),
            issues_open: false,
            home_open: false,
            issue_for_worktree: None,
            issue_draft: None,
            pr_status: None,
            opening: false,
            worktree_status: None,
            finishing: false,
            confirm_discard: false,
            finder_query: String::new(),
            finder_matches: Vec::new(),
            finder_open: false,
            pr_serial: 0,
            settings_open: false,
            agents,
            notifications: settings["notifications"].as_bool().unwrap_or(true),
            idle_after: (idle_minutes > 0).then(|| Duration::from_secs(idle_minutes * 60)),
            max_live: settings["max_live_agents"].as_u64().unwrap_or(8).max(1) as usize,
            left_open: true,
            tree_hidden: false,
            widths: Widths::default(),
            term_theme: (
                crate::system::terminal::Rgb(0xdd, 0xdd, 0xdd),
                crate::system::terminal::Rgb(0x12, 0x12, 0x12),
            ),
            threads_expanded: HashSet::new(),
            projects_collapsed: HashSet::new(),
            project_order: vec![],
            heads: HashMap::new(),
            browse: true,
            side_open: false,
            swapped: config::viewer_left(&settings).unwrap_or(false),
            agent_open: true,
            choices: config::load_choices(),
            chosen_agent: command.clone(),
            split: false,
            rendered: true,
            save_pending: false,
            known_projects: Default::default(),
        };
        if let Some(requests) = requests {
            Self::serve_control(requests, window, cx);
        }
        Self::serve_nsh(window, cx);
        Self::watch_issues(cx);
        crate::dev::trace::mark("reader built");
        reader.refresh_tree();
        crate::dev::trace::mark("tree");
        let resumed = crate::session::workspace::Workspace::load(&reader.home)
            .is_some_and(|saved| reader.restore(saved, cx));
        reader.known_projects = reader.workspace().known();
        crate::dev::trace::mark("restore");
        let landing = command.is_none();
        match command.filter(|_| !resumed) {
            Some(command) => reader.connect(&command, false, window, cx),
            None => reader.wake_thread(window, cx),
        }
        // Po opens on Home, unless it was started to do something: a command to run, or a session
        // worth returning to.
        if landing && !resumed {
            reader.show_home(window, cx);
        }
        crate::dev::trace::mark("connect");
        reader.watch(watch, window, cx);
        crate::dev::trace::mark("watch");
        Self::poll_heads(cx);
        Self::sweep_idle(window, cx);
        reader
    }
}

/// The widths of the left side, the viewer pane and the file tree, when they were set.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct Widths {
    pub left: Option<f32>,
    pub right: Option<f32>,
    pub explorer: Option<f32>,
}
