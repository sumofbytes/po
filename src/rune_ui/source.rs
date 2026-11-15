//! The viewer's source pane on Rune: the open file as numbered lines coloured by rune-kit's
//! scanners, or a markdown file rendered. Counterpart of `src/nsh/source.rs`.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rune::tok::TextStyle;
use rune::{El, KeyEvent, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::Kit;
use rune_kit::markdown::Markdown;
use rune_kit::syntax::{self, Highlight, SyntaxTheme};

use crate::model::bus::{Cmd, send};
use crate::model::source::{Body, Snapshot};

fn mono_style() -> TextStyle {
    crate::config::fonts::mono(13.0, 19.0)
}
fn caption_style() -> TextStyle {
    crate::config::fonts::ui(12.0, 400.0)
}
fn name_style() -> TextStyle {
    crate::config::fonts::ui(13.0, 400.0)
}
const LINE: f32 = 19.0;

/// Bytes a chunk of lines is coloured over: under the highlighter's own limit, which leaves
/// what is past it plain.
const CHUNK: usize = 64 * 1024;

/// A file split into lines. Colours are made a chunk of lines at a time, when a row in it is
/// first drawn, so a big file is coloured where it is looked at and not only at its top. Code
/// is coloured from the start of its chunk, so a comment or string that began in an earlier
/// chunk is not known to it.
pub struct Lines {
    text: Arc<str>,
    extension: Option<String>,
    /// Where each line starts, and a last entry where the text ends.
    starts: Vec<usize>,
    /// The first line of each chunk, and a last entry for the line count.
    chunks: Vec<usize>,
    /// What each chunk's lines are coloured with, once made.
    marks: RefCell<Vec<Option<Rc<Vec<Vec<Highlight>>>>>>,
}

impl Lines {
    pub fn new(path: &str, text: Arc<str>) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        // A final newline ends the last line rather than starting an empty one.
        if starts.last() == Some(&text.len()) && starts.len() > 1 {
            starts.pop();
        }
        starts.push(text.len());
        let lines = starts.len() - 1;
        let mut chunks = vec![0];
        for line in 1..lines {
            if starts[line] - starts[*chunks.last().unwrap()] >= CHUNK {
                chunks.push(line);
            }
        }
        chunks.push(lines);
        let extension = path
            .rsplit('.')
            .next()
            .filter(|e| *e != path)
            .map(str::to_string);
        Self {
            text,
            extension,
            marks: RefCell::new(vec![None; chunks.len() - 1]),
            starts,
            chunks,
        }
    }

    pub fn len(&self) -> usize {
        self.starts.len() - 1
    }

    fn line(&self, i: usize) -> &str {
        self.text[self.starts[i]..self.starts[i + 1]].trim_end_matches(['\n', '\r'])
    }

    /// The colours of line `i`, made with the rest of its chunk if they are not yet.
    fn spans(&self, i: usize, theme: &SyntaxTheme) -> Vec<rune::Span> {
        let chunk = self.chunks.partition_point(|&first| first <= i) - 1;
        let made = self.marks.borrow()[chunk].clone();
        let made = made.unwrap_or_else(|| {
            let made = Rc::new(self.colour(chunk));
            self.marks.borrow_mut()[chunk] = Some(made.clone());
            made
        });
        theme
            .spans(&made[i - self.chunks[chunk]], 0)
            .into_iter()
            .map(|s| s.mono())
            .collect()
    }

    /// The colours of each line of `chunk`, from the code of the chunk alone.
    fn colour(&self, chunk: usize) -> Vec<Vec<Highlight>> {
        let (first, end) = (self.chunks[chunk], self.chunks[chunk + 1]);
        let base = self.starts[first];
        let code = &self.text[base..self.starts[end]];
        let found = syntax::highlight(self.extension.as_deref(), code);
        let mut marks = vec![Vec::new(); end - first];
        let mut line = first;
        for (range, paint) in found.iter() {
            // Highlights come in order; one that crosses lines is cut at each.
            let mut from = range.start + base;
            let stop = range.end + base;
            while from < stop {
                while line + 1 < end && self.starts[line + 1] <= from {
                    line += 1;
                }
                let cut = stop.min(self.starts[line + 1]);
                marks[line - first]
                    .push((from - self.starts[line]..cut - self.starts[line], *paint));
                from = cut;
            }
        }
        marks
    }
}

