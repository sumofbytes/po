//! The strip screen's data: what [`crate::Reader`] publishes. No view code.
#![allow(unused_imports)]
use std::sync::LazyLock;

use tokio::sync::watch;

use super::header::Control;

/// The strip a PR-reviewed project shows, or the way to review one.
#[derive(Clone, PartialEq, Debug)]
pub enum Pr {
    /// The open PR's summary, with its way out.
    Opened { label: String },
    /// Nothing open: a field for the PR's number or URL, whether opening one is running, and
    /// how the last attempt went (`Some` while it runs, and failed once it stopped).
    Asking { status: Option<String>, opening: bool },
}

/// The strip a worktree project shows: its branch, and the ways to finish it.
#[derive(Clone, PartialEq, Debug)]
pub struct Worktree {
    pub branch: String,
    /// Discard was asked for once and waits for a second click, since it can't be undone.
    pub confirm_discard: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub worktree: Option<Worktree>,
    pub pr: Option<Pr>,
    /// Bumped each time the PR field was emptied after a PR opened: the screen clears what
    /// its field holds when it sees a new value.
    pub pr_reset: u64,
}

pub(crate) static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// Shows `snapshot`; nothing is redrawn when it is what the screen already has.
pub fn publish(snapshot: Snapshot) {
    SNAPSHOT.send_if_modified(|now| {
        let changed = *now != snapshot;
        if changed {
            *now = snapshot;
        }
        changed
    });
}

/// The snapshot now and after every change, for a screen that draws it.
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}
