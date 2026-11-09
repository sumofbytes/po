//! The terminal pane on Rune. [`crate::terminal`] runs the shell and hands out frames; each row
//! of a frame is one mono text with a span per styled run, so Rune draws it like any text.
use std::sync::Arc;

use rune::tok::{Family, TextStyle};
use rune::{WheelEvent, Color, El, Key as RuneKey, KeyEvent, Span, Tx, col, color, text};
use rune_kit::Kit;

use crate::terminal::{self, Frame, Key, KeyInput, Rgb, Session};

const COLS: u16 = 100;
const ROWS: u16 = 30;
const FONT: TextStyle = TextStyle { family: Family::SystemMono, line_height: 17.0, ..TextStyle::new(13.0, 400.0) };

/// Where the host keeps the terminal: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut Term;

pub struct Term {
    session: Option<Session>,
    frame: Arc<Frame>,
    /// The grid the shell was last told it has.
    size: (u16, u16),
}

impl Default for Term {
    fn default() -> Self {
        Self { session: None, frame: Arc::default(), size: (COLS, ROWS) }
    }
}

fn c(rgb: Rgb) -> Color {
    color::hex(&format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)).expect("a valid colour")
}

impl Term {
    /// Starts the shell in `cwd`, with the colours (foreground, background) the shell is told it has.
    pub fn start_with(&mut self, cwd: &std::path::Path, theme: (Rgb, Rgb), wake: impl Fn() + Send + 'static) {
        self.session = Session::spawn(cwd, COLS, ROWS, theme, wake).ok();
    }

    /// The shell has ended.
    pub fn exited(&self) -> bool {
        self.session.as_ref().is_some_and(|session| session.frame().exited.is_some())
    }

    /// True when the shell drew something new since the last call.
    pub fn poll(&mut self) -> bool {
        let Some(session) = &self.session else { return false };
        let frame = session.frame();
        let changed = !Arc::ptr_eq(&frame, &self.frame);
        self.frame = frame;
        changed
    }

    /// The area for the grid is now `width` by `height` points: the shell gets as many cells as fit.
    pub fn fit(&mut self, width: f32, height: f32) {
        let cell = rune::text::advance("M", FONT).max(1.0);
        let size = (((width / cell).floor() as u16).clamp(10, 500), ((height / FONT.line_height).floor() as u16).clamp(3, 200));
        if size == self.size || width < 1.0 {
            return;
        }
        self.size = size;
        if let Some(session) = &self.session {
            session.resize(size.0, size.1, (cell as u32, FONT.line_height as u32));
        }
    }

    /// The history moves by the wheel: `dy` points, positive showing earlier lines.
    pub fn wheel(&self, dy: f32) -> bool {
        let Some(session) = &self.session else { return false };
        let lines = (dy / FONT.line_height).round() as isize;
        if lines != 0 {
            session.scroll(-lines);
        }
        true
    }

    /// Text pasted into the window goes to the shell.
    pub fn paste(&self, text: &str) {
        if let Some(session) = &self.session {
            session.paste(text.to_string());
        }
    }

    /// Sends a key to the shell; false when there is none to take it.
    pub fn key(&self, event: &KeyEvent) -> bool {
        let Some(session) = &self.session else { return false };
        let m = event.mods;
        // Copy and paste are the window's: Ctrl+Shift+C and V (or the platform key), where Ctrl+C
        // and Ctrl+V are the shell's.
        if let RuneKey::Command(ch) = &event.key
            && matches!(ch.to_ascii_lowercase(), 'c' | 'v')
            && (m.logo || (m.ctrl && m.shift))
        {
            return false;
        }
        // Several characters at once (an IME commit, typed text) go in one by one: the encoder
        // takes one key with its text.
        if let RuneKey::Text(t) = &event.key
            && t.chars().nth(1).is_some()
        {
            for ch in t.chars() {
                session.key(KeyInput { key: key_of(ch), text: Some(ch.to_string()), shift: m.shift, alt: m.alt, ctrl: m.ctrl, meta: false });
            }
            return true;
        }
        let (key, text) = match &event.key {
            RuneKey::Text(t) => (t.chars().next().map(key_of).unwrap_or(Key::Unidentified), Some(t.clone())),
            RuneKey::Command(ch) => (key_of(*ch), None),
            RuneKey::Enter => (Key::Enter, Some("\r".into())),
            RuneKey::Backspace => (Key::Backspace, None),
            RuneKey::Delete => (Key::Delete, None),
            RuneKey::Left => (Key::ArrowLeft, None),
            RuneKey::Right => (Key::ArrowRight, None),
            RuneKey::Up => (Key::ArrowUp, None),
            RuneKey::Down => (Key::ArrowDown, None),
            RuneKey::Home => (Key::Home, None),
            RuneKey::End => (Key::End, None),
            RuneKey::PageUp => (Key::PageUp, None),
            RuneKey::PageDown => (Key::PageDown, None),
            RuneKey::Insert => (Key::Insert, None),
            RuneKey::Escape => (Key::Escape, None),
            RuneKey::Tab => (Key::Tab, None),
            _ => return false,
        };
        session.key(KeyInput { key, text, shift: m.shift, alt: m.alt, ctrl: m.ctrl, meta: false });
        true
    }
}

