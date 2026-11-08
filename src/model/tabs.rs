//! The tabs screen's data: what [`crate::Reader`] publishes. No view code.
#![allow(unused_imports)]
use std::sync::LazyLock;

use tokio::sync::watch;

use super::header::Control;

#[derive(Clone, PartialEq, Debug)]
pub struct Tab {
    pub id: String,
    pub title: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    /// The tab on screen; none is the files or the changes.
    pub shown: Option<String>,
    pub tabs: Vec<Tab>,
    /// A view the agent offers to show, by its title.
    pub offer: Option<Tab>,
    /// The window's own controls at the strip's ends, which the pane's headers then go without.
    pub controls: (Vec<Control>, Vec<Control>),
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
    /// The snapshot with the window's controls, which the layout decides.
    pub fn with_controls(mut self, controls: (Vec<Control>, Vec<Control>)) -> Self {
        self.controls = controls;
        self
    }
}

/// The snapshot now and after every change, for a screen that draws it.
#[allow(dead_code)] // read by the Rune host only
pub fn watch() -> watch::Receiver<Snapshot> {
    SNAPSHOT.subscribe()
}
