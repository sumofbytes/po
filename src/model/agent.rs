//! The agent screen's data: what [`crate::Reader`] publishes and what it sends back. No view code.
#![allow(unused_imports)]
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};
use tokio::sync::watch;

use super::header::Control;
use super::source::Shared;

/// One field of the form on screen.
#[derive(Clone, PartialEq, Debug)]
pub struct Field {
    pub name: String,
    pub label: String,
    /// The options to pick among; empty is a field to type into.
    pub options: Vec<String>,
    /// The text it starts with, or the choice picked to begin with.
    pub value: Option<String>,
}

/// A markdown document the agent wrote, or a form of its the user answers. What a field of the
/// form holds is the screen's own, and it is seeded anew when the `id` or the `fields` of the
/// form change, which is what a replaced form means.
#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// Only the header: the terminal, which the app draws below it.
    Bare,
    Markdown(Shared),
    Form { id: String, fields: Vec<Field>, submit: String, answered: bool, doc: Option<Shared> },
}

impl Default for Body {
    fn default() -> Self {
        Self::Markdown(Shared(Arc::from("")))
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub glass: bool,
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    pub body: Body,
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

impl Snapshot {
    /// The snapshot with the shell's chrome: what the core builds has none.
    pub fn with_chrome(mut self, chrome: super::header::Chrome) -> Self {
        (self.glass, self.lead, self.trail) = (chrome.glass, chrome.lead, chrome.trail);
        self
    }
}

/// The snapshot now and after every change, for a screen that draws it.
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}
