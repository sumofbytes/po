//! The terminal pane: a [`Session`]'s frames painted in the mono font of the theme, and the keys,
//! paste and wheel that go back to it.
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use futures::StreamExt as _;
use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, Font, FontStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Pixels, Render,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, Styled as _, ClipboardItem, StrikethroughStyle, TextRun, UnderlineStyle, Window, canvas, div, fill, point, px, size,
};

use crate::terminal::{CursorShape, Frame, Key, KeyInput, Rgb, Session};
use crate::theme::Palette;

/// Wheel lines per notch when the wheel reports lines.
const WHEEL_LINES: f32 = 3.0;
/// Height of a line, as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.3;

/// A selection in the visible cells: where the press was and where the pointer is now, as
/// (column, row).
type Selection = Option<((u16, u16), (u16, u16))>;

/// The shell ended: the view has nothing left to show.
pub struct Exited;

pub struct TerminalView {
    root: PathBuf,
    focus: FocusHandle,
    session: Option<Rc<Session>>,
    error: Option<String>,
    /// The cells and the size of one, as last painted.
    grid: Rc<Cell<(u16, u16, f32, f32)>>,
    /// The size of a cell in the font and size it was measured for: shaping text to measure it is
    /// not done again on every frame.
    measured: Rc<Cell<Option<(gpui::SharedString, Pixels, (f32, f32))>>>,
    /// The colors the session was last told of.
    theme: Option<(Rgb, Rgb)>,
    /// The part of a line the wheel has turned and not yet scrolled: a touchpad sends fractions.
    wheel_rest: f32,
    selection: Rc<Cell<Selection>>,
    /// Whether the left button is down, selecting.
    selecting: bool,
    /// Where the cells start on the screen, as last painted.
    origin: Rc<Cell<gpui::Point<Pixels>>>,
}

