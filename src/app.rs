//! The Rune host of the [`crate::Reader`]: the window, its panes, and the loop that gives the
//! reader what happens (tasks finishing, commands the screens send, keys) and the screens what the
//! reader publishes.
use std::sync::Mutex;
use std::time::Instant;

use rune::{AppSpec, Cx, El, Key, KeyEvent, Tx, Waker};
use rune_kit::Kit;

use crate::host::{self, Inbox, Wake};
use crate::model::bus::{self, Cmd};
use crate::reader::render::Layout;
use crate::rt::{Context, Core, Window};
use crate::rune_ui::shell::{Host, Shell};
use crate::{Prefetch, Reader, config, pr, rune_ui};

/// What `main` parsed: what the first window opens.
struct Launch {
    root: std::path::PathBuf,
    pr: Option<pr::Pr>,
    command: Option<String>,
    prefetch: Prefetch,
}

static LAUNCH: Mutex<Option<Launch>> = Mutex::new(None);

/// The reader and what drives it, once the window is there to wake.
struct Live {
    core: Core,
    host: host::Host<Core>,
    inbox: Inbox<Core>,
    commands: Option<bus::Commands>,
}

struct State {
    kit: Kit,
    shell: Shell,
    live: Option<Live>,
    /// The viewer is on the left in the settings, for the page that edits it.
    viewer_left: bool,
    /// The terminal was what the viewer showed, as of the last poll: it takes the keyboard on opening.
    terminal_was_shown: bool,
    /// The saved pane widths were handed to the panes.
    widths_applied: bool,
}

impl Host for State {
    fn shell(&mut self) -> &mut Shell {
        &mut self.shell
    }

    fn shell_ref(&self) -> &Shell {
        &self.shell
    }

    fn layout(&self) -> Layout {
        self.live.as_ref().map(|live| live.core.reader.layout()).unwrap_or_default()
    }

    fn terminal(&self) -> Option<&rune_ui::terminal::Term> {
        self.live.as_ref().and_then(|live| live.core.reader.project.terminal.as_ref())
    }

    fn attach(&mut self, paths: Vec<std::path::PathBuf>) {
        if let Some(Live { core, host, .. }) = self.live.as_mut() {
            core.reader.attach(paths.into_iter().map(crate::Source::Path).collect(), &mut Context::new(host.clone()));
        }
    }

    fn set_active(&mut self, active: bool) {
        if let Some(live) = self.live.as_mut() {
            live.core.window.active = active;
        }
    }

    fn terminal_mut(&mut self) -> Option<&mut rune_ui::terminal::Term> {
        self.live.as_mut().and_then(|live| live.core.reader.project.terminal.as_mut())
    }
}

fn init() -> State {
    let settings = config::load();
    let name = std::env::var("PO_THEME").ok().or_else(|| settings["theme"].as_str().map(String::from)).unwrap_or_else(|| rune_ui::theme::DEFAULT.into());
    let (kit, _) = rune_ui::theme::kit(&rune_ui::theme::load(&name));
    State { kit, shell: Shell::default(), live: None, viewer_left: config::viewer_left(&settings).unwrap_or(false), terminal_was_shown: false, widths_applied: false }
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    rune_ui::shell::view::<State>(s, &s.kit)
}

/// Builds the reader, now that the window can be woken from other threads.
fn setup(s: &mut State, waker: Waker) {
    // Pictures: the cache that decodes the model's `data:` URIs, awake with the runtime.
    rune_ui::pictures::set(waker.clone());
    let Some(launch) = LAUNCH.lock().ok().and_then(|mut launch| launch.take()) else { return };
    let wake: Wake = waker;
    let (host, inbox) = host::host::<Core>(wake);
    let mut window = Window::new(host.clone());
    let mut cx = Context::new(host.clone());
    let reader = Reader::new(launch.root, launch.pr, launch.command, launch.prefetch, &mut window, &mut cx);
    let mut core = Core { reader, window };
    core.reader.term_theme = terminal_colors(&s.kit);
    s.live = Some(Live { core, host, inbox, commands: bus::commands() });
}

