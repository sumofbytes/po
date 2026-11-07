//! The Rune port's proving ground. For now: a window in Po's Rose Pine colours holding a live
//! shell, to test whether Rune can draw a terminal. `cargo run --bin po-rune` opens it;
//! `-- --snap po.png` renders it headless.

use std::sync::Arc;

use rune::tok::{Family, TextStyle};
use rune::{AppSpec, Color, Cx, El, Key as RuneKey, KeyEvent, Span, Tx, Waker, col, color, text};

#[allow(dead_code)]
#[path = "../terminal.rs"]
mod terminal;

use terminal::{Frame, KeyInput, Rgb, Session};

const COLS: u16 = 100;
const ROWS: u16 = 30;
const FONT: TextStyle = TextStyle { family: Family::SystemMono, line_height: 17.0, ..TextStyle::new(13.0, 400.0) };

/// Rose Pine, Po's default theme.
const BASE: Rgb = Rgb(0x19, 0x17, 0x24);
const TEXT: Rgb = Rgb(0xe0, 0xde, 0xf4);

#[derive(Default)]
struct State {
    session: Option<Session>,
    frame: Arc<Frame>,
}

fn c(rgb: Rgb) -> Color {
    color::hex(&format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)).expect("a valid colour")
}

/// One terminal row as text plus a span per styled run. Cells are placed by column, so a gap
/// between runs becomes spaces.
fn row(frame: &Frame, y: usize) -> El {
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
            spans.push(Span::new(start, start + ch.len_utf8()).bg(c(TEXT), 0.0).color(c(BASE)).mono());
        }
    }
    text(line).font(FONT).color(c(frame.foreground)).spans(spans)
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let mut grid = col().key("terminal").focusable(true).on_key(on_key);
    for y in 0..ROWS as usize {
        grid = grid.child(row(&s.frame, y));
    }
    col().w_full().h_full().fill(c(BASE)).padding(12.0).child(grid)
}

/// The key the encoder names for a typed character.
fn key_of(ch: char) -> terminal::Key {
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

fn on_key(s: &mut State, event: &KeyEvent, _: &mut Tx) -> bool {
    use terminal::Key as K;
    let Some(session) = &s.session else { return false };
    let (key, text) = match &event.key {
        RuneKey::Text(t) => (t.chars().next().map(key_of).unwrap_or(K::Unidentified), Some(t.clone())),
        RuneKey::Command(ch) => (key_of(*ch), None),
        RuneKey::Enter => (K::Enter, Some("\r".into())),
        RuneKey::Backspace => (K::Backspace, None),
        RuneKey::Delete => (K::Delete, None),
        RuneKey::Left => (K::ArrowLeft, None),
        RuneKey::Right => (K::ArrowRight, None),
        RuneKey::Up => (K::ArrowUp, None),
        RuneKey::Down => (K::ArrowDown, None),
        RuneKey::Home => (K::Home, None),
        RuneKey::End => (K::End, None),
        RuneKey::PageUp => (K::PageUp, None),
        RuneKey::PageDown => (K::PageDown, None),
        RuneKey::Insert => (K::Insert, None),
        RuneKey::Escape => (K::Escape, None),
        RuneKey::Tab => (K::Tab, None),
        _ => return false,
    };
    let m = event.mods;
    session.key(KeyInput { key, text, shift: m.shift, alt: m.alt, ctrl: m.ctrl, meta: false });
    true
}

fn setup(s: &mut State, wake: Waker) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    s.session = Session::spawn(&cwd, COLS, ROWS, (TEXT, BASE), move || wake()).ok();
}

/// True when the shell drew something new.
fn poll(s: &mut State, _: f64, _: &mut Tx) -> bool {
    let Some(session) = &s.session else { return false };
    let frame = session.frame();
    let changed = !Arc::ptr_eq(&frame, &s.frame);
    s.frame = frame;
    changed
}

fn main() {
    rune::run(AppSpec::<State> {
        // Mono spans draw in the mono slot: point it at the system's monospace font.
        families: ("Inter", "ui-monospace"),
        setup: Some(setup),
        poll: Some(poll),
        ..AppSpec::new("Po", (1000.0, 560.0), State::default, view)
    });
}
