//! The terminals an agent asks Po to run commands in (ACP's `terminal/*` methods). The client that
//! takes the requests and the transcript that draws them are on different threads, so the sessions
//! live here, by id, and the transcript reads their frames.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::host::Wake;
use crate::system::terminal::{Frame, Launch, Rgb, Session};

/// The grid a command is told it has: the transcript draws it as wide as it is, and as tall as it
/// needs, so the width is a guess that wraps long lines the way a normal terminal would.
pub const GRID: (u16, u16) = (100, 40);
/// The most terminals running or kept for one window; the agent releases them, and those it forgot go
/// when its thread closes.
pub const MAX: usize = 32;
/// The most output kept for a command when the agent names no limit, in bytes.
pub const DEFAULT_LIMIT: usize = 1 << 20;

static NEXT: AtomicU64 = AtomicU64::new(1);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static WAKE: OnceLock<Wake> = OnceLock::new();
static THEME: Mutex<(Rgb, Rgb)> = Mutex::new((Rgb(0xdd, 0xdd, 0xdd), Rgb(0x12, 0x12, 0x12)));
static ALL: Mutex<Option<HashMap<String, Arc<Entry>>>> = Mutex::new(None);

struct Entry {
    session: Session,
    /// A command the agent runs itself and streams the output of, rather than one Po runs.
    display: bool,
    /// When it was made, to drop the oldest displays first.
    made: u64,
    /// How many frames it drew.
    drawn: Arc<AtomicU64>,
}

/// The most display terminals kept: a long thread runs many commands, and the oldest go first.
pub const MAX_DISPLAYS: usize = 64;

/// The window can be woken from here on, so a terminal that drew something gets drawn.
pub fn set_waker(wake: Wake) {
    let _ = WAKE.set(wake);
}

/// The colours new commands are told they have.
pub fn set_theme(theme: (Rgb, Rgb)) {
    if let Ok(mut now) = THEME.lock() {
        *now = theme;
    }
}

/// Bumped whenever any terminal drew something: the window redraws when it changed.
pub fn generation() -> u64 {
    FRAMES.load(Ordering::Acquire)
}

fn find(id: &str) -> Option<Arc<Entry>> {
    ALL.lock().ok()?.as_ref()?.get(id).cloned()
}

/// Starts `launch` in `cwd`; the terminal's id, or why it did not start.
pub fn create(launch: &Launch, cwd: &Path, limit: usize) -> Result<String, String> {
    let mut all = ALL.lock().map_err(|_| "terminals are unavailable")?;
    let all = all.get_or_insert_with(HashMap::new);
    if all.values().filter(|e| !e.display).count() >= MAX {
        return Err(format!("too many terminals open ({MAX}): release some"));
    }
    let theme = THEME
        .lock()
        .map_or((Rgb(0xdd, 0xdd, 0xdd), Rgb(0x12, 0x12, 0x12)), |t| *t);
    let count = Arc::default();
    let session = Session::spawn_command(launch, cwd, GRID, theme, limit, drew(&count))
        .map_err(|e| format!("could not start `{}`: {e}", launch.program))?;
    let made = NEXT.fetch_add(1, Ordering::Relaxed);
    let id = format!("po-term-{made}");
    let entry = Entry {
        session,
        display: false,
        made,
        drawn: count,
    };
    all.insert(id.clone(), Arc::new(entry));
    Ok(id)
}

/// What a terminal calls when it drew something: counts it, and wakes the window to draw it.
fn drew(count: &Arc<AtomicU64>) -> impl Fn() + Send + 'static {
    let count = count.clone();
    move || {
        count.fetch_add(1, Ordering::AcqRel);
        FRAMES.fetch_add(1, Ordering::AcqRel);
        if let Some(wake) = WAKE.get() {
            wake();
        }
    }
}

/// How many frames the terminal `id` drew: a new count is something new to show.
pub fn drawn(id: &str) -> Option<u64> {
    find(id).map(|e| e.drawn.load(Ordering::Acquire))
}

