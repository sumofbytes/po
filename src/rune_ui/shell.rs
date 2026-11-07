//! The window: a sidebar, the main pane and a viewer pane, as Rune's `SplitView` lays them out.
//! Counterpart of the layout half of `Reader::render` in `main.rs`. What fills the panes comes from
//! the screens; this decides what is shown where.
use rune::{El, Tx, col};
use rune_kit::{Kit, PaneState, SplitView};

use super::{agent, chat, diff, explorer, finder, home, issues, settings, sidebar, source, tabs, terminal};
use crate::issues::shared;
use crate::model;
use crate::model::bus::{Cmd, send};
use crate::reader::render::Layout;

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

pub struct Shell {
    pub sidebar: PaneState,
    pub viewer: PaneState,
    pub settings: Option<settings::Page>,
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
    pub chat_feed: Feed<model::chat::Snapshot>,
    pub chat: chat::State,
    pub home: home::State,
    pub home_model: model::home::Model,
    pub issues: issues::State,
    overview_feed: Feed<crate::issues::shared::Overview>,
    pub finder: finder::State,
    finder_was_open: bool,
    pub finder_feed: Feed<model::finder::Snapshot>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            sidebar: PaneState::new(260.0).range(180.0, 420.0),
            viewer: PaneState::new(460.0).range(280.0, 900.0).collapsed(),
            settings: None,
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
            chat_feed: Feed::new(model::chat::watch()),
            chat: chat::State::default(),
            home: home::State::default(),
            home_model: Default::default(),
            issues: issues::State::default(),
            overview_feed: Feed::new(crate::issues::shared::overview()),
            finder: finder::State::default(),
            finder_was_open: false,
            finder_feed: Feed::new(model::finder::watch()),
        }
    }
}

impl Shell {
    /// Follows the layout the `Reader` has: the panes that are open, the settings page that is up.
    pub fn follow(&mut self, layout: &Layout, viewer_left: bool) {
        self.sidebar.set_open(layout.left_open);
        // The viewer is the right pane unless it fills the middle or the conversation is.
        self.viewer.set_open(layout.side_open && layout.agent_open && !layout.home_open && !layout.issues_open);
        if layout.settings_open && self.settings.is_none() {
            self.settings = Some(settings::Page::load(viewer_left));
        }
        if !layout.settings_open {
            self.settings = None;
        }
        if layout.finder_open && !self.finder_was_open {
            self.finder.query = rune::TextEdit::new("");
            self.finder.query.focused = true;
            self.finder.at = 0;
        }
        self.finder_was_open = layout.finder_open;
        if layout.issues_open {
            if let Some(nav) = shared::take_nav() {
                self.issues.go_to(nav, &self.overview_feed.value);
            }
        }
    }

    /// Takes what the `Reader` published since the last call; true when the view is stale.
    pub fn poll(&mut self) -> bool {
        let mut stale = self.sidebar_feed.poll();
        let overview = self.overview_feed.poll();
        if stale || overview {
            self.home_model = model::home::model(&self.overview_feed.value, &self.sidebar_feed.value, crate::date::today());
        }
        stale |= overview;
        stale |= self.issues.tick(&self.overview_feed.value);
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
        if self.chat_feed.poll() {
            self.chat.follow(&self.chat_feed.value);
            stale = true;
        }
        self.explorer.follow(&self.strip_feed.value);
        stale
    }

    /// Takes what a screen asked of the shell: a settings page that saved or closed says so to the
    /// `Reader`, which puts it away.
    pub fn settle(&mut self) {
        if let Some(outcome) = self.settings.as_mut().and_then(|page| page.outcome.take()) {
            if outcome == settings::Outcome::Saved {
                send(Cmd::SettingsSaved);
            }
            send(Cmd::CloseSettings);
        }
    }
}

/// The state of the app a [`Shell`] lives in: handlers reach it through this.
pub trait Host: 'static {
    fn shell(&mut self) -> &mut Shell;
    fn shell_ref(&self) -> &Shell;
    /// What the window shows and where.
    fn layout(&self) -> Layout;
    /// The shell of the project on screen, if it has started one.
    fn terminal(&self) -> Option<&terminal::Term>;
    fn terminal_mut(&mut self) -> Option<&mut terminal::Term>;
    /// The window has, or has lost, the user's attention.
    fn set_active(&mut self, active: bool);
    /// Files dropped on the window, to attach to the message being written.
    fn attach(&mut self, paths: Vec<std::path::PathBuf>);
}

fn sidebar_of<S: Host>(s: &mut S) -> &mut PaneState {
    &mut s.shell().sidebar
}

fn viewer_of<S: Host>(s: &mut S) -> &mut PaneState {
    &mut s.shell().viewer
}