fn terminal_colors(kit: &Kit) -> (crate::terminal::Rgb, crate::terminal::Rgb) {
    let rgb = |c: rune::Color| {
        let hex = c.hex();
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
        crate::terminal::Rgb(byte(1), byte(3), byte(5))
    };
    (rgb(kit.c.text_base), rgb(kit.c.bg_base))
}

/// Between frames: what tasks asked of the reader, what the screens sent, what the reader asks of
/// the window; then what the reader publishes goes to the screens. True when the view is stale.
fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
    let Some(live) = s.live.as_mut() else { return false };
    let Live { core, host, inbox, commands } = live;
    let mut stale = inbox.run(core);
    let mut cx = Context::new(host.clone());
    while let Some(cmd) = commands.as_mut().and_then(|commands| commands.try_recv().ok()) {
        core.reader.run(cmd, &mut core.window, &mut cx);
        stale = true;
    }
    core.reader.reap_terminals(&mut cx);
    core.reader.flush_save();
    core.reader.publish_screens();
    if let Some(term) = core.reader.project.terminal.as_mut() {
        stale |= term.poll();
    }
    let window = &mut core.window;
    for url in window.urls.drain(..) {
        tx.open_url(&url).ok();
    }
    if std::mem::take(&mut window.focus_asked) {
        s.shell.chat.draft.focused = true;
    }
    if std::mem::take(&mut window.close_asked) {
        tx.close_window();
    }
    if std::mem::take(&mut window.folder_asked) {
        tx.open_folder(Default::default(), |s: &mut State, picked, _| {
            let Some(root) = picked.ok().flatten().and_then(|path| path.canonicalize().ok()) else { return };
            if let Some(Live { core, host, .. }) = s.live.as_mut() {
                core.reader.add_project(root, None, &mut core.window, &mut Context::new(host.clone()));
            }
        });
    }
    let layout = core.reader.layout();
    if layout.terminal_shown && !s.terminal_was_shown {
        tx.blur();
    }
    s.terminal_was_shown = layout.terminal_shown;
    s.shell.follow(&layout, s.viewer_left);
    // The panes start as the workspace left them, and what they are dragged to is saved.
    let widths = &mut core.reader.widths;
    if !s.widths_applied {
        s.widths_applied = true;
        if let Some(width) = widths.left {
            s.shell.sidebar.set_width(width);
        }
        if let Some(width) = widths.right {
            s.shell.viewer.set_width(width);
        }
    } else {
        let now = crate::Widths { left: Some(s.shell.sidebar.width()), right: Some(s.shell.viewer.width()), explorer: widths.explorer };
        let dragging = s.shell.sidebar.is_resizing() || s.shell.viewer.is_resizing();
        if !dragging && now != *widths && widths.left.is_some() | widths.right.is_some() | (now.left != Some(s.shell.sidebar.default_width())) | (now.right != Some(s.shell.viewer.default_width())) {
            *widths = now;
            core.reader.request_save(&mut cx);
        }
    }
    s.shell.settle();
    stale | s.shell.poll()
}