impl TerminalView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self { root, focus: cx.focus_handle(), session: None, error: None, grid: Rc::new(Cell::new((80, 24, 8.0, 16.0))), measured: Rc::new(Cell::new(None)), theme: None, wheel_rest: 0.0, selection: Rc::new(Cell::new(None)), selecting: false, origin: Rc::new(Cell::new(point(px(0.), px(0.)))) };
        view.start(cx);
        view
    }

    /// The shell, started over: the first time, or after it exited.
    fn start(&mut self, cx: &mut Context<Self>) {
        let (fg, bg) = colors(cx.global::<Palette>());
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let (cols, rows, ..) = self.grid.get();
        match Session::spawn(&self.root, cols, rows, (fg, bg), move || {
            let _ = wake.unbounded_send(());
        }) {
            Ok(session) => {
                self.session = Some(Rc::new(session));
                self.error = None;
                self.theme = Some((fg, bg));
                cx.spawn(async move |this, cx| {
                    while woken.next().await.is_some() {
                        // Whatever else arrived meanwhile is in the same frame.
                        while woken.try_recv().is_ok() {}
                        let gone = this.update(cx, |this, cx| {
                            cx.notify();
                            let exited = this.session.as_ref().is_some_and(|s| s.frame().exited.is_some());
                            if exited {
                                cx.emit(Exited);
                            }
                            exited
                        });
                        // Once the shell has ended, the one `Exited` was all there was to say.
                        if gone.unwrap_or(true) {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                log::warn!("terminal: {error}");
                self.session = None;
                self.error = Some(format!("The shell could not start: {error}"));
            }
        }
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if self.session.is_none() {
            // The shell could not start: any key tries again.
            self.start(cx);
            return;
        }
        let (shift, alt, ctrl, meta) = (stroke.modifiers.shift, stroke.modifiers.alt, stroke.modifiers.control, stroke.modifiers.platform);
        // Paste is Ctrl+Shift+V, or Cmd+V.
        if stroke.key == "v" && ((ctrl && shift) || (meta && !ctrl)) {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.session.as_ref().inspect(|s| s.paste(text));
            }
            cx.stop_propagation();
            return;
        }
        // Copy is Ctrl+Shift+C, or Cmd+C: Ctrl+C stays the shell's interrupt.
        if stroke.key == "c" && ((ctrl && shift) || (meta && !ctrl)) {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        // Cmd keys are the app's, not the shell's.
        if meta {
            return;
        }
        let Some(key) = key_of(&stroke.key) else { return };
        let text = stroke.key_char.clone().filter(|t| !t.chars().all(char::is_control));
        // A key the encoder has no name for is only worth sending if it types something.
        if key == Key::Unidentified && text.is_none() {
            return;
        }
        // Typing goes to the shell and ends the selection.
        self.selection.set(None);
        if let Some(session) = &self.session {
            session.key(KeyInput { key, text, shift, alt, ctrl, meta });
        }
        cx.stop_propagation();
    }

    /// The cell under `position`, clamped to the screen.
    fn cell_at(&self, position: gpui::Point<Pixels>) -> (u16, u16) {
        let (cols, rows, cell_w, cell_h) = self.grid.get();
        let origin = self.origin.get();
        let col = (f32::from(position.x - origin.x) / cell_w).floor().clamp(0.0, f32::from(cols.saturating_sub(1)));
        let row = (f32::from(position.y - origin.y) / cell_h).floor().clamp(0.0, f32::from(rows.saturating_sub(1)));
        (col as u16, row as u16)
    }

    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window, cx);
        let at = self.cell_at(event.position);
        self.selecting = true;
        // A click alone selects nothing, and drops what was selected.
        self.selection.set(Some((at, at)));
        cx.notify();
    }

    fn drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let at = self.cell_at(event.position);
        if let Some((from, _)) = self.selection.get().filter(|(_, to)| *to != at) {
            self.selection.set(Some((from, at)));
            cx.notify();
        }
    }

    fn release(&mut self, _: &MouseUpEvent, cx: &mut Context<Self>) {
        self.selecting = false;
        if self.selection.get().is_some_and(|(from, to)| from == to) {
            self.selection.set(None);
            cx.notify();
        }
    }

    /// Puts the selected text on the clipboard.
    fn copy(&self, cx: &mut Context<Self>) {
        let (Some(session), Some((from, to))) = (&self.session, self.selection.get()) else { return };
        let text = selected_text(&session.frame(), from, to);
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn wheel(&mut self, event: &ScrollWheelEvent) {
        let line = self.grid.get().3.max(1.0);
        let lines = match event.delta {
            gpui::ScrollDelta::Lines(delta) => delta.y * WHEEL_LINES,
            gpui::ScrollDelta::Pixels(delta) => f32::from(delta.y) / line,
        };
        self.wheel_rest += lines;
        let step = self.wheel_rest.trunc();
        self.wheel_rest -= step;
        if step != 0.0 {
            // The lines under the selection move.
            self.selection.set(None);
        }
        if let (Some(session), true) = (&self.session, step != 0.0) {
            // A wheel turned up reads older lines, which is up in the history.
            session.scroll(-(step as isize));
        }
    }
}

impl gpui::EventEmitter<Exited> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.global::<Palette>();
        let (family, font_size, muted) = (palette.mono_font_family.clone(), palette.mono_font_size, palette.muted_foreground);
        let now = colors(palette);
        if let (Some(session), true) = (&self.session, self.theme != Some(now)) {
            session.theme(now.0, now.1);
            self.theme = Some(now);
        }
        let area = div().size_full().overflow_hidden().px(px(8.)).py(px(4.));
        let Some(session) = self.session.clone() else {
            return area
                .id("terminal")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, event, _, cx| this.key_down(event, cx)))
                .text_color(muted)
                .child(self.error.clone().unwrap_or_default())
                .into_any_element();
        };
        let (grid, focus, measured) = (self.grid.clone(), self.focus.clone(), self.measured.clone());
        let (selection, origin) = (self.selection.clone(), self.origin.clone());
        let caret = palette.caret;
        area.id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, _, cx| this.key_down(event, cx)))
            .on_scroll_wheel(cx.listener(|this, event, _, cx| {
                this.wheel(event);
                cx.stop_propagation();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event, window, cx| this.press(event, window, cx)))
            .on_mouse_move(cx.listener(|this, event, _, cx| this.drag(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, event, _, cx| this.release(event, cx)))
            .child(
                canvas(
                    {
                        let (session, grid, measured, origin) = (session.clone(), grid.clone(), measured.clone(), origin.clone());
                        let family = family.clone();
                        move |bounds, window, _| {
                            origin.set(bounds.origin);
                            let (cell_w, cell_h) = match measured.take() {
                                Some((name, size, cell)) if name == family && size == font_size => {
                                    measured.set(Some((name, size, cell)));
                                    cell
                                }
                                _ => {
                                    let cell = cell_size(window, &family, font_size);
                                    measured.set(Some((family.clone(), font_size, cell)));
                                    cell
                                }
                            };
                            let cols = (f32::from(bounds.size.width) / cell_w).floor().max(1.0) as u16;
                            let rows = (f32::from(bounds.size.height) / cell_h).floor().max(1.0) as u16;
                            if grid.get() != (cols, rows, cell_w, cell_h) {
                                grid.set((cols, rows, cell_w, cell_h));
                                session.resize(cols, rows, (cell_w as u32, cell_h as u32));
                            }
                        }
                    },
                    move |bounds, (), window, cx| {
                        let (_, _, cell_w, cell_h) = grid.get();
                        paint(&session.frame(), selection.get(), bounds, (cell_w, cell_h), (&family, font_size), (caret, focus.is_focused(window)), window, cx);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

fn colors(palette: &Palette) -> (Rgb, Rgb) {
    let rgb = |c: Hsla| {
        let c = gpui::Rgba::from(c);
        Rgb((c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8)
    };
    (rgb(palette.foreground), rgb(palette.background))
}

fn from_rgb(c: Rgb) -> Hsla {
    gpui::rgb((u32::from(c.0) << 16) | (u32::from(c.1) << 8) | u32::from(c.2)).into()
}

fn font(family: &gpui::SharedString, bold: bool, italic: bool) -> Font {
    Font {
        family: family.clone(),
        features: gpui::FontFeatures::default(),
        fallbacks: None,
        weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
        style: if italic { FontStyle::Italic } else { FontStyle::Normal },
    }
}

/// How wide and tall one cell is in this font.
fn cell_size(window: &Window, family: &gpui::SharedString, font_size: Pixels) -> (f32, f32) {
    let run = TextRun { len: 1, font: font(family, false, false), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
    let width = window.text_system().shape_line("M".into(), font_size, &[run], None).width;
    (f32::from(width).max(1.0), (f32::from(font_size) * LINE_HEIGHT).round())
}

/// The cells of a row, each with what it shows: a wide character is in its first cell, and its
/// second is empty. Cells no run covers are blank.
fn row_cells(line: &[crate::terminal::Run], cols: u16) -> Vec<String> {
    let mut cells = vec![" ".to_string(); usize::from(cols)];
    for run in line {
        let glyphs = run.text.chars().count();
        if glyphs == usize::from(run.cells) {
            for (i, c) in run.text.chars().enumerate() {
                if let Some(cell) = cells.get_mut(usize::from(run.col) + i) {
                    *cell = c.to_string();
                }
            }
        } else if let Some(cell) = cells.get_mut(usize::from(run.col)) {
            *cell = run.text.clone();
            for i in 1..usize::from(run.cells) {
                if let Some(cell) = cells.get_mut(usize::from(run.col) + i) {
                    cell.clear();
                }
            }
        }
    }
    cells
}

/// The text between two cells of the frame, in reading order: rows end where the selection does or
/// where their text does, and are joined by newlines.
fn selected_text(frame: &Frame, a: (u16, u16), b: (u16, u16)) -> String {
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    let mut out = Vec::new();
    for row in start.1..=end.1 {
        let Some(line) = frame.lines.get(usize::from(row)) else { break };
        let cells = row_cells(line, frame.cols);
        let from = if row == start.1 { usize::from(start.0) } else { 0 };
        let to = if row == end.1 { usize::from(end.0) + 1 } else { cells.len() };
        let text: String = cells.get(from..to.min(cells.len())).unwrap_or_default().concat();
        out.push(text.trim_end().to_string());
    }
    out.join("\n")
}

fn paint(frame: &Frame, selection: Selection, bounds: Bounds<Pixels>, (cell_w, cell_h): (f32, f32), (family, font_size): (&gpui::SharedString, Pixels), (caret, focused): (Hsla, bool), window: &mut Window, cx: &mut App) {
    let origin = bounds.origin;
    let at = |col: u16, row: u16| point(origin.x + px(f32::from(col) * cell_w), origin.y + px(f32::from(row) * cell_h));
    let line_height = px(cell_h);
    let text = |run: &crate::terminal::Run, color: Hsla, window: &mut Window, cx: &mut App, row: u16| {
        let underline = run.underline.then(|| UnderlineStyle { thickness: px(1.), color: Some(color), wavy: false });
        let strikethrough = run.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(color) });
        // Every glyph on its cell, whatever font it falls back to: a run of one-cell characters
        // advances by a cell each, a wide character by two.
        let glyphs = run.text.chars().count();
        let advance = if glyphs == usize::from(run.cells) { Some(px(cell_w)) } else if glyphs == 1 { Some(px(f32::from(run.cells) * cell_w)) } else { None };
        let shaped = window.text_system().shape_line(
            run.text.clone().into(),
            font_size,
            &[TextRun { len: run.text.len(), font: font(family, run.bold, run.italic), color, background_color: None, underline, strikethrough }],
            advance,
        );
        let _ = shaped.paint(at(run.col, row), line_height, gpui::TextAlign::Left, None, window, cx);
    };
    if let Some((a, b)) = selection {
        let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
        for row in start.1..=end.1 {
            let from = if row == start.1 { start.0 } else { 0 };
            let to = if row == end.1 { end.0 + 1 } else { frame.cols };
            window.paint_quad(fill(Bounds::new(at(from, row), size(px(f32::from(to.saturating_sub(from)) * cell_w), line_height)), caret.opacity(0.35)));
        }
    }
    for (row, line) in frame.lines.iter().enumerate() {
        let row = row as u16;
        for run in line.iter() {
            if let Some(bg) = run.bg {
                window.paint_quad(fill(Bounds::new(at(run.col, row), size(px(f32::from(run.cells) * cell_w), line_height)), from_rgb(bg)));
            }
        }
        for run in line.iter() {
            text(run, from_rgb(run.fg), window, cx, row);
        }
    }
    let Some(cursor) = frame.cursor.as_ref() else { return };
    let cell = Bounds::new(at(cursor.col, cursor.row), size(px(cell_w), line_height));
    let thin = px(2.);
    match (cursor.shape, focused) {
        (CursorShape::Block, true) => {
            window.paint_quad(fill(cell, caret));
            // The character under it, in the background's color so it reads on the block.
            if !cursor.under.trim().is_empty() {
                let one = crate::terminal::Run { col: cursor.col, cells: 1, text: cursor.under.clone(), fg: frame.background, bg: None, bold: false, italic: false, underline: false, strike: false };
                text(&one, from_rgb(frame.background), window, cx, cursor.row);
            }
        }
        (CursorShape::Bar, true) => window.paint_quad(fill(Bounds::new(cell.origin, size(thin, cell.size.height)), caret)),
        (CursorShape::Underline, true) => window.paint_quad(fill(Bounds::new(point(cell.origin.x, cell.origin.y + cell.size.height - thin), size(cell.size.width, thin)), caret)),
        // Not focused, or asked for hollow: an outline.
        _ => {
            let (w, h) = (cell.size.width, cell.size.height);
            let one = px(1.);
            for edge in [
                Bounds::new(cell.origin, size(w, one)),
                Bounds::new(point(cell.origin.x, cell.origin.y + h - one), size(w, one)),
                Bounds::new(cell.origin, size(one, h)),
                Bounds::new(point(cell.origin.x + w - one, cell.origin.y), size(one, h)),
            ] {
                window.paint_quad(fill(edge, caret));
            }
        }
    }
}

/// The key a keystroke's name stands for.
fn key_of(name: &str) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A, Key::B, Key::C, Key::D, Key::E, Key::F, Key::G, Key::H, Key::I, Key::J, Key::K, Key::L, Key::M, Key::N, Key::O, Key::P, Key::Q, Key::R, Key::S, Key::T, Key::U, Key::V, Key::W, Key::X,
        Key::Y, Key::Z,
    ];
    const DIGITS: [Key; 10] = [Key::Digit0, Key::Digit1, Key::Digit2, Key::Digit3, Key::Digit4, Key::Digit5, Key::Digit6, Key::Digit7, Key::Digit8, Key::Digit9];
    const FUNCTION: [Key; 12] = [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let c = c.to_ascii_lowercase();
        return Some(match c {
            'a'..='z' => LETTERS[c as usize - 'a' as usize],
            '0'..='9' => DIGITS[c as usize - '0' as usize],
            '-' => Key::Minus,
            '=' => Key::Equal,
            '[' => Key::BracketLeft,
            ']' => Key::BracketRight,
            '\\' => Key::Backslash,
            ';' => Key::Semicolon,
            '\'' => Key::Quote,
            ',' => Key::Comma,
            '.' => Key::Period,
            '/' => Key::Slash,
            '`' => Key::Backquote,
            ' ' => Key::Space,
            _ => Key::Unidentified,
        });
    }
    Some(match name {
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "space" => Key::Space,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        _ => {
            let n: usize = name.strip_prefix('f')?.parse().ok()?;
            *FUNCTION.get(n.checked_sub(1)?)?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn selected_text_follows_the_cells_and_trims_the_ends_of_rows() {
        let run = |col, cells, text: &str| crate::terminal::Run { col, cells, text: text.into(), fg: Rgb(0, 0, 0), bg: None, bold: false, italic: false, underline: false, strike: false };
        let frame = Frame { cols: 8, rows: 2, lines: vec![Arc::new(vec![run(0, 2, "ab"), run(3, 2, "中"), run(6, 1, "x")]), Arc::new(vec![run(1, 2, "yz")])], ..Frame::default() };
        assert_eq!(selected_text(&frame, (0, 0), (7, 0)), "ab 中 x");
        assert_eq!(selected_text(&frame, (1, 0), (2, 1)), "b 中 x\n yz");
        // Dragged backwards, it is the same.
        assert_eq!(selected_text(&frame, (2, 1), (1, 0)), "b 中 x\n yz");
    }

    #[test]
    fn keys_are_named_the_way_the_encoder_wants() {
        assert_eq!(key_of("a"), Some(Key::A));
        assert_eq!(key_of("7"), Some(Key::Digit7));
        assert_eq!(key_of("enter"), Some(Key::Enter));
        assert_eq!(key_of("f5"), Some(Key::F5));
        assert_eq!(key_of("pageup"), Some(Key::PageUp));
        assert_eq!(key_of("f0"), None);
        assert_eq!(key_of("shift"), None);
    }
}