fn term_of<S: Host>(s: &mut S) -> &mut terminal::Term {
    s.terminal_mut().expect("the terminal is drawn only where there is one")
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

fn chat_of<S: Host>(s: &mut S) -> &mut chat::State {
    &mut s.shell().chat
}

fn issues_of<S: Host>(s: &mut S) -> &mut issues::State {
    &mut s.shell().issues
}

fn home_of<S: Host>(s: &mut S) -> &mut home::State {
    &mut s.shell().home
}

fn finder_of<S: Host>(s: &mut S) -> &mut finder::State {
    &mut s.shell().finder
}

/// The viewer pane: the agent's tabs, then what the viewer shows: the terminal, an agent's own
/// view, or the strips and the tree with the file or the diff beside it.
fn viewer<S: Host>(shell: &Shell, host: &S, layout: &Layout, k: &Kit) -> El {
    let tabs_bar = tabs::view::<S>(&shell.tabs_feed.value, k);
    if layout.terminal_shown {
        let body = match host.terminal() {
            Some(term) => terminal::view::<S>(term, k, term_of::<S>),
            None => col(),
        };
        return col().w_full().h_full().min_h(0.0).child(tabs_bar).child(agent::view::<S>(&shell.agent_feed.value, &shell.agent, k, agent_of::<S>)).child(col().w_full().grow(1.0).min_h(0.0).child(body));
    }
    if layout.showing_agent {
        return col().w_full().h_full().min_h(0.0).child(tabs_bar).child(col().w_full().grow(1.0).min_h(0.0).child(agent::view::<S>(&shell.agent_feed.value, &shell.agent, k, agent_of::<S>)));
    }
    let content = if layout.diff { diff::view::<S>(&shell.diff_feed.value, host, k, diff_ref::<S>, diff_mut::<S>) } else { source::view::<S>(&shell.source_feed.value, &shell.source, k) };
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(tabs_bar)
        .child(explorer::strips::<S>(&shell.strip_feed.value, &shell.explorer, k, explorer_of::<S>))
        .child(col().w_full().grow(1.0).min_h(0.0).child(explorer::tree::<S>(&shell.explorer_feed.value, k)))
        .child(col().w_full().grow(2.0).min_h(0.0).child(content))
}

fn placeholder(k: &Kit, what: &str) -> El {
    col().w_full().h_full().center().child(rune::text(what.to_string()).color(k.c.text_muted))
}

/// The whole window, laid out as the `Reader` says: the sidebar, a middle pane and a right pane
/// that are the conversation and the viewer, either way round, or a page (Home, the issues, the
/// settings, the finder) in place of the conversation.
pub fn view<S: Host>(host: &S, k: &Kit) -> El {
    let shell = host.shell_ref();
    let layout = host.layout();
    let chat = || chat::view::<S>(&shell.chat_feed.value, &shell.chat, k, chat_of::<S>);
    let pane = || viewer::<S>(shell, host, &layout, k);
    let page = if layout.finder_open {
        Some(finder::view::<S>(&shell.finder_feed.value, &shell.finder, k, finder_of::<S>))
    } else if let (true, Some(page)) = (layout.settings_open, &shell.settings) {
        Some(settings::view::<S>(page, k, settings_of::<S>))
    } else if layout.home_open {
        Some(home::view::<S>(&shell.home_model, &shell.home, k, home_of::<S>))
    } else if layout.issues_open {
        Some(issues::view::<S>(&shell.overview_feed.value, &shell.issues, k, issues_of::<S>))
    } else {
        None
    };
    // The viewer is the middle pane when it is swapped with the conversation or fills the window.
    let focus = layout.side_open && !layout.agent_open;
    let (middle, right) = match page {
        Some(page) => (page, None),
        None if !layout.side_open => (chat(), None),
        None if focus => (pane(), None),
        None if layout.swapped => (pane(), Some(chat())),
        None => (chat(), Some(pane())),
    };
    let split = SplitView::new(middle).sidebar::<S>(&shell.sidebar, sidebar_of::<S>, sidebar::view::<S>(&shell.sidebar_feed.value, &shell.side, k, side_of::<S>));
    let root = match right {
        Some(right) => split.inspector::<S>(&shell.viewer, viewer_of::<S>, right).build(k),
        None => split.build(k),
    };
    // Text pasted while no field has the keyboard goes to the terminal, where it is showing.
    root.on_drop_files(|s: &mut S, paths: &[std::path::PathBuf], _: &mut Tx| s.attach(paths.to_vec())).on_window_focus(|s: &mut S, focused: bool, _: &mut Tx| s.set_active(focused)).on_paste(|s: &mut S, text: &str, _: &mut Tx| {
        if s.layout().terminal_shown
            && let Some(term) = s.terminal_mut()
        {
            term.paste(text);
        }
    })
}
