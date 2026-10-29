//! The terminal pane: a [`Session`]'s frames painted in the mono font of the theme, and the keys,
//! paste and wheel that go back to it.
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use futures::StreamExt as _;
use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, Font, FontStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Pixels, Render,
    ScrollWheelEvent, Styled as _, StrikethroughStyle, TextRun, UnderlineStyle, Window, canvas, div, fill, point, px, size,
};

use crate::terminal::{CursorShape, Frame, Key, KeyInput, Rgb, Session};
use crate::theme::Palette;

/// Wheel lines per notch when the wheel reports lines.
const WHEEL_LINES: f32 = 3.0;
/// Height of a line, as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.3;

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
}

impl TerminalView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self { root, focus: cx.focus_handle(), session: None, error: None, grid: Rc::new(Cell::new((80, 24, 8.0, 16.0))), measured: Rc::new(Cell::new(None)), theme: None, wheel_rest: 0.0 };
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
        if let Some(session) = &self.session {
            session.key(KeyInput { key, text, shift, alt, ctrl, meta });
        }
        cx.stop_propagation();
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
        let caret = palette.caret;
        area.id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, _, cx| this.key_down(event, cx)))
            .on_scroll_wheel(cx.listener(|this, event, _, cx| {
                this.wheel(event);
                cx.stop_propagation();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus(window, cx)))
            .child(
                canvas(
                    {
                        let (session, grid, measured) = (session.clone(), grid.clone(), measured.clone());
                        let family = family.clone();
                        move |bounds, window, _| {
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
                        paint(&session.frame(), bounds, (cell_w, cell_h), (&family, font_size), (caret, focus.is_focused(window)), window, cx);
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

fn paint(frame: &Frame, bounds: Bounds<Pixels>, (cell_w, cell_h): (f32, f32), (family, font_size): (&gpui::SharedString, Pixels), (caret, focused): (Hsla, bool), window: &mut Window, cx: &mut App) {
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
