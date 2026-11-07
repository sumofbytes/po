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
        Self { kit, shell }
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
    let drew = s.shell.term.poll();
    let page = s.shell.page;
    s.shell.settle();
    drew || page != s.shell.page
}

fn on_key(s: &mut State, event: &rune::KeyEvent, _: &mut Tx) -> bool {
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
