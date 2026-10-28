//! A shell in a pseudo-terminal, emulated by libghostty-vt. [`Session`] runs the shell and hands out
//! [`Frame`]s: plain data (runs of styled text, the cursor) that the view paints, so nothing that
//! belongs to libghostty crosses a thread. The emulator is not `Send`, so it lives on one thread of
//! its own, which also encodes the keys and answers the shell's queries.
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The least time between two frames while output is streaming in.
const FRAME: Duration = Duration::from_millis(8);
const SCROLLBACK: usize = 10_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Consecutive cells of a row that share a style.
#[derive(Clone, PartialEq, Debug)]
pub struct Run {
    pub col: u16,
    /// Width in cells.
    pub cells: u16,
    pub text: String,
    pub fg: Rgb,
    /// None is the terminal's own background.
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Hollow,
    Bar,
    Underline,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Vec<Run>>,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Option<Cursor>,
    /// What the shell set as its title.
    pub title: String,
    /// The shell has exited, with its status.
    pub exited: Option<i32>,
}

impl Default for Frame {
    fn default() -> Self {
        Self { cols: 0, rows: 0, lines: vec![], background: Rgb(0, 0, 0), foreground: Rgb(255, 255, 255), cursor: None, title: String::new(), exited: None }
    }
}

/// What the user did in the terminal.
#[derive(Clone, Debug)]
pub struct KeyInput {
    pub key: Key,
    /// The text the key types, if it types any.
    pub text: Option<String>,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
}

pub use libghostty_vt::key::Key;

enum Msg {
    Output(Vec<u8>),
    Key(KeyInput),
    Paste(String),
    Resize { cols: u16, rows: u16, cell: (u32, u32) },
    Scroll(isize),
    /// The default foreground and background.
    Theme(Rgb, Rgb),
    Exited(i32),
    Stop,
}

/// A running shell. Dropping it stops the shell.
pub struct Session {
    tx: Sender<Msg>,
    frame: Arc<Mutex<Arc<Frame>>>,
    pty: pty::Pty,
}

