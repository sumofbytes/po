//! The Rune port of Po. `cargo run --bin po-rune` opens the window; `-- --snap po.png` renders it
//! headless. It replaces the gpui `po` binary once the screens are all ported.

#![recursion_limit = "1024"]

use rune::{AppSpec, Cx, El, Tx, Waker};
use rune_kit::Kit;

#[allow(dead_code)]
#[path = "../terminal.rs"]
mod terminal;

#[allow(dead_code)]
#[path = "../rune_ui/mod.rs"]
mod rune_ui;
#[allow(dead_code)]
#[path = "../explorer.rs"]
mod explorer;
#[allow(dead_code)]
#[path = "../model/mod.rs"]
mod model;
#[allow(dead_code)]
#[path = "../schemes.rs"]
mod schemes;

#[allow(dead_code)]
#[path = "../config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../settings.rs"]
mod settings;
/// Only the names the settings page reads: the gpui `fonts` module can't come along. The Rune
/// port replaces it.
mod fonts {
    pub const SIZES: std::ops::RangeInclusive<f32> = 8.0..=48.0;
    pub const BUNDLED_UI_FAMILY: &str = "Geist";
    pub const BUNDLED_MONO_FAMILY: &str = "Geist Mono";
}

use rune_ui::shell::{Host, Page, Shell};

struct State {
    kit: Kit,
    shell: Shell,
    /// What the screens ask for, until the `Reader` is on Rune to run it.
    commands: Option<model::bus::Commands>,
    /// Say each command on stderr (`PO_RUNE_SCREEN=demo`).
    echo: bool,
}

/// A made-up sidebar, for looking at the screen before the `Reader` publishes a real one.
fn demo() {
    use model::sidebar::{Mark, Project, Snapshot, Suggestion, Thread, Waiting};
    let thread = |id, label: &str, mark, selected| Thread { project: 1, id, label: label.into(), name: label.into(), mark, selected, controlled: false, ago: Some("5m".into()), closing: false };
    model::sidebar::publish(Snapshot {
        waiting: vec![Waiting { thread: Thread { project: 2, ..thread(30, "Ship the release notes", Mark::Asking, false) }, project: "docs".into() }],
        projects: vec![
            Project {
                id: 1,
                name: "po".into(),
                active: true,
                collapsed: false,
                closable: true,
                threads: vec![thread(10, "Port the sidebar", Mark::Running, true), thread(11, "Fix the diff header", Mark::Unread, false), thread(12, "Retry bug", Mark::Idle, false)],
                fold: Some((false, 4)),
            },
            Project { id: 2, name: "docs".into(), active: false, collapsed: true, closable: true, threads: vec![], fold: None },
        ],
        suggestions: vec![Suggestion { id: 7, text: "\u{201c}Claude\u{201d} suggests docs".into(), yes: "Go" }],
        status: Some("Worktree made on feat/rune".into()),
        issues: (12, 2),
        ..Default::default()
    });
    use model::explorer::{Body, Entry};
    let entry = |id: &str, name: &str, depth, folder, expanded, selected, mark| Entry { row: explorer::Row { id: id.into(), name: name.into(), depth, folder, expanded, selected }, mark };
    model::explorer::publish(model::explorer::Snapshot {
        changes: true,
        body: Body::Rows(vec![
            entry("src", "src", 0, true, true, false, None),
            entry("src/main.rs", "main.rs", 1, false, false, true, Some('M')),
            entry("src/host.rs", "host.rs", 1, false, false, false, Some('A')),
            entry("docs", "docs", 0, true, false, false, None),
            entry("old.txt", "old.txt", 0, false, false, false, Some('D')),
        ]),
        ..Default::default()
    });
    model::strip::publish(model::strip::Snapshot {
        worktree: Some(model::strip::Worktree { branch: "feat/rune".into(), confirm_discard: false }),
        pr: Some(model::strip::Pr::Asking { status: None, opening: false }),
        pr_reset: 0,
    });
    use model::tabs::Tab;
    model::tabs::publish(model::tabs::Snapshot {
        shown: Some("plan".into()),
        tabs: vec![Tab { id: "plan".into(), title: "Plan".into() }, Tab { id: "notes".into(), title: "Notes".into() }],
        offer: Some(Tab { id: "risks".into(), title: "Risks".into() }),
        controls: Default::default(),
    });
    model::finder::publish(model::finder::Snapshot { matches: vec!["src/main.rs".into(), "src/rune_ui/shell.rs".into(), "docs/rune-port.md".into()], capped: None });
}

impl Host for State {
    fn shell(&mut self) -> &mut Shell {
        &mut self.shell
    }
}

impl Default for State {
    fn default() -> Self {
        let name = std::env::var("PO_THEME").ok().or_else(|| config::load()["theme"].as_str().map(String::from)).unwrap_or_else(|| rune_ui::theme::DEFAULT.into());
        let (kit, _) = rune_ui::theme::kit(&rune_ui::theme::load(&name));
        let mut shell = Shell::default();
        // `PO_RUNE_SCREEN=settings` opens on the settings page, for looking at it headless.
        if std::env::var("PO_RUNE_SCREEN").as_deref() == Ok("settings") {
            shell.show(Page::Settings);
        }
        let echo = std::env::var("PO_RUNE_SCREEN").as_deref() == Ok("demo");
        if echo {
            demo();
            shell.viewer.set_open(true);
        }
        Self { kit, shell, commands: model::bus::commands(), echo }
    }
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    rune_ui::shell::view::<State>(&s.shell, &s.kit)
}

fn setup(s: &mut State, wake: Waker) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    s.shell.term.start(&cwd, &s.kit, wake);
}

/// True when something the view shows changed on its own: the shell drew, or a page closed.
fn poll(s: &mut State, _: f64, _: &mut Tx) -> bool {
    let mut asked = false;
    while let Some(cmd) = s.commands.as_mut().and_then(|c| c.try_recv().ok()) {
        if s.echo {
            eprintln!("cmd: {cmd:?}");
        }
        asked = true;
        match cmd {
            model::bus::Cmd::OpenSettings => s.shell.show(Page::Settings),
            model::bus::Cmd::CloseSettings => s.shell.show(Page::Terminal),
            model::bus::Cmd::CloseFinder | model::bus::Cmd::FinderPick(_) => s.shell.finder_open = false,
            _ => {}
        }
    }
    let drew = s.shell.term.poll() | s.shell.poll() | asked;
    let page = s.shell.page;
    s.shell.settle();
    drew || page != s.shell.page
}

fn on_key(s: &mut State, event: &rune::KeyEvent, _: &mut Tx) -> bool {
    // Ctrl+P is the finder.
    if event.is_command('p') {
        s.shell.finder_open = true;
        s.shell.finder.query.focused = true;
        return true;
    }
    s.shell.key(event)
}

fn main() {
    rune::run(AppSpec::<State> {
        // Mono spans draw in the mono slot: point it at the system's monospace font.
        families: ("Inter", "ui-monospace"),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        ..AppSpec::new("Po", (1200.0, 760.0), State::default, view)
    });
}
