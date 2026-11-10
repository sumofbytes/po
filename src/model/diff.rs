//! The diff screen's data: what [`crate::Reader`] publishes and what it sends back. No view code.
#![allow(unused_imports)]
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};
use tokio::sync::watch;

use super::header::Control;
use super::source::Shared;

#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// No file is picked: what to do about it.
    Hint(String),
    /// The diff is being worked out.
    Loading,
    /// The file has no textual changes.
    NoChanges,
    /// The file's two versions, `path` naming the language.
    Versions { path: String, old: Shared, new: Shared },
}

#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    /// Side by side (true) or inline.
    pub split: bool,
    /// The lines added and removed.
    pub counts: (usize, usize),
    pub body: Body,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { title: String::new(), lead: vec![], trail: vec![], split: true, counts: (0, 0), body: Body::Loading }
    }
}

pub(crate) static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

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