/// What the host keeps for the pane: the lines of the file shown, and the file they are of.
#[derive(Default)]
pub struct State {
    shown: Option<(Arc<str>, Rc<Lines>)>,
    /// The file being edited, and what is typed into it. `None` while the file is only read.
    editing: Option<(String, TextEdit)>,
}

pub type At<S> = fn(&mut S) -> &mut State;

impl State {
    /// Follows a new snapshot, splitting its file when it is a different one.
    pub fn follow(&mut self, snapshot: &Snapshot) {
        // Editing goes on while the file is on screen, and ends when another is opened. What is
        // typed is kept when the file changes on disk beneath it.
        if let Some((editing, _)) = &self.editing
            && !matches!(&snapshot.body, Body::Code { path, .. } if path == editing)
        {
            self.editing = None;
        }
        match &snapshot.body {
            Body::Code { path, text } => {
                if !self
                    .shown
                    .as_ref()
                    .is_some_and(|(held, _)| Arc::ptr_eq(held, &text.0))
                {
                    self.shown = Some((text.0.clone(), Rc::new(Lines::new(path, text.0.clone()))));
                }
            }
            _ => self.shown = None,
        }
    }
}

impl State {
    fn edit(&mut self) -> &mut TextEdit {
        &mut self.editing.get_or_insert_with(Default::default).1
    }
}

/// Saves what is typed, when it differs from the file.
fn save<S: 'static>(s: &mut S, at: At<S>) {
    let state = at(s);
    let Some((path, edit)) = &state.editing else {
        return;
    };
    send(Cmd::SaveFile(path.clone(), edit.text.clone()));
}

/// The bytes of `text` to colour with the caret at `focus`: all of it when small, else the
/// chunk the caret is in and the next, cut at line ends.
fn window(text: &str, focus: usize) -> (usize, usize) {
    if text.len() <= CHUNK * 2 {
        return (0, text.len());
    }
    let bytes = text.as_bytes();
    let a = focus.min(text.len()) / CHUNK * CHUNK;
    let b = (a + CHUNK * 2).min(text.len());
    let from = bytes[..a].iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
    let to = bytes[b..].iter().position(|&c| c == b'\n').map_or(text.len(), |i| b + i + 1);
    (from, to)
}

/// The colours of the text being edited. A big file is coloured only near the caret, in the
/// chunks it is in and the next, cut at line ends: the window moves by chunk, so the
/// highlighter's cache holds while the caret moves within one.
fn edit_spans(path: &str, edit: &TextEdit, theme: &SyntaxTheme) -> Vec<rune::Span> {
    let text = edit.text.as_str();
    let extension = path.rsplit('.').next().filter(|e| *e != path);
    let (from, to) = window(text, edit.focus);
    let marks = syntax::highlight(extension, &text[from..to]);
    theme.spans(&marks, from).into_iter().map(|s| s.mono()).collect()
}

fn centered(k: &Kit, message: &str) -> El {
    col().w_full().grow(1.0).center().padding(16.0).child(
        text(message.to_string())
            .font(caption_style())
            .color(k.c.text_muted),
    )
}

fn pill<S: 'static>(k: &Kit, label: &'static str, key: &'static str, on: bool, cmd: Cmd) -> El {
    action::<S>(k, label, key, on, move |_| send(cmd.clone()))
}

fn action<S: 'static>(
    k: &Kit,
    label: &'static str,
    key: &'static str,
    on: bool,
    press: impl Fn(&mut S) + 'static,
) -> El {
    let r = row()
        .key(key)
        .px(8.0)
        .py(2.0)
        .radius(6.0)
        .child(text(label).font(caption_style()).color(k.c.text_base))
        .on_press(press);
    k.interactive(if on { r.fill(k.c.layer_03) } else { r })
}