impl Session {
    /// Starts `$SHELL` in `cwd`. `wake` is called, from another thread, whenever there is a new frame.
    pub fn spawn(cwd: &Path, cols: u16, rows: u16, theme: (Rgb, Rgb), wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let pty = pty::Pty::spawn(cwd, cols, rows)?;
        let (tx, rx) = mpsc::channel();
        let frame = Arc::new(Mutex::new(Arc::new(Frame::default())));

        let reader = pty.reader()?;
        let output = tx.clone();
        std::thread::Builder::new().name("terminal-read".into()).spawn({
            let pty = pty.clone();
            move || {
                use std::io::Read as _;
                let mut reader = reader;
                let mut buf = vec![0; 64 * 1024];
                // A closed pty reads as an error on Linux, and as the end elsewhere.
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 || output.send(Msg::Output(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                let _ = output.send(Msg::Exited(pty.wait()));
            }
        })?;

        let writer = pty.writer()?;
        let shared = frame.clone();
        std::thread::Builder::new().name("terminal".into()).spawn(move || emulate(rx, writer, shared, (cols, rows), theme, wake))?;
        Ok(Self { tx, frame, pty })
    }

    /// The latest frame.
    pub fn frame(&self) -> Arc<Frame> {
        self.frame.lock().map(|f| f.clone()).unwrap_or_default()
    }

    pub fn key(&self, key: KeyInput) {
        let _ = self.tx.send(Msg::Key(key));
    }

    pub fn paste(&self, text: String) {
        let _ = self.tx.send(Msg::Paste(text));
    }

    /// Scrolls the history by `lines`; up is negative.
    pub fn scroll(&self, lines: isize) {
        let _ = self.tx.send(Msg::Scroll(lines));
    }

    pub fn theme(&self, foreground: Rgb, background: Rgb) {
        let _ = self.tx.send(Msg::Theme(foreground, background));
    }

    /// The window is now `cols` by `rows` cells of `cell` pixels.
    pub fn resize(&self, cols: u16, rows: u16, cell: (u32, u32)) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.pty.resize(cols, rows);
        let _ = self.tx.send(Msg::Resize { cols, rows, cell });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        self.pty.kill();
    }
}

fn emulate(rx: mpsc::Receiver<Msg>, writer: impl std::io::Write + Send + 'static, shared: Arc<Mutex<Arc<Frame>>>, (cols, rows): (u16, u16), theme: (Rgb, Rgb), wake: impl Fn()) {
    use libghostty_vt::key::{Action, Encoder, Event, Mods};
    use libghostty_vt::terminal::ScrollViewport;
    use libghostty_vt::{RenderState, Terminal, TerminalOptions};

    let Ok(mut terminal) = Terminal::new(TerminalOptions { cols, rows, max_scrollback: SCROLLBACK }) else {
        log::error!("terminal: libghostty could not start");
        return;
    };
    // The shell's queries (cursor position, device attributes) are answered through the same pipe
    // the keys go down.
    let writer = Arc::new(Mutex::new(writer));
    let reply = writer.clone();
    let _ = terminal.on_pty_write(move |_, data| {
        if let Ok(mut out) = reply.lock() {
            let _ = out.write_all(data).and_then(|()| out.flush());
        }
    });
    let title = Arc::new(Mutex::new(String::new()));
    let named = title.clone();
    let _ = terminal.on_title_changed(move |terminal| {
        if let (Ok(mut name), Ok(now)) = (named.lock(), terminal.title()) {
            *name = now.to_string();
        }
    });
    let colors = |terminal: &mut Terminal<'_, '_>, (fg, bg): (Rgb, Rgb)| {
        use libghostty_vt::style::RgbColor;
        let _ = terminal.set_default_fg_color(Some(RgbColor { r: fg.0, g: fg.1, b: fg.2 }));
        let _ = terminal.set_default_bg_color(Some(RgbColor { r: bg.0, g: bg.1, b: bg.2 }));
    };
    colors(&mut terminal, theme);
    let (Ok(mut render), Ok(mut encoder), Ok(mut event)) = (RenderState::new(), Encoder::new(), Event::new()) else {
        log::error!("terminal: libghostty could not start");
        return;
    };
    let send = |bytes: &[u8]| {
        if let Ok(mut out) = writer.lock() {
            let _ = out.write_all(bytes).and_then(|()| out.flush());
        }
    };

    let (mut dirty, mut last, mut exited) = (true, Instant::now() - FRAME, None);
    let mut keys = Vec::new();
    loop {
        let message = if dirty {
            match rx.recv_timeout(FRAME.saturating_sub(last.elapsed())) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            }
        };
        let timed_out = message.is_none();
        if let Some(message) = message {
            dirty = true;
            match message {
                Msg::Output(bytes) => terminal.vt_write(&bytes),
                Msg::Key(input) => {
                    // Typing scrolls back to where the shell is.
                    let mut mods = Mods::empty();
                    for (on, flag) in [(input.shift, Mods::SHIFT), (input.alt, Mods::ALT), (input.ctrl, Mods::CTRL), (input.meta, Mods::SUPER)] {
                        if on {
                            mods |= flag;
                        }
                    }
                    event.set_action(Action::Press).set_key(input.key).set_mods(mods).set_utf8(input.text.as_deref().filter(|t| !t.is_empty()));
                    if let Some(c) = input.text.as_deref().and_then(|t| t.chars().next()).filter(|c| c.is_alphanumeric()) {
                        event.set_unshifted_codepoint(c.to_lowercase().next().unwrap_or(c));
                    }
                    encoder.set_options_from_terminal(&terminal);
                    keys.clear();
                    if encoder.encode_to_vec(&event, &mut keys).is_ok() && !keys.is_empty() {
                        terminal.scroll_viewport(ScrollViewport::Bottom);
                        send(&keys);
                    }
                }
                Msg::Paste(text) => {
                    let bracketed = terminal.mode(libghostty_vt::terminal::Mode::BRACKETED_PASTE).unwrap_or(false);
                    let mut data = text.into_bytes();
                    let mut buf = vec![0; data.len() + 16];
                    if let Ok(n) = libghostty_vt::paste::encode(&mut data, bracketed, &mut buf) {
                        terminal.scroll_viewport(ScrollViewport::Bottom);
                        send(&buf[..n]);
                    }
                }
                Msg::Resize { cols, rows, cell } => {
                    let _ = terminal.resize(cols, rows, cell.0, cell.1);
                }
                Msg::Scroll(lines) => terminal.scroll_viewport(ScrollViewport::Delta(lines)),
                Msg::Theme(fg, bg) => colors(&mut terminal, (fg, bg)),
                Msg::Exited(status) => exited = Some(status),
                Msg::Stop => break,
            }
        }
        if dirty && (timed_out || exited.is_some() || last.elapsed() >= FRAME) {
            let name = title.lock().map(|t| t.clone()).unwrap_or_default();
            if let Some(frame) = snapshot(&terminal, &mut render, name, exited) {
                if let Ok(mut slot) = shared.lock() {
                    *slot = Arc::new(frame);
                }
                wake();
            }
            (dirty, last) = (false, Instant::now());
        }
    }
}

