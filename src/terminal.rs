//! A shell in a pseudo-terminal, emulated by libghostty-vt. [`Session`] runs the shell and hands out
//! [`Frame`]s: plain data (runs of styled text, the cursor) that the view paints, so nothing that
//! belongs to libghostty crosses a thread. The emulator is not `Send`, so it lives on one thread of
//! its own, which also encodes the keys and answers the shell's queries.
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// What goes to the emulator, and the frame it hands the view: followed by hotpath in a profiled
// build, the std types otherwise.
use hotpath::wrap::std::sync::{Mutex as FrameLock, mpsc as watched};

/// The least time between two frames while output is streaming in.
const FRAME: Duration = Duration::from_millis(8);
const SCROLLBACK: usize = 10_000;
/// The most output that waits to be emulated, in bytes.
const BACKLOG: usize = 4 * 1024 * 1024;

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

#[derive(Clone, PartialEq, Debug)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    /// What the cell under it holds, to draw over a block.
    pub under: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// Rows that did not change are shared with the frame before.
    pub lines: Vec<Arc<Vec<Run>>>,
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
    tx: watched::Sender<Msg>,
    frame: Arc<FrameLock<Arc<Frame>>>,
    pty: pty::Pty,
}

impl Session {
    /// Starts `$SHELL` in `cwd`. `wake` is called, from another thread, whenever there is a new frame.
    pub fn spawn(cwd: &Path, cols: u16, rows: u16, theme: (Rgb, Rgb), wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let pty = pty::Pty::spawn(cwd, cols, rows)?;
        let (tx, rx) = hotpath::channel!(mpsc::channel(), label = "terminal input");
        let frame = Arc::new(hotpath::mutex!(Mutex::new(Arc::new(Frame::default())), label = "terminal frame"));

        let reader = pty.reader()?;
        let output = tx.clone();
        // Bytes read and not yet emulated: the reader waits when the emulator falls behind, so a
        // runaway program can't fill memory.
        let backlog = Arc::new(AtomicUsize::new(0));
        let reading = backlog.clone();
        std::thread::Builder::new().name("terminal-read".into()).spawn({
            let pty = pty.clone();
            move || {
                use std::io::Read as _;
                let mut reader = reader;
                let mut buf = vec![0; 64 * 1024];
                // A closed pty reads as an error on Linux, and as the end elsewhere.
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => 0,
                    };
                    if n == 0 {
                        break;
                    }
                    while reading.load(Ordering::Acquire) > BACKLOG {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    reading.fetch_add(n, Ordering::AcqRel);
                    if output.send(Msg::Output(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                let _ = output.send(Msg::Exited(pty.wait()));
            }
        })?;

        // What goes to the shell is written on a thread of its own: a program that isn't reading
        // blocks the write, and the emulator must keep drawing meanwhile.
        let (to_shell, written) = mpsc::channel::<Vec<u8>>();
        let mut writer = pty.writer()?;
        std::thread::Builder::new().name("terminal-write".into()).spawn(move || {
            use std::io::Write as _;
            while let Ok(bytes) = written.recv() {
                if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                    break;
                }
            }
        })?;
        let shared = frame.clone();
        std::thread::Builder::new().name("terminal".into()).spawn(move || {
            let released = backlog.clone();
            emulate(rx, to_shell, backlog, shared, (cols, rows), theme, wake);
            // Nothing will be emulated any more: a reader waiting for room must not wait for it.
            // Its next send fails, which ends it.
            released.store(0, Ordering::Release);
        })?;
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
        // Ending the shell waits on it: not on the thread that draws.
        let pty = self.pty.clone();
        if std::thread::Builder::new().name("terminal-kill".into()).spawn(move || pty.kill()).is_err() {
            log::warn!("terminal: could not end the shell");
        }
    }
}

fn emulate(rx: watched::Receiver<Msg>, to_shell: Sender<Vec<u8>>, backlog: Arc<AtomicUsize>, shared: Arc<FrameLock<Arc<Frame>>>, (cols, rows): (u16, u16), theme: (Rgb, Rgb), wake: impl Fn()) {
    use libghostty_vt::key::{Action, Encoder, Event, Mods};
    use libghostty_vt::terminal::ScrollViewport;
    use libghostty_vt::{RenderState, Terminal, TerminalOptions};

    let Ok(mut terminal) = Terminal::new(TerminalOptions { cols, rows, max_scrollback: SCROLLBACK }) else {
        log::error!("terminal: libghostty could not start");
        return;
    };
    // The shell's queries (cursor position, device attributes) are answered through the same pipe
    // the keys go down.
    let reply = to_shell.clone();
    let _ = terminal.on_pty_write(move |_, data| {
        let _ = reply.send(data.to_vec());
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
        let _ = to_shell.send(bytes.to_vec());
    };

    let (mut dirty, mut last, mut exited) = (true, Instant::now() - FRAME, None);
    let mut keys = Vec::new();
    let mut cache = Cache::default();
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
                Msg::Output(bytes) => {
                    terminal.vt_write(&bytes);
                    backlog.fetch_sub(bytes.len(), Ordering::AcqRel);
                }
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
            if let Some(frame) = snapshot(&terminal, &mut render, &mut cache, name, exited) {
                if let Ok(mut slot) = shared.lock() {
                    *slot = Arc::new(frame);
                }
                wake();
            }
            (dirty, last) = (false, Instant::now());
        }
    }
}

