//! The window: a sidebar, the main pane and a viewer pane, as Rune's `SplitView` lays them out.
//! Counterpart of the layout half of `Reader::render` in `main.rs`. What fills the panes comes from
//! the screens; this decides what is shown where.
use rune::{El, col};
use rune_kit::{Kit, PaneState, SplitView};

use super::{agent, diff, explorer, finder, settings, sidebar, source, tabs, terminal};
use crate::model;

/// What the `Reader` last published for a screen, and the channel it comes through.
pub struct Feed<T> {
    pub value: T,
    rx: tokio::sync::watch::Receiver<T>,
}

impl<T: Clone> Feed<T> {
    pub fn new(rx: tokio::sync::watch::Receiver<T>) -> Self {
        let value = rx.borrow().clone();
        Self { value, rx }
    }

    /// Takes what was published since the last call; true when there was something.
    pub fn poll(&mut self) -> bool {
        if self.rx.has_changed().unwrap_or(false) {
            self.value = self.rx.borrow_and_update().clone();
            return true;
        }
        false
    }
}

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
    pub sidebar_feed: Feed<model::sidebar::Snapshot>,
    pub explorer: explorer::State,
    pub explorer_feed: Feed<model::explorer::Snapshot>,
    pub strip_feed: Feed<model::strip::Snapshot>,
    pub tabs_feed: Feed<model::tabs::Snapshot>,
    pub source_feed: Feed<model::source::Snapshot>,
    pub source: source::State,
    pub diff_feed: Feed<model::diff::Snapshot>,
    pub diff: diff::State,
    pub agent_feed: Feed<model::agent::Snapshot>,
    pub agent: agent::State,
    pub finder: finder::State,
    pub finder_feed: Feed<model::finder::Snapshot>,
    /// The Ctrl+P finder is up over the main pane.
    pub finder_open: bool,
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
            sidebar_feed: Feed::new(model::sidebar::watch()),
            explorer: explorer::State::default(),
            explorer_feed: Feed::new(model::explorer::watch()),
            strip_feed: Feed::new(model::strip::watch()),
            tabs_feed: Feed::new(model::tabs::watch()),
            source_feed: Feed::new(model::source::watch()),
            source: source::State::default(),
            diff_feed: Feed::new(model::diff::watch()),
            diff: diff::State::default(),
            agent_feed: Feed::new(model::agent::watch()),
            agent: agent::State::default(),
            finder: finder::State::default(),
            finder_feed: Feed::new(model::finder::watch()),
            finder_open: false,
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

    /// Takes what the `Reader` published since the last call; true when the view is stale.
    pub fn poll(&mut self) -> bool {
        let mut stale = self.sidebar_feed.poll();
        stale |= self.explorer_feed.poll();
        stale |= self.strip_feed.poll();
        stale |= self.tabs_feed.poll();
        stale |= self.finder_feed.poll();
        if self.source_feed.poll() {
            self.source.follow(&self.source_feed.value);
            stale = true;
        }
        if self.diff_feed.poll() {
            self.diff.follow(&self.diff_feed.value);
            stale = true;
        }
        if self.agent_feed.poll() {
            self.agent.follow(&self.agent_feed.value);
            stale = true;
        }
        self.explorer.follow(&self.strip_feed.value);
        stale
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
    fn shell_ref(&self) -> &Shell;
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

fn explorer_of<S: Host>(s: &mut S) -> &mut explorer::State {
    &mut s.shell().explorer
}

fn diff_ref<S: Host>(s: &S) -> &diff::State {
    &s.shell_ref().diff
}

fn diff_mut<S: Host>(s: &mut S) -> &mut diff::State {
    &mut s.shell().diff
}

fn agent_of<S: Host>(s: &mut S) -> &mut agent::State {
    &mut s.shell().agent
}

fn finder_of<S: Host>(s: &mut S) -> &mut finder::State {
    &mut s.shell().finder
}

/// The viewer pane: the agent's tabs, the strips, and the tree. The source and diff screens go
/// beside the tree once they are ported.
fn viewer<S: Host>(shell: &Shell, host: &S, k: &Kit) -> El {
    // The tree's own mode says which of the two panes goes under it.
    let content = if shell.tabs_feed.value.shown.is_some() {
        // A tab of the agent's: its document or form takes the place of the files.
        return col()
            .w_full()
            .h_full()
            .min_h(0.0)
            .child(tabs::view::<S>(&shell.tabs_feed.value, k))
            .child(col().w_full().grow(1.0).min_h(0.0).child(agent::view::<S>(&shell.agent_feed.value, &shell.agent, k, agent_of::<S>)));
    } else if shell.explorer_feed.value.changes {
        diff::view::<S>(&shell.diff_feed.value, host, k, diff_ref::<S>, diff_mut::<S>)
    } else {
        source::view::<S>(&shell.source_feed.value, &shell.source, k)
    };
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(tabs::view::<S>(&shell.tabs_feed.value, k))
        .child(explorer::strips::<S>(&shell.strip_feed.value, &shell.explorer, k, explorer_of::<S>))
        .child(col().w_full().grow(1.0).min_h(0.0).child(explorer::tree::<S>(&shell.explorer_feed.value, k)))
        .child(col().w_full().grow(2.0).min_h(0.0).child(content))
}

/// The whole window.
pub fn view<S: Host>(host: &S, k: &Kit) -> El {
    let shell = host.shell_ref();
    let main = match (&shell.page, &shell.settings) {
        _ if shell.finder_open => finder::view::<S>(&shell.finder_feed.value, &shell.finder, k, finder_of::<S>),
        (Page::Settings, Some(page)) => settings::view::<S>(page, k, settings_of::<S>),
        _ => terminal::view::<S>(&shell.term, k, term_of::<S>),
    };
    SplitView::new(main)
        .sidebar::<S>(&shell.sidebar, sidebar_of::<S>, sidebar::view::<S>(&shell.sidebar_feed.value, &shell.side, k, side_of::<S>))
        .inspector::<S>(&shell.viewer, viewer_of::<S>, viewer::<S>(shell, host, k))
        .build(k)
}
