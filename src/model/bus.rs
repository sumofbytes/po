//! What the NSH screens ask of Po. A screen only draws a snapshot Po publishes (the way
//! [`crate::issues::shared`]'s store is read); everything the user does on it comes back here as a [`Cmd`],
//! which the [`crate::Reader`] runs where it has the window and the agents.
use std::sync::{LazyLock, Mutex};

use tokio::sync::mpsc;

#[cfg(feature = "hotpath")]
use hotpath::wrap::tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
#[cfg(not(feature = "hotpath"))]
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::chat::Cmd as ChatCmd;

/// Something the user did on a screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// The conversation, the big one.
    Chat(ChatCmd),
    /// The sidebar's filter changed.
    SideQuery(String),
    /// A project row was pressed: fold or unfold its threads, and show it.
    ToggleProject(u64),
    StartThread(u64),
    /// The close button on a project's header.
    CloseProject(u64),
    /// "N more" or "Show less" under a project.
    ToggleMore(u64),
    OpenThread(u64, u64),
    CloseThread(u64, u64),
    AddProject,
    NewThread,
    OpenSettings,
    ToggleIssues,
    /// The Home page, the landing page: put up or put away.
    ToggleHome,
    /// An issue of the repo given, on the issues page.
    OpenIssue(std::path::PathBuf, u64),
    CloseIssues,
    /// Start a thread in a worktree on an issue of the repo given.
    WorkOnIssue(std::path::PathBuf, u64),
    /// The line under the Projects header is put away.
    DismissStatus,
    /// The window's own controls, which sit in the headers of the panes at its edges.
    ToggleLeft,
    ToggleRight,
    ToggleFocus,
    /// The files, or the changes (true), in the viewer; or the terminal.
    Show(bool),
    Terminal,
    /// Markdown shown rendered, or as its source.
    ToggleRendered,
    /// A tab of the viewer: an agent's view or form, closing one.
    ShowView(String),
    CloseView(String),
    /// A row of the explorer's tree was pressed: a file to open, or a folder to flip.
    TreeClick(String),
    /// An arrow key in the explorer's tree.
    TreeStep(crate::explorer::Step),
    /// The diff shown side by side (true) or inline.
    SetSplit(bool),
    /// The finder's query changed.
    FinderQuery(String),
    /// A file picked in the finder.
    FinderPick(String),
    CloseFinder,
    CloseSettings,
    /// The settings were written to config.json.
    SettingsSaved,
    /// An agent's suggestion, answered yes or no.
    Answer(u64, bool),
    /// A form the agent put on screen, answered by the user.
    SubmitForm(String, serde_json::Value),
    /// A PR to open for review, from the strip's field.
    OpenPr(String),
    /// The PR review on screen is closed.
    ClosePr,
    /// The worktree on screen is merged, or discarded with its branch.
    MergeWorktree,
    DiscardWorktree,
}

/// The sending end, and the receiving end until the one listener takes it.
type Channel = (UnboundedSender<Cmd>, Mutex<Option<Commands>>);

/// The receiving end: hotpath's wrapper of it in a profiled build.
pub type Commands = UnboundedReceiver<Cmd>;

static CHANNEL: LazyLock<Channel> = LazyLock::new(|| {
    let (sender, receiver) = hotpath::channel!(mpsc::unbounded_channel(), label = "nsh commands");
    (sender, Mutex::new(Some(receiver)))
});

/// Asks Po to do `cmd`; any thread may.
pub fn send(cmd: Cmd) {
    let _ = CHANNEL.0.send(cmd);
}

/// The commands, for the one that runs them.
pub fn commands() -> Option<Commands> {
    CHANNEL.1.lock().ok()?.take()
}
