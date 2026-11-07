//! The sidebar screen's data: what [`crate::Reader`] publishes and what it sends back. No view code.
#![allow(unused_imports)]
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};
use tokio::sync::watch;

use super::header::Control;

/// What a thread is up to, for its mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Idle,
    /// Finished a turn nobody has looked at.
    Unread,
    Running,
    /// Waiting for an answer to a permission request.
    Asking,
    /// Its agent failed.
    Failed,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Thread {
    pub project: u64,
    pub id: u64,
    pub label: String,
    /// Its own name, which the label is when it is not shown as a step under its parent: what a
    /// rename starts from.
    pub name: String,
    pub mark: Mark,
    /// On screen: the only filled row.
    pub selected: bool,
    /// What its agent may do through Po's API is more than talk.
    pub controlled: bool,
    /// How long ago it was last active (`5m`, `3h`, `2d`), if that is known.
    pub ago: Option<String>,
    /// Its close was pressed and closing it would lose work: pressing again closes it.
    pub closing: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    pub id: u64,
    pub name: String,
    /// The one on screen has its name at full strength.
    pub active: bool,
    pub collapsed: bool,
    /// More than one project is open, so this one can be closed.
    pub closable: bool,
    pub threads: Vec<Thread>,
    /// A fold under the threads: `(expanded, how many are hidden)`.
    pub fold: Option<(bool, usize)>,
}

/// A thread waiting on the user, with the project it is in.
#[derive(Clone, PartialEq, Debug)]
pub struct Waiting {
    pub thread: Thread,
    pub project: String,
}

/// What an agent has put to the user: a line, and the label of its yes.
#[derive(Clone, PartialEq, Debug)]
pub struct Suggestion {
    pub id: u64,
    pub text: String,
    pub yes: &'static str,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    /// The glass look is on: headers have no line under them.
    pub glass: bool,
    pub waiting: Vec<Waiting>,
    pub projects: Vec<Project>,
    pub suggestions: Vec<Suggestion>,
    /// What a worktree is doing, under the Projects header.
    pub status: Option<String>,
    pub searching: bool,
    pub issues_open: bool,
    /// The Home page is on screen.
    pub home_open: bool,
    /// How many of the project's issues are open, and how many of those are late.
    pub issues: (usize, usize),
}

pub(crate) static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// The snapshot now and after every change, for a screen that shows threads of its own.
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}

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

impl Snapshot {
    /// The snapshot with the window's theme, which the core doesn't know.
    pub fn with_glass(mut self, glass: bool) -> Self {
        self.glass = glass;
        self
    }
}