/// The display terminal `id`, made the first time the agent names it.
fn display(id: &str) -> Option<Arc<Entry>> {
    let mut all = ALL.lock().ok()?;
    let all = all.get_or_insert_with(HashMap::new);
    if let Some(entry) = all.get(id) {
        return entry.display.then(|| entry.clone());
    }
    let displays = all.values().filter(|e| e.display).count();
    if displays >= MAX_DISPLAYS
        && let Some(oldest) = all
            .iter()
            .filter(|(_, e)| e.display)
            .min_by_key(|(_, e)| e.made)
            .map(|(id, _)| id.clone())
    {
        all.remove(&oldest);
    }
    let theme = THEME
        .lock()
        .map_or((Rgb(0xdd, 0xdd, 0xdd), Rgb(0x12, 0x12, 0x12)), |t| *t);
    let count = Arc::default();
    let session = Session::display(GRID, theme, drew(&count)).ok()?;
    let entry = Arc::new(Entry {
        session,
        display: true,
        made: NEXT.fetch_add(1, Ordering::Relaxed),
        drawn: count,
    });
    all.insert(id.to_string(), entry.clone());
    Some(entry)
}

/// The agent named the display terminal `id`: it shows, empty, until output comes.
pub fn announce(id: &str) {
    display(id);
}

/// The agent sent output of its command in `id`: more of it, or (`replace`) all of it so far.
pub fn feed(id: &str, text: &str, replace: bool) {
    if let Some(entry) = display(id) {
        entry.session.feed(text, replace);
    }
}

/// The agent's command in `id` ended.
pub fn finish(id: &str, status: i32) {
    if let Some(entry) = display(id) {
        entry.session.finish(status);
    }
}

/// What the terminal shows now.
pub fn frame(id: &str) -> Option<Arc<Frame>> {
    find(id).map(|e| e.session.frame())
}

/// What it wrote so far, whether the start was dropped for the limit, and its exit status once over.
pub fn output(id: &str) -> Option<(String, bool, Option<i32>)> {
    let entry = find(id)?;
    let (text, truncated) = entry.session.output();
    Some((text, truncated, entry.session.frame().exited))
}

/// Whether Po runs the command of `id`, so it can end it; the agent runs a display's itself.
pub fn owned(id: &str) -> bool {
    find(id).is_some_and(|e| !e.display)
}

/// A key the user typed into the terminal `id`, for the command to read; false when it is not Po's
/// to take it, or has ended.
pub fn key(id: &str, event: &rune::KeyEvent) -> bool {
    find(id)
        .filter(|e| !e.display && e.session.frame().exited.is_none())
        .is_some_and(|e| crate::rune_ui::terminal::send_key(&e.session, event))
}

/// Text pasted into the terminal `id`, for the command to read.
pub fn paste(id: &str, text: &str) {
    if let Some(e) = find(id).filter(|e| !e.display) {
        e.session.paste(text.to_string());
    }
}

/// Ends the command, keeping the terminal to show what it wrote.
pub fn kill(id: &str) -> bool {
    find(id).map(|e| e.session.kill()).is_some()
}

/// Forgets the terminal, which ends its command if it still runs.
pub fn release(id: &str) -> bool {
    ALL.lock()
        .ok()
        .and_then(|mut all| all.as_mut()?.remove(id))
        .is_some()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_is_created_read_and_released() {
        let launch = Launch {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo hi".into()],
            env: vec![],
        };
        let id = create(&launch, &std::env::temp_dir(), 1024).expect("starts");
        for _ in 0..200 {
            if output(&id).is_some_and(|(_, _, status)| status.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let (text, _, status) = output(&id).expect("known");
        assert!(text.contains("hi"));
        assert_eq!(status, Some(0));
        assert!(release(&id));
        assert!(output(&id).is_none());
    }

    #[test]
    fn a_display_terminal_is_made_by_its_output() {
        feed("toolu_test_1", "built\n", false);
        finish("toolu_test_1", 0);
        for _ in 0..200 {
            if frame("toolu_test_1").is_some_and(|f| f.exited.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let frame = frame("toolu_test_1").expect("made");
        assert_eq!(frame.exited, Some(0));
        let text: String = frame.lines[0].iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text.trim(), "built");
        // A command Po did not start can't be stopped through it.
        assert!(!owned("toolu_test_1"));
        release("toolu_test_1");
    }
}