fn header<S: 'static>(k: &Kit, snapshot: &Snapshot, state: &State, at: At<S>) -> El {
    let mut bar = row()
        .w_full()
        .h(super::header::BAND)
        .items_center()
        .gap(8.0)
        .px(12.0)
        .child(super::header::controls::<S>(k, &snapshot.lead));
    bar = bar.child(
        row().grow(1.0).min_w(0.0).child(
            text(snapshot.title.clone())
                .font(name_style())
                .color(k.c.text_base),
        ),
    );
    if !snapshot.language.is_empty() {
        bar = bar.child(
            text(snapshot.language.clone())
                .font(caption_style())
                .color(k.c.text_muted),
        );
    }
    if let Some(problem) = &snapshot.problem {
        bar = bar.child(text(problem.clone()).font(caption_style()).color(k.c.text_muted));
    }
    if let Body::Code { path, text: shown } = &snapshot.body {
        match &state.editing {
            None => {
                let (path, shown) = (path.clone(), shown.0.clone());
                bar = bar.child(action::<S>(k, "Edit", "edit", false, move |s| {
                    // The caret starts at the top, where the file is drawn from.
                    let mut edit = TextEdit::new(shown.to_string());
                    edit.anchor = 0;
                    edit.focus = 0;
                    at(s).editing = Some((path.clone(), edit));
                }));
            }
            Some((_, edit)) => {
                let dirty = edit.text != *shown.0;
                bar = bar.child(action::<S>(
                    k,
                    if dirty { "Discard" } else { "Done" },
                    "stop-edit",
                    false,
                    move |s| at(s).editing = None,
                ));
                if dirty {
                    bar = bar.child(action::<S>(k, "Save", "save", true, move |s| save(s, at)));
                }
            }
        }
    }
    if let Some(rendered) = snapshot.preview {
        bar = bar.child(pill::<S>(
            k,
            if rendered { "Source" } else { "Preview" },
            "preview",
            false,
            Cmd::ToggleRendered,
        ));
    }
    super::header::ruled(
        k,
        bar.child(super::header::controls::<S>(k, &snapshot.trail)),
    )
}

pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>) -> El {
    let theme = SyntaxTheme::from_kit(k);
    let body = match (&snapshot.body, &state.shown) {
        (Body::Hint(message) | Body::Notice(message), _) => centered(k, message),
        (Body::Markdown(source), _) => col().w_full().grow(1.0).min_h(0.0).child(
            k.scroll_bar(
                col()
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .padding(16.0)
                    .child(
                        Markdown::new(source.0.to_string())
                            .style(super::book_style(k))
                            .selectable(true)
                            .build(k),
                    ),
            ),
        ),
        (Body::Code { .. }, _) if state.editing.is_some() => {
            let (path, edit) = state.editing.as_ref().unwrap();
            let spans = edit_spans(path, edit, &theme);
            let field = rune::text_input(edit, mono_style())
                .spans(spans)
                .key("source-edit")
                .w_full()
                .color(k.c.text_base)
                .caret(k.c.text_base, k.selection())
                .label("File contents")
                .on_edit(move |s: &mut S| at(s).edit())
                .on_input_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| {
                    if event.is_command('s') {
                        save(s, at);
                        return true;
                    }
                    false
                });
            col().w_full().grow(1.0).min_h(0.0).child(
                k.scroll_bar(
                    col()
                        .w_full()
                        .grow(1.0)
                        .min_h(0.0)
                        .scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        .padding(8.0)
                        .child(field),
                ),
            )
        }
        (Body::Code { .. }, Some((_, lines))) => {
            let lines = lines.clone();
            let width = lines.len().to_string().len() as f32 * 8.0 + 16.0;
            let (muted, base) = (k.c.text_muted, k.c.text_base);
            let rows = col()
                .key("source-lines")
                .w_full()
                .growth(rune::GrowthSpec::default())
                .virtual_rows(
                    lines.len(),
                    LINE,
                    |_: &S, i| i.to_string().into(),
                    move |_: &S, i| {
                        let spans = lines.spans(i, &theme);
                        row()
                            .w_full()
                            .h(LINE)
                            .child(
                                text((i + 1).to_string())
                                    .font(mono_style())
                                    .color(muted)
                                    .w(width)
                                    .px(4.0),
                            )
                            .child(
                                text(lines.line(i).to_string())
                                    .font(mono_style())
                                    .color(base)
                                    .spans(spans),
                            )
                    },
                );
            col().w_full().grow(1.0).min_h(0.0).child(
                k.scroll_bar(
                    col()
                        .w_full()
                        .grow(1.0)
                        .min_h(0.0)
                        .scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        .child(rows),
                ),
            )
        }
        (Body::Code { .. }, None) => centered(k, ""),
    };
    col()
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header::<S>(k, snapshot, state, at))
        .child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust(lines: usize) -> String {
        (0..lines).map(|i| format!("fn f{i}() {{}}\n")).collect()
    }

    #[test]
    fn a_small_file_is_one_chunk() {
        let lines = Lines::new("a.rs", Arc::from(rust(10)));
        assert_eq!(lines.len(), 10);
        assert_eq!(lines.chunks, vec![0, 10]);
        assert_eq!(lines.line(3), "fn f3() {}");
    }

    #[test]
    fn a_big_file_is_cut_into_chunks_that_cover_every_line_once() {
        let text = rust(20_000);
        let lines = Lines::new("a.rs", Arc::from(text.as_str()));
        assert_eq!(lines.len(), 20_000);
        assert!(lines.chunks.len() > 3, "{:?}", lines.chunks);
        assert_eq!((lines.chunks[0], *lines.chunks.last().unwrap()), (0, 20_000));
        assert!(lines.chunks.windows(2).all(|w| w[0] < w[1]));
        for w in lines.chunks.windows(2) {
            assert!(lines.starts[w[1]] - lines.starts[w[0]] <= CHUNK + 32);
        }
    }

    #[test]
    fn a_chunk_far_down_is_coloured_on_its_own() {
        let lines = Lines::new("a.rs", Arc::from(rust(20_000)));
        let last = lines.chunks.len() - 2;
        let marks = lines.colour(last);
        assert_eq!(marks.len(), lines.chunks[last + 1] - lines.chunks[last]);
        assert!(marks.iter().all(|line| !line.is_empty()));
        for (n, line) in marks.iter().enumerate() {
            let len = lines.line(lines.chunks[last] + n).len();
            assert!(line.iter().all(|(range, _)| range.end <= len));
        }
        // Nothing was made for the chunks not asked for.
        assert!(lines.marks.borrow().iter().all(Option::is_none));
    }

    #[test]
    fn a_comment_across_lines_is_cut_at_each() {
        let lines = Lines::new("a.rs", Arc::from("/* one\ntwo */\nfn a() {}\n"));
        let marks = lines.colour(0);
        assert!(!marks[0].is_empty() && !marks[1].is_empty());
        assert_eq!(marks[0][0].0, 0..6);
        assert_eq!(marks[1][0].0, 0..6);
    }

    #[test]
    fn a_small_text_is_coloured_whole() {
        let text = rust(100);
        assert_eq!(window(&text, 40), (0, text.len()));
    }

    #[test]
    fn a_big_text_is_coloured_in_two_chunks_from_a_line_start_to_a_line_end() {
        let text = rust(20_000);
        let (from, to) = window(&text, CHUNK * 2 + 5);
        assert!(from > 0 && to < text.len());
        assert_eq!(text.as_bytes()[from - 1], b'\n');
        assert_eq!(text.as_bytes()[to - 1], b'\n');
        assert!(from <= CHUNK * 2 && to >= CHUNK * 4);
        // Within a chunk the window holds still, so the highlighter's cache does.
        assert_eq!(window(&text, CHUNK * 2 + 9_000), (from, to));
        // The end of the text clamps.
        let (_, to) = window(&text, text.len());
        assert_eq!(to, text.len());
    }

    #[test]
    fn a_window_over_multibyte_text_stays_on_characters() {
        let text = "héllo wörld ✓\n".repeat(20_000);
        let (from, to) = window(&text, CHUNK * 3 + 1);
        assert!(text.is_char_boundary(from) && text.is_char_boundary(to));
    }
}
