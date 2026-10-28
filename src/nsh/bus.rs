//! What the NSH screens ask of Po. A screen only draws a snapshot Po publishes (the way
//! [`crate::todo`]'s store is read); everything the user does on it comes back here as a [`Cmd`],
//! which the [`crate::Reader`] runs where it has the window and the agents.
use std::sync::{LazyLock, Mutex};

use tokio::sync::mpsc;

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
    CloseProject(u64),
    /// "N more" or "Show less" under a project.
    ToggleMore(u64),
    OpenThread(u64, u64),
    CloseThread(u64, u64),
    AddProject,
    NewThread,
    OpenSettings,
    ToggleTodos,
    CloseTodos,
    ToggleIssues,
    CloseIssues,
    /// Start a thread in a worktree on an issue.
    WorkOnIssue(u64),
    /// The window's own controls, which sit in the headers of the panes at its edges.
    ToggleLeft,
    ToggleRight,
    ToggleFocus,
    /// A narrow viewer pane shows the explorer (true) or the file.
    Browse(bool),
    /// Markdown shown rendered, or as its source.
    ToggleRendered,
    OpenUrl(String),
    /// A tab of the viewer: the files (or changes), an agent's view or form, closing one.
    ShowHome,
    ShowView(String),
    CloseView(String),
    /// The explorer's mode switch: the changes (true) or the files.
    SetMode(bool),
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
type Channel = (mpsc::UnboundedSender<Cmd>, Mutex<Option<mpsc::UnboundedReceiver<Cmd>>>);

static CHANNEL: LazyLock<Channel> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::unbounded_channel();
    (sender, Mutex::new(Some(receiver)))
});

/// Asks Po to do `cmd`; any thread may.
pub fn send(cmd: Cmd) {
    let _ = CHANNEL.0.send(cmd);
}

/// The commands, for the one that runs them.
pub fn commands() -> Option<mpsc::UnboundedReceiver<Cmd>> {
    CHANNEL.1.lock().ok()?.take()
}

/// Held by a test of a screen for as long as it runs: the screens share this one channel, so they
/// take turns. Starts with nothing waiting in it.
#[cfg(test)]
pub fn lock() -> std::sync::MutexGuard<'static, Option<mpsc::UnboundedReceiver<Cmd>>> {
    let mut commands = CHANNEL.1.lock().unwrap_or_else(|e| e.into_inner());
    while commands.as_mut().is_some_and(|c| c.try_recv().is_ok()) {}
    commands
}