fn snapshot<'a>(terminal: &libghostty_vt::Terminal<'a, '_>, render: &mut libghostty_vt::RenderState<'a>, title: String, exited: Option<i32>) -> Option<Frame> {
    use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
    use libghostty_vt::screen::CellWide;
    use libghostty_vt::style::{RgbColor, Underline};

    let rgb = |c: RgbColor| Rgb(c.r, c.g, c.b);
    let snapshot = render.update(terminal).ok()?;
    let colors = snapshot.colors().ok()?;
    let (background, foreground) = (rgb(colors.background), rgb(colors.foreground));
    let mut rows = RowIterator::new().ok()?;
    let mut cells = CellIterator::new().ok()?;
    let mut row_iter = rows.update(&snapshot).ok()?;
    let mut lines = Vec::new();
    while let Some(row) = row_iter.next() {
        let mut runs: Vec<Run> = Vec::new();
        let mut cell_iter = cells.update(row).ok()?;
        let mut col = 0_u16;
        while let Some(cell) = cell_iter.next() {
            let wide = cell.raw_cell().and_then(|c| c.wide()).unwrap_or(CellWide::Narrow);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                col += 1;
                continue;
            }
            let style = cell.style().unwrap_or_default();
            let (mut fg, mut bg) = (cell.fg_color().ok().flatten().map(rgb), cell.bg_color().ok().flatten().map(rgb));
            if style.inverse {
                (fg, bg) = (Some(bg.unwrap_or(background)), Some(fg.unwrap_or(foreground)));
            }
            let mut fg = fg.unwrap_or(foreground);
            if style.faint {
                let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
                fg = Rgb(mix(fg.0, background.0), mix(fg.1, background.1), mix(fg.2, background.2));
            }
            let mut text = String::new();
            if !style.invisible {
                let mut utf8 = String::new();
                if cell.graphemes_utf8(&mut utf8).is_ok() {
                    text = utf8;
                }
            }
            if text.is_empty() {
                text.push(' ');
            }
            let width = if wide == CellWide::Wide { 2 } else { 1 };
            let (underline, strike) = (style.underline != Underline::None, style.strikethrough);
            match runs.last_mut() {
                Some(run) if run.col + run.cells == col && (run.fg, run.bg, run.bold, run.italic, run.underline, run.strike) == (fg, bg, style.bold, style.italic, underline, strike) => {
                    run.text.push_str(&text);
                    run.cells += width;
                }
                _ => runs.push(Run { col, cells: width, text, fg, bg, bold: style.bold, italic: style.italic, underline, strike }),
            }
            col += width;
        }
        // Blank stretches in the default background are what the terminal's own background shows.
        runs.retain(|run| run.bg.is_some() || run.underline || run.strike || run.text.chars().any(|c| c != ' '));
        lines.push(runs);
    }
    let cursor = snapshot
        .cursor_visible()
        .ok()
        .filter(|visible| *visible)
        .and_then(|_| snapshot.cursor_viewport().ok().flatten())
        .map(|at| Cursor {
            col: at.x,
            row: at.y,
            shape: match snapshot.cursor_visual_style() {
                Ok(CursorVisualStyle::Bar) => CursorShape::Bar,
                Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
                Ok(CursorVisualStyle::BlockHollow) => CursorShape::Hollow,
                _ => CursorShape::Block,
            },
        });
    let (cols, row_count) = (snapshot.cols().ok()?, snapshot.rows().ok()?);
    // The renderer's dirty flags are never read: every frame is the whole screen.
    let _ = snapshot.set_dirty(libghostty_vt::render::Dirty::Clean);
    Some(Frame { cols, rows: row_count, lines, background, foreground, cursor, title, exited })
}

