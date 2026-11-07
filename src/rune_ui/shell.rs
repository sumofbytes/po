//! The window: a sidebar, the main pane and a viewer pane, as Rune's `SplitView` lays them out.
//! Counterpart of the layout half of `Reader::render` in `main.rs`. What fills the panes comes from
//! the screens; this decides what is shown where.
use rune::{El, col, text};
use rune_kit::{Kit, PaneState, SplitView};

use super::{settings, sidebar, terminal};
use crate::model;

/// What the main pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Terminal,
    Settings,
}

pub struct Shell {
    pub page: Page,
    pub sidebar: PaneState,
    pub viewer: PaneState,
    pub settings: Option<settings::Page>,
    pub term: terminal::Term,
    /// The sidebar's filter, and what the `Reader` last published for it.
    pub side: sidebar::State,
    pub snapshot: model::sidebar::Snapshot,
    snapshots: tokio::sync::watch::Receiver<model::sidebar::Snapshot>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            page: Page::Terminal,
            sidebar: PaneState::new(260.0).range(180.0, 420.0),
            viewer: PaneState::new(460.0).range(280.0, 900.0).collapsed(),
            settings: None,
            term: terminal::Term::default(),
            side: sidebar::State::default(),
            snapshot: model::sidebar::Snapshot::default(),
            snapshots: model::sidebar::watch(),
        }
    }
}

impl Shell {
    pub fn show(&mut self, page: Page) {
        if page == Page::Settings && self.settings.is_none() {
            self.settings = Some(settings::Page::load(false));
        }
        self.page = page;
    }

    /// Takes the sidebar snapshot the `Reader` published since the last call; true when there was one.
    pub fn poll(&mut self) -> bool {
        if self.snapshots.has_changed().unwrap_or(false) {
            self.snapshot = self.snapshots.borrow_and_update().clone();
            return true;
        }
        false
    }

    /// A key nothing in the window took: the shell's, while it is the page shown.
    pub fn key(&mut self, event: &rune::KeyEvent) -> bool {
        self.page == Page::Terminal && self.term.key(event)
    }

    /// Takes what a screen asked of the shell: a page that saved or closed is put away.
    pub fn settle(&mut self) {
        if let Some(outcome) = self.settings.as_mut().and_then(|page| page.outcome.take()) {
            let _ = outcome;
            self.settings = None;
            self.page = Page::Terminal;
        }
    }
}

/// The state of the app a [`Shell`] lives in: handlers reach it through this.
pub trait Host: 'static {
    fn shell(&mut self) -> &mut Shell;
}

fn sidebar_of<S: Host>(s: &mut S) -> &mut PaneState {
    &mut s.shell().sidebar
}

fn viewer_of<S: Host>(s: &mut S) -> &mut PaneState {
    &mut s.shell().viewer
}

fn term_of<S: Host>(s: &mut S) -> &mut terminal::Term {
    &mut s.shell().term
}

fn settings_of<S: Host>(s: &mut S) -> &mut settings::Page {
    s.shell().settings.as_mut().expect("the settings page is open")
}

fn side_of<S: Host>(s: &mut S) -> &mut sidebar::State {
    &mut s.shell().side
}

fn viewer(k: &Kit) -> El {
    col().w_full().h_full().center().child(text("Viewer").color(k.c.text_muted))
}

/// The whole window.
pub fn view<S: Host>(shell: &Shell, k: &Kit) -> El {
    let main = match (&shell.page, &shell.settings) {
        (Page::Settings, Some(page)) => settings::view::<S>(page, k, settings_of::<S>),
        _ => terminal::view::<S>(&shell.term, k, term_of::<S>),
    };
    SplitView::new(main)
        .sidebar::<S>(&shell.sidebar, sidebar_of::<S>, sidebar::view::<S>(&shell.snapshot, &shell.side, k, side_of::<S>))
        .inspector::<S>(&shell.viewer, viewer_of::<S>, viewer(k))
        .build(k)
}