/// The key the encoder names for a typed character.
fn key_of(ch: char) -> Key {
    use terminal::Key as K;
    const LETTERS: [K; 26] = [
        K::A, K::B, K::C, K::D, K::E, K::F, K::G, K::H, K::I, K::J, K::K, K::L, K::M, K::N, K::O, K::P, K::Q, K::R, K::S, K::T, K::U, K::V, K::W, K::X, K::Y, K::Z,
    ];
    match ch.to_ascii_lowercase() {
        l @ 'a'..='z' => LETTERS[l as usize - 'a' as usize],
        ' ' => K::Space,
        _ => K::Unidentified,
    }
}

/// One terminal row as text plus a span per styled run. Cells are placed by column, so a gap
/// between runs becomes spaces.
fn row(frame: &Frame, kit: &Kit, y: usize) -> El {
    let mut line = String::new();
    let mut spans = vec![];
    let mut at = 0u16;
    for run in frame.lines.get(y).map(|l| l.as_slice()).unwrap_or_default() {
        while at < run.col {
            line.push(' ');
            at += 1;
        }
        let start = line.len();
        line.push_str(&run.text);
        at = run.col + run.cells;
        let mut span = Span::new(start, line.len()).color(c(run.fg));
        if let Some(bg) = run.bg {
            span = span.bg(c(bg), 0.0);
        }
        if run.bold {
            span = span.weight(700.0);
        }
        if run.italic {
            span = span.italic();
        }
        if run.underline {
            span = span.underline(c(run.fg));
        }
        spans.push(span.mono());
    }
    if let Some(cursor) = frame.cursor.as_ref().filter(|k| k.row as usize == y) {
        while at <= cursor.col {
            line.push(' ');
            at += 1;
        }
        // The cursor's cell is the n-th char of the line: cells and chars agree off wide glyphs.
        if let Some((start, ch)) = line.char_indices().nth(cursor.col as usize) {
            spans.push(Span::new(start, start + ch.len_utf8()).bg(kit.c.text_base, 0.0).color(kit.c.bg_base).mono());
        }
    }
    text(line).font(FONT).color(c(frame.foreground)).spans(spans).selectable(true).h(FONT.line_height).min_h(FONT.line_height)
}

/// The window's copy chord: the modifier key, or Ctrl+Shift+C elsewhere. Paste is handled by the
/// window, so this takes the copy chord only.
fn copy_chord(event: &KeyEvent) -> bool {
    matches!(&event.key, RuneKey::Command('c')) && (event.mods.logo || (event.mods.ctrl && event.mods.shift))
}

/// The grid, focusable, sending its keys to the shell.
pub fn view<S: 'static>(term: &Term, kit: &Kit, at: At<S>) -> El {
    let mut grid = col()
        .key("terminal")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .focusable(true)
        .no_focus_ring()
        .selection_group(true)
        .on_key(move |s: &mut S, event: &KeyEvent, tx: &mut Tx| {
            // The selection goes to the clipboard on the window's copy chord, where Ctrl+C is the
            // shell's and the selection's own menu copies besides.
            if copy_chord(event)
                && let Some(selected) = tx.text_selection()
            {
                tx.copy(selected.text().to_string());
                return true;
            }
            at(s).key(event)
        })
        .on_wheel(move |s: &mut S, event: WheelEvent, _: &mut Tx| at(s).wheel(event.dy))
        .on_layout(move |s: &mut S, rect: [f32; 4]| at(s).fit(rect[2], rect[3]));
    for y in 0..term.size.1 as usize {
        grid = grid.child(row(&term.frame, kit, y));
    }
    col().w_full().h_full().min_h(0.0).fill(kit.c.bg_base).px(12.0).py(8.0).clip_soft(0.0).child(grid)
}
