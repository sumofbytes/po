//! The terminal pane on Rune. [`crate::terminal`] runs the shell and hands out frames; each row
//! of a frame is one mono text with a span per styled run, so Rune draws it like any text.
use std::sync::Arc;

use rune::tok::{Family, TextStyle};
use rune::{Color, El, Key as RuneKey, KeyEvent, Span, Tx, Waker, col, color, text};
use rune_kit::Kit;

use crate::terminal::{self, Frame, Key, KeyInput, Rgb, Session};

const COLS: u16 = 100;
const ROWS: u16 = 30;
const FONT: TextStyle = TextStyle { family: Family::SystemMono, line_height: 17.0, ..TextStyle::new(13.0, 400.0) };

/// Where the host keeps the terminal: handlers reach it through this.
pub type At<S> = fn(&mut S) -> &mut Term;

#[derive(Default)]
pub struct Term {
    session: Option<Session>,
    frame: Arc<Frame>,
}

fn c(rgb: Rgb) -> Color {
    color::hex(&format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)).expect("a valid colour")
}

fn rgb(c: Color) -> Rgb {
    let hex = c.hex();
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    Rgb(byte(1), byte(3), byte(5))
}

impl Term {
    /// Starts the shell in `cwd`; `wake` is called, from another thread, when it has drawn.
    pub fn start(&mut self, cwd: &std::path::Path, kit: &Kit, wake: Waker) {
        self.session = Session::spawn(cwd, COLS, ROWS, (rgb(kit.c.text_base), rgb(kit.c.bg_base)), move || wake()).ok();
    }

    /// True when the shell drew something new since the last call.
    pub fn poll(&mut self) -> bool {
        let Some(session) = &self.session else { return false };
        let frame = session.frame();
        let changed = !Arc::ptr_eq(&frame, &self.frame);
        self.frame = frame;
        changed
    }

    pub fn running(&self) -> bool {
        self.session.is_some()
    }

    /// Sends a key to the shell; false when there is none to take it.
    pub fn key(&self, event: &KeyEvent) -> bool {
        let Some(session) = &self.session else { return false };
        let m = event.mods;
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
    text(line).font(FONT).color(c(frame.foreground)).spans(spans)
}

/// The grid, focusable, sending its keys to the shell.
pub fn view<S: 'static>(term: &Term, kit: &Kit, at: At<S>) -> El {
    let mut grid = col().key("terminal").focusable(true).on_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| at(s).key(event));
    for y in 0..ROWS as usize {
        grid = grid.child(row(&term.frame, kit, y));
    }
    col().w_full().h_full().fill(kit.c.bg_base).padding(12.0).child(grid)
}