#[cfg(unix)]
mod pty {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};

    /// The master end of a pseudo-terminal and the shell on its slave.
    #[derive(Clone)]
    pub struct Pty {
        master: Arc<OwnedFd>,
        child: Arc<Mutex<Child>>,
    }

    fn size(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
    }

    impl Pty {
        pub fn spawn(cwd: &Path, cols: u16, rows: u16) -> io::Result<Self> {
            let (mut master, mut slave) = (0, 0);
            let ws = size(cols, rows);
            // SAFETY: both fds are out parameters, owned right after.
            if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &ws) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openpty just made these.
            let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
            let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
            let mut command = Command::new(&shell);
            command
                .current_dir(cwd)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .env("TERM_PROGRAM", "po")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: only async-signal-safe calls run between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    // A session of its own, with the pty as its terminal, so job control and ^C work.
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn()?;
            Ok(Self { master: Arc::new(master), child: Arc::new(Mutex::new(child)) })
        }

        fn file(&self) -> io::Result<File> {
            Ok(File::from(self.master.try_clone()?))
        }

        pub fn reader(&self) -> io::Result<File> {
            self.file()
        }

        pub fn writer(&self) -> io::Result<File> {
            self.file()
        }

        pub fn resize(&self, cols: u16, rows: u16) {
            let ws = size(cols, rows);
            // SAFETY: the master fd is open as long as `self`.
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) };
        }

        /// Waits for the shell to end: its exit status, or the signal's number.
        pub fn wait(&self) -> i32 {
            use std::os::unix::process::ExitStatusExt as _;
            // The lock is not held while waiting, or `kill` could never get in.
            loop {
                let status = self.child.lock().ok().and_then(|mut child| child.try_wait().ok().flatten());
                if let Some(status) = status {
                    return status.code().or(status.signal()).unwrap_or(0);
                }
                if self.child.lock().is_err() {
                    return 0;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        pub fn kill(&self) {
            if let Ok(mut child) = self.child.lock() {
                // The hangup is what a closing terminal sends.
                // SAFETY: a plain signal to our own child.
                unsafe { libc::kill(child.id() as i32, libc::SIGHUP) };
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[cfg(not(unix))]
mod pty {
    use std::io;
    use std::path::Path;

    #[derive(Clone)]
    pub struct Pty;

    impl Pty {
        pub fn spawn(_: &Path, _: u16, _: u16) -> io::Result<Self> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "the terminal needs a Unix pty"))
        }
        pub fn reader(&self) -> io::Result<std::fs::File> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub fn writer(&self) -> io::Result<std::fs::File> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub fn resize(&self, _: u16, _: u16) {}
        pub fn wait(&self) -> i32 {
            0
        }
        pub fn kill(&self) {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn text(frame: &Frame) -> String {
        frame.lines.iter().map(|line| line.iter().map(|run| run.text.as_str()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    fn until(session: &Session, what: impl Fn(&Frame) -> bool) -> Arc<Frame> {
        for _ in 0..200 {
            let frame = session.frame();
            if what(&frame) {
                return frame;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("never showed it: {}", text(&session.frame()));
    }

    #[test]
    fn a_shell_runs_and_its_output_is_a_frame() {
        let dir = std::env::temp_dir();
        let session = Session::spawn(&dir, 80, 24, (Rgb(220, 220, 220), Rgb(0, 0, 0)), || {}).expect("a shell");
        let typed = |text: &str| {
            for c in text.chars() {
                let key = if c == '\n' { Key::Enter } else { Key::Unidentified };
                let text = (c != '\n').then(|| c.to_string());
                session.key(KeyInput { key, text, shift: false, alt: false, ctrl: false, meta: false });
            }
        };
        typed("echo ghost$((20+22))ty\n");
        let frame = until(&session, |f| text(f).contains("ghost42ty"));
        assert_eq!((frame.cols, frame.rows), (80, 24));
        assert!(frame.cursor.is_some());
    }
}
