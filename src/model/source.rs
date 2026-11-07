//! The source screen's data: what [`crate::Reader`] publishes and what it sends back. No view code.
#![allow(unused_imports)]
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};
use tokio::sync::watch;

use super::header::Control;

/// A file's text, compared by identity so a snapshot that holds the same text costs nothing to
/// compare.
#[derive(Clone, Debug)]
pub struct Shared(pub Arc<str>);

impl PartialEq for Shared {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Body {
    /// Nothing is open: what to do about it.
    Hint(String),
    /// A file that can't be shown, and why.
    Notice(String),
    Code { path: String, text: Shared },
    /// A markdown file, rendered.
    Markdown(Shared),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub glass: bool,
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    /// The file's language, said in the header.
    pub language: String,
    /// A markdown file has a toggle: `Some(rendered)`.
    pub preview: Option<bool>,
    pub body: Body,
    /// A line to scroll to (from 0), and a number that changes each time one is asked for.
    pub reveal: Option<(u32, u64)>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { glass: false, title: String::new(), lead: vec![], trail: vec![], language: String::new(), preview: None, body: Body::Hint(String::new()), reveal: None }
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
