//! The explorer screen's data: what [`crate::Reader`] publishes and what it sends back. No view code.
#![allow(unused_imports)]
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};
use tokio::sync::watch;

use super::header::Control;
use crate::explorer::{Row, Step};

/// A row of the tree, with what Po knows of its file.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub row: Row,
    /// `A`dded, `D`eleted, `M`odified…: how the file changed.
    pub mark: Option<char>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// Nothing to list: why.
    Empty(String),
    Rows(Vec<Entry>),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub glass: bool,
    /// Changes mode (true) or the files.
    pub changes: bool,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    pub body: Body,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { glass: false, changes: false, lead: vec![], trail: vec![], body: Body::Rows(vec![]) }
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
        (self.glass, self.lead, self.trail) = (chrome.glass, chrome.lead, chrome.trail);
        self
    }
}

/// The snapshot now and after every change, for a screen that draws it.
#[allow(dead_code)] // read by the Rune host only
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}