/// The window's shortcuts, which the focused control did not take. Where the terminal is what the
/// viewer shows, the keys of readline, fzf and tmux (Ctrl+O, T, B, N, P, W and \) go to its shell.
fn on_key(s: &mut State, event: &KeyEvent, _: &mut Tx) -> bool {
    let Some(live) = s.live.as_mut() else { return false };
    let Live { core, host, .. } = live;
    core.reader.last_input = Instant::now();
    let terminal_has_keys = core.reader.layout().terminal_shown && core.reader.project.terminal.is_some();
    let readline = ['o', 't', 'b', 'n', 'p', 'w', '\\'].into_iter().any(|c| event.is_command(c)) && !event.mods.shift;
    if terminal_has_keys && readline {
        return core.reader.project.terminal.as_ref().is_some_and(|term| term.key(event));
    }
    let mut cx = Context::new(host.clone());
    let (reader, window) = (&mut core.reader, &mut core.window);
    let shift = event.mods.shift;
    let handled = match &event.key {
        Key::Escape if reader.layout().settings_open => {
            reader.hide_settings(window, &mut cx);
            true
        }
        _ if event.is_command('o') => {
            reader.toggle_tools(&mut cx);
            true
        }
        _ if event.is_command('t') => {
            reader.toggle_thoughts(&mut cx);
            true
        }
        _ if event.is_command('b') && shift => {
            reader.toggle_right(&mut cx);
            true
        }
        _ if event.is_command('b') => {
            reader.toggle_left(&mut cx);
            true
        }
        _ if event.is_command('1') => {
            reader.show_viewer(crate::Mode::Source, &mut cx);
            true
        }
        _ if event.is_command('2') => {
            reader.show_viewer(crate::Mode::Diff, &mut cx);
            true
        }
        _ if event.is_command('3') => {
            reader.show_chat(&mut cx);
            true
        }
        _ if event.is_command('s') && shift => {
            reader.swap_panes(&mut cx);
            true
        }
        _ if event.is_command('f') && shift => {
            reader.toggle_focus(&mut cx);
            true
        }
        _ if event.is_command('h') && shift => {
            reader.resume_thread(window, &mut cx);
            true
        }
        _ if event.is_command('\\') => {
            let split = !reader.split;
            reader.set_split(split, &mut cx);
            true
        }
        _ if event.is_command('m') && shift => {
            reader.toggle_rendered(&mut cx);
            true
        }
        _ if event.is_command('n') => {
            reader.hide_issues(window, &mut cx);
            reader.new_blank_thread(window, &mut cx);
            true
        }
        _ if event.is_command('o') && shift => {
            reader.hide_issues(window, &mut cx);
            reader.pick_project(window, &mut cx);
            true
        }
        _ if event.is_command('p') => {
            reader.hide_issues(window, &mut cx);
            reader.show_finder(window, &mut cx);
            true
        }
        _ if event.is_command(',') => {
            reader.hide_issues(window, &mut cx);
            reader.toggle_settings(window, &mut cx);
            true
        }
        _ if event.is_command('w') => {
            window.remove_window();
            true
        }
        _ if event.is_command('`') => {
            reader.toggle_terminal(window, &mut cx);
            true
        }
        _ => false,
    };
    if handled {
        return true;
    }
    // Whatever else is typed goes to the shell the viewer shows, if it shows one.
    terminal_has_keys && core.reader.project.terminal.as_ref().is_some_and(|term| term.key(event))
}

pub fn run(root: std::path::PathBuf, pr: Option<pr::Pr>, command: Option<String>, prefetch: Prefetch) {
    *LAUNCH.lock().expect("launch") = Some(Launch { root, pr, command, prefetch });
    let settings = config::load();
    // The fonts the settings name, as a list the text system tries in order, over the bundled ones.
    let ui: &'static str = Box::leak(settings["font"]["family"].as_str().map_or_else(|| format!("{}, system-ui", crate::fonts::BUNDLED_UI_FAMILY), |f| format!("{f}, {}, system-ui", crate::fonts::BUNDLED_UI_FAMILY)).into_boxed_str());
    let mono: &'static str = Box::leak(settings["font"]["mono_family"].as_str().map_or_else(|| format!("{}, ui-monospace", crate::fonts::BUNDLED_MONO_FAMILY), |f| format!("{f}, {}, ui-monospace", crate::fonts::BUNDLED_MONO_FAMILY)).into_boxed_str());
    rune::set_app_id("po");
    rune::run(AppSpec::<State> {
        fonts: &crate::fonts::BUNDLED,
        families: (ui, mono),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        min_size: Some((640.0, 420.0)),
        ..AppSpec::new("Po", (1360.0, 840.0), init, view)
    });
}
