//! The finder's data: the files that match, and the query it starts from. No view code.
use std::sync::{LazyLock, Mutex};

use tokio::sync::watch;

/// The files that match the query, in the order to show them.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub matches: Vec<String>,
    /// The walk stopped at this many files, so the list may be missing some.
    pub capped: Option<usize>,
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

/// What the field holds as the finder opens: the last query, which is selected.
pub(crate) static QUERY: Mutex<String> = Mutex::new(String::new());

/// Sets what the next finder starts from.
pub fn start(query: &str) {
    *QUERY.lock().unwrap_or_else(|e| e.into_inner()) = query.to_string();
}

/// The snapshot now and after every change, for a screen that draws it.
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}