/// The rows of the last frame, so the rows that did not change are not built again.
#[derive(Default)]
struct Cache {
    lines: Vec<Arc<Vec<Run>>>,
    colors: Option<(Rgb, Rgb)>,
}

#[hotpath::measure]
fn snapshot<'a>(terminal: &libghostty_vt::Terminal<'a, '_>, render: &mut libghostty_vt::RenderState<'a>, cache: &mut Cache, title: String, exited: Option<i32>) -> Option<Frame> {
    use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
    use libghostty_vt::screen::CellWide;
    use libghostty_vt::style::{RgbColor, Underline};

    let rgb = |c: RgbColor| Rgb(c.r, c.g, c.b);
    let snapshot = render.update(terminal).ok()?;
    let colors = snapshot.colors().ok()?;
    let (background, foreground) = (rgb(colors.background), rgb(colors.foreground));
    // Anything but a partial change redraws it all, and so does a change of colors, which the
    // cached runs have resolved.
    let all = snapshot.dirty().ok() != Some(libghostty_vt::render::Dirty::Partial) || cache.colors != Some((background, foreground));
    cache.colors = Some((background, foreground));
    let mut rows = RowIterator::new().ok()?;
    let mut cells = CellIterator::new().ok()?;
    let mut row_iter = rows.update(&snapshot).ok()?;
    let mut lines = Vec::new();
    let at = snapshot.cursor_visible().ok().filter(|visible| *visible).and_then(|_| snapshot.cursor_viewport().ok().flatten());
    let cursor_at = at.map(|at| (at.x, at.y));
    let mut under = String::new();
    while let Some(row) = row_iter.next() {
        let row_index = lines.len() as u16;
        // The cursor's row is built again for the character under the cursor.
        let kept = if all || row.dirty().unwrap_or(true) || at.is_some_and(|at| at.y == row_index) { None } else { cache.lines.get(usize::from(row_index)) };
        if let Some(kept) = kept {
            lines.push(kept.clone());
            continue;
        }
        let _ = row.set_dirty(false);
        let mut runs: Vec<Run> = Vec::new();
        let mut cell_iter = cells.update(row).ok()?;
        let (mut col, mut after_wide) = (0_u16, false);
        while let Some(cell) = cell_iter.next() {
            let wide = cell.raw_cell().and_then(|c| c.wide()).unwrap_or(CellWide::Narrow);
            // The wide cell before it already took the column.
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
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
            // A wide character is a run of its own, so the text after it starts on its column.
            let alone = width == 2 || after_wide;
            after_wide = width == 2;
            if cursor_at == Some((col, row_index)) {
                under = text.clone();
            }
            match runs.last_mut() {
                Some(run) if !alone && run.col + run.cells == col && (run.fg, run.bg, run.bold, run.italic, run.underline, run.strike) == (fg, bg, style.bold, style.italic, underline, strike) => {
                    run.text.push_str(&text);
                    run.cells += width;
                }
                _ => runs.push(Run { col, cells: width, text, fg, bg, bold: style.bold, italic: style.italic, underline, strike }),
            }
            col += width;
        }
        // Blank stretches in the default background are what the terminal's own background shows.
        runs.retain(|run| run.bg.is_some() || run.underline || run.strike || run.text.chars().any(|c| c != ' '));
        lines.push(Arc::new(runs));
    }
    cache.lines.clone_from(&lines);
    let cursor = at.map(|at| Cursor {
            col: at.x,
            row: at.y,
            under,
            shape: match snapshot.cursor_visual_style() {
                Ok(CursorVisualStyle::Bar) => CursorShape::Bar,
                Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
                Ok(CursorVisualStyle::BlockHollow) => CursorShape::Hollow,
                _ => CursorShape::Block,
            },
        });
    let (cols, row_count) = (snapshot.cols().ok()?, snapshot.rows().ok()?);
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
            let mut ws = size(cols, rows);
            // SAFETY: both fds are out parameters, owned right after.
            // The BSDs and macOS declare the termios and winsize pointers `*mut`, Linux `*const`.
            #[allow(clippy::unnecessary_mut_passed)]
            let failed = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) } != 0;
            if failed {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openpty just made these.
            let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
            // Neither end belongs to anything else this process starts; the shell gets the slave
            // as its stdio, which keeps it across the exec.
            for fd in [&master, &slave] {
                // SAFETY: a flag on an fd we own.
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
            }
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
                    // Whatever this process ignores or blocks, the shell inherits across the exec: a
                    // shell that ignores the hangup never hangs up its jobs.
                    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU, libc::SIGCHLD, libc::SIGPIPE] {
                        libc::signal(signal, libc::SIG_DFL);
                    }
                    let mut none: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut none);
                    libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
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

        /// Ends the shell and what it started: the hangup a closing terminal sends, to the shell's
        /// whole process group (it leads its own session) and to everything below it, since an
        /// interactive shell puts its background jobs in groups of their own and may die before it
        /// has hung them up; then a kill for what ignores it.
        pub fn kill(&self) {
            let Ok(mut child) = self.child.lock() else { return };
            // Once reaped, its pid may belong to someone else.
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            let pid = child.id() as i32;
            let below = descendants(pid);
            let signal = |signal| {
                // SAFETY: plain signals to our own child's group and its children.
                unsafe {
                    libc::kill(-pid, signal);
                    for pid in &below {
                        libc::kill(*pid, signal);
                    }
                }
            };
            signal(libc::SIGHUP);
            for _ in 0..20 {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            signal(libc::SIGKILL);
            let _ = child.wait();
        }
    }

    /// The process table as (pid, ppid), read from /proc where there is no `ps`.
    fn proc_table() -> Vec<(i32, i32)> {
        let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
        dir.filter_map(|entry| {
            let pid: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // The name is in parentheses and may hold anything: the fields start after the last one.
            let rest = stat.rsplit_once(')')?.1;
            let ppid = rest.split_whitespace().nth(1)?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect()
    }

    /// Every process below `root`, from the process table.
    fn descendants(root: i32) -> Vec<i32> {
        let pairs = match Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() {
            Ok(table) => String::from_utf8_lossy(&table.stdout)
                .lines()
                .filter_map(|line| {
                    let mut ids = line.split_whitespace().map(str::parse::<i32>);
                    Some((ids.next()?.ok()?, ids.next()?.ok()?))
                })
                .collect(),
            Err(_) => proc_table(),
        };
        let mut found = vec![root];
        let mut next = 0;
        while next < found.len() {
            let parent = found[next];
            found.extend(pairs.iter().filter(|(_, ppid)| *ppid == parent).map(|(pid, _)| *pid));
            next += 1;
        }
        found.remove(0);
        found
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

    #[test]
    fn wide_characters_take_two_columns_and_the_text_after_them_follows() {
        let dir = std::env::temp_dir();
        let session = Session::spawn(&dir, 80, 24, (Rgb(220, 220, 220), Rgb(0, 0, 0)), || {}).expect("a shell");
        for c in "printf '\\344\\270\\255XY\\n'\n".chars() {
            let key = if c == '\n' { Key::Enter } else { Key::Unidentified };
            let text = (c != '\n').then(|| c.to_string());
            session.key(KeyInput { key, text, shift: false, alt: false, ctrl: false, meta: false });
        }
        let frame = until(&session, |f| f.lines.iter().flat_map(|line| line.iter()).any(|run| run.text == "中"));
        let at = |text: &str| frame.lines.iter().flat_map(|line| line.iter()).find(|run| run.text.ends_with(text) && run.col > 0 || run.text == text).map(|run| (run.col, run.cells));
        let wide = frame.lines.iter().flat_map(|line| line.iter()).find(|run| run.text == "中").expect("the wide character");
        assert_eq!(wide.cells, 2);
        let after = frame.lines.iter().flat_map(|line| line.iter()).find(|run| run.text.starts_with("XY")).unwrap_or_else(|| panic!("{:?}", at("XY")));
        assert_eq!(after.col, wide.col + 2, "the text after a wide character starts right after its two cells");
    }

    #[test]
    fn closing_the_terminal_ends_what_the_shell_left_running() {
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("po-terminal-test-{}", std::process::id()));
        let session = Session::spawn(&dir, 80, 24, (Rgb(220, 220, 220), Rgb(0, 0, 0)), || {}).expect("a shell");
        let line = format!("sleep 31337 & echo $! > {}\n", marker.display());
        for c in line.chars() {
            let key = if c == '\n' { Key::Enter } else { Key::Unidentified };
            let text = (c != '\n').then(|| c.to_string());
            session.key(KeyInput { key, text, shift: false, alt: false, ctrl: false, meta: false });
        }
        let pid = (0..200)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(25));
                std::fs::read_to_string(&marker).ok().and_then(|p| p.trim().parse::<i32>().ok())
            })
            .expect("the background job started");
        // A killed process nobody has reaped yet still answers signal 0, so Linux's state is read.
        let alive = |pid: i32| {
            // SAFETY: signal 0 only asks whether the process exists.
            let exists = unsafe { libc::kill(pid, 0) == 0 };
            let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| stat.rsplit(')').next().is_some_and(|rest| rest.trim_start().starts_with('Z')));
            exists && !zombie
        };
        assert!(alive(pid));
        drop(session);
        let gone = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(25));
            !alive(pid)
        });
        let _ = std::fs::remove_file(&marker);
        assert!(gone, "the background job outlived its terminal");
    }
}
