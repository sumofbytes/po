//! The viewer's source pane on Rune: the open file as numbered lines coloured by rune-kit's
//! scanners, or a markdown file rendered. Counterpart of `src/nsh/source.rs`.
use std::cell::RefCell;
use std::collections::HashMap;
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

/// Bytes before a chunk that are coloured with it and not drawn from it, so a comment or string
/// that began shortly before the chunk is known to it. With a chunk, within the limit.
const LEAD: usize = 24 * 1024;

/// The colours of each line of a chunk of lines.
type Chunk = Vec<Vec<Highlight>>;

/// A file split into lines. Colours are made a chunk of lines at a time, when a row in it is
/// first drawn, so a big file is coloured where it is looked at and not only at its top. A chunk
/// is coloured from [`LEAD`] bytes before it, so a comment or string that began further back
/// than that is not known to it.
pub struct Lines {
    text: Arc<str>,
    extension: Option<String>,
    /// Where each line starts, and a last entry where the text ends.
    starts: Vec<usize>,
    /// The first line of each chunk, and a last entry for the line count.
    chunks: Vec<usize>,
    /// What each chunk's lines are coloured with, once made.
    marks: RefCell<Vec<Option<Rc<Chunk>>>>,
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

    /// The colours of each line of `chunk`, from the code of the chunk and the lines before it
    /// that are within [`LEAD`].
    fn colour(&self, chunk: usize) -> Chunk {
        let (first, end) = (self.chunks[chunk], self.chunks[chunk + 1]);
        let lead = self
            .starts
            .partition_point(|&at| at < self.starts[first].saturating_sub(LEAD))
            .min(first);
        let base = self.starts[lead];
        let code = &self.text[base..self.starts[end]];
        let found = syntax::highlight(self.extension.as_deref(), code);
        let mut marks = vec![Vec::new(); end - first];
        let mut line = lead;
        for (range, paint) in found.iter() {
            // Highlights come in order; one that crosses lines is cut at each.
            let mut from = range.start + base;
            let stop = range.end + base;
            while from < stop {
                while line + 1 < end && self.starts[line + 1] <= from {
                    line += 1;
                }
                let cut = stop.min(self.starts[line + 1]);
                // The lead-in is only read, not drawn.
                if line >= first {
                    marks[line - first]
                        .push((from - self.starts[line]..cut - self.starts[line], *paint));
                }
                from = cut;
            }
        }
        marks
    }
}

/// An edit of one file, begun in the viewer.
struct Draft {
    edit: TextEdit,
    /// What the file held when the edit began, or last matched the edit: what is unsaved is
    /// measured against it.
    base: Arc<str>,
    /// What the file holds now, when that is not `base` and the edit has changes of its own to
    /// lose: the file changed beneath it.
    disk: Option<Arc<str>>,
}

impl Draft {
    /// An edit of `text`, with the caret at the top, where the file is drawn from.
    fn new(text: &Arc<str>) -> Self {
        let mut edit = TextEdit::new(text.to_string());
        edit.anchor = 0;
        edit.focus = 0;
        Self {
            edit,
            base: text.clone(),
            disk: None,
        }
    }

    fn dirty(&self) -> bool {
        *self.edit.text != *self.base
    }

    /// Takes in what the file holds. The edit follows the file when it has no changes of its
    /// own; otherwise it keeps them, and says the file moved on.
    fn meet(&mut self, disk: &Arc<str>) {
        if Arc::ptr_eq(disk, &self.base) || **disk == *self.base {
            self.disk = None;
        } else if *self.edit.text == **disk {
            // The edit's own save, read back.
            self.base = disk.clone();
            self.disk = None;
        } else if self.dirty() {
            self.disk = Some(disk.clone());
        } else {
            self.reset(disk);
        }
    }

    /// The edit made again from `disk`, the caret kept where it was.
    fn reset(&mut self, disk: &Arc<str>) {
        let mut at = self.edit.focus.min(disk.len());
        while !disk.is_char_boundary(at) {
            at -= 1;
        }
        self.edit.replace_text(disk.to_string());
        self.edit.anchor = at;
        self.edit.focus = at;
        self.base = disk.clone();
        self.disk = None;
    }
}

/// What the host keeps for the pane: the lines of the file shown, and the edits begun.
#[derive(Default)]
pub struct State {
    shown: Option<(Arc<str>, Rc<Lines>)>,
    /// An edit begun, by the file it is of. It is kept while the pane shows another file or the
    /// file's rendered preview, and dropped when it has no changes and its file is not shown.
    drafts: HashMap<String, Draft>,
    /// What an edit with no draft behind it, a late one after Done, is made to.
    scratch: TextEdit,
}

pub type At<S> = fn(&mut S) -> &mut State;

impl State {
    /// Follows a new snapshot, splitting its file when it is a different one.
    pub fn follow(&mut self, snapshot: &Snapshot) {
        match &snapshot.body {
            Body::Code { path, text } => {
                if let Some(draft) = self.drafts.get_mut(path) {
                    draft.meet(&text.0);
                }
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
        // The title is the open file's path, whatever the body is of it.
        self.drafts
            .retain(|path, draft| draft.dirty() || *path == snapshot.title);
    }

    fn edit_of(&mut self, path: &str) -> &mut TextEdit {
        match self.drafts.get_mut(path) {
            Some(draft) => &mut draft.edit,
            None => &mut self.scratch,
        }
    }
}

/// Begins an edit of the file shown.
fn begin<S: 'static>(s: &mut S, at: At<S>, path: &str, text: &Arc<str>) {
    at(s).drafts.insert(path.to_string(), Draft::new(text));
    // A failed save is said while it is being edited, and not after.
    send(Cmd::ClearSaveError);
}

/// Ends the edit begun: what is typed is lost.
fn end<S: 'static>(s: &mut S, at: At<S>, path: &str) {
    at(s).drafts.remove(path);
    send(Cmd::ClearSaveError);
}

/// Saves what is typed. With the file changed beneath the edit it takes `over`, to overwrite
/// that; with nothing to save it does nothing, so the file is not written for no change.
fn save<S: 'static>(s: &mut S, at: At<S>, path: &str, over: bool) {
    let Some(draft) = at(s).drafts.get(path) else {
        return;
    };
    if draft.dirty() && (draft.disk.is_none() || over) {
        send(Cmd::SaveFile(path.to_string(), draft.edit.text.clone()));
    }
}

/// Takes the file as it is on disk, in place of what was typed.
fn reload<S: 'static>(s: &mut S, at: At<S>, path: &str) {
    if let Some(draft) = at(s).drafts.get_mut(path)
        && let Some(disk) = draft.disk.clone()
    {
        draft.reset(&disk);
    }
}

/// The bytes of `text` to colour with the caret at `focus`: all of it when small, else from
/// [`LEAD`] before the chunk the caret is in to a chunk and a half after its start, cut at line
/// ends. The window moves by chunk, so the highlighter's cache holds while the caret moves
/// within one.
fn window(text: &str, focus: usize) -> (usize, usize) {
    if text.len() <= CHUNK * 2 {
        return (0, text.len());
    }
    let bytes = text.as_bytes();
    let a = focus.min(text.len()) / CHUNK * CHUNK;
    let before = a.saturating_sub(LEAD);
    let after = (a + CHUNK + CHUNK / 2).min(text.len());
    let from = bytes[..before]
        .iter()
        .rposition(|&c| c == b'\n')
        .map_or(0, |i| i + 1);
    let to = bytes[after..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(text.len(), |i| after + i + 1);
    (from, to)
}

/// The colours of the text being edited. A big file is coloured only near the caret.
fn edit_spans(path: &str, edit: &TextEdit, theme: &SyntaxTheme) -> Vec<rune::Span> {
    let text = edit.text.as_str();
    let extension = path.rsplit('.').next().filter(|e| *e != path);
    let (from, to) = window(text, edit.focus);
    let marks = syntax::highlight(extension, &text[from..to]);
    theme
        .spans(&marks, from)
        .into_iter()
        .map(|s| s.mono())
        .collect()
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
        bar = bar.child(
            text(problem.clone())
                .font(caption_style())
                .color(k.c.text_muted),
        );
    }
    if let Body::Code { path, text: shown } = &snapshot.body {
        match state.drafts.get(path) {
            None => {
                let (path, shown) = (path.clone(), shown.0.clone());
                bar = bar.child(action::<S>(k, "Edit", "edit", false, move |s| {
                    begin(s, at, &path, &shown)
                }));
            }
            Some(draft) => {
                let (leave, name) = (path.clone(), if draft.dirty() { "Discard" } else { "Done" });
                bar = bar.child(action::<S>(k, name, "stop-edit", false, move |s| {
                    end(s, at, &leave)
                }));
                if draft.disk.is_some() {
                    // Both ways out are said: a save would write over what is on disk now.
                    let (to_reload, to_save) = (path.clone(), path.clone());
                    bar = bar
                        .child(
                            text("Changed on disk")
                                .font(caption_style())
                                .color(k.c.text_muted),
                        )
                        .child(action::<S>(k, "Reload", "reload", false, move |s| {
                            reload(s, at, &to_reload)
                        }))
                        .child(action::<S>(k, "Overwrite", "save", true, move |s| {
                            save(s, at, &to_save, true)
                        }));
                } else if draft.dirty() {
                    let to_save = path.clone();
                    bar = bar.child(action::<S>(k, "Save", "save", true, move |s| {
                        save(s, at, &to_save, false)
                    }));
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
        (Body::Code { path, .. }, _) if state.drafts.contains_key(path) => {
            let edit = &state.drafts[path].edit;
            let spans = edit_spans(path, edit, &theme);
            let (typed, keys) = (path.clone(), path.clone());
            let field = rune::text_input(edit, mono_style())
                .spans(spans)
                .key("source-edit")
                .w_full()
                .color(k.c.text_base)
                .caret(k.c.text_base, k.selection())
                .label("File contents")
                .on_edit(move |s: &mut S| at(s).edit_of(&typed))
                .on_input_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| {
                    // Only Ctrl/Cmd+S: Shift or Alt with it is another chord.
                    if event.is_command('s') && !event.mods.shift && !event.mods.alt {
                        save(s, at, &keys, false);
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
        assert_eq!(
            (lines.chunks[0], *lines.chunks.last().unwrap()),
            (0, 20_000)
        );
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
    fn a_big_text_is_coloured_from_before_its_chunk_by_line_ends() {
        let text = rust(20_000);
        let a = CHUNK * 2;
        let (from, to) = window(&text, a + 5);
        assert!(from > 0 && to < text.len());
        assert_eq!(text.as_bytes()[from - 1], b'\n');
        assert_eq!(text.as_bytes()[to - 1], b'\n');
        // From within a line of the lead-in, to a line past a chunk and a half.
        assert!(from <= a - LEAD && from + 32 > a - LEAD, "{from}");
        assert!(
            to >= a + CHUNK + CHUNK / 2 && to < a + CHUNK + CHUNK / 2 + 32,
            "{to}"
        );
        // Within the highlighter's limit.
        assert!(to - from <= 128 * 1024);
        // Within a chunk the window holds still, so the highlighter's cache does.
        assert_eq!(window(&text, a + 9_000), (from, to));
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

    #[test]
    fn a_comment_that_began_before_a_chunk_is_known_to_it() {
        // A block comment of 200 lines opens a few lines before the second chunk starts.
        let mut text = String::new();
        while text.len() < CHUNK - 100 {
            text.push_str("fn a() {}\n");
        }
        text.push_str("/*\n");
        for _ in 0..200 {
            text.push_str("not code at all\n");
        }
        text.push_str("*/\nfn z() {}\n");
        let lines = Lines::new("a.rs", Arc::from(text.as_str()));
        assert!(lines.chunks.len() > 2, "{:?}", lines.chunks);
        let second = lines.chunks[1];
        let marks = lines.colour(1);
        // The first line of the chunk is inside the comment: it is coloured whole, as a comment.
        let len = lines.line(second).len();
        assert!(
            marks[0].iter().any(|(range, _)| *range == (0..len)),
            "{:?} of {len}",
            marks[0]
        );
        // Without the lead-in, the same line is not known for a comment.
        let alone = syntax::highlight(Some("rs"), &text[lines.starts[second]..]);
        assert_ne!(alone[0].1, marks[0][0].1);
    }

    fn snapshot(path: &str, text: &Arc<str>) -> Snapshot {
        Snapshot {
            title: path.to_string(),
            body: Body::Code {
                path: path.to_string(),
                text: crate::model::source::Shared(text.clone()),
            },
            ..Snapshot::default()
        }
    }

    fn with_draft(path: &str, text: &str) -> (State, Arc<str>) {
        let disk: Arc<str> = Arc::from(text);
        let mut state = State::default();
        state.follow(&snapshot(path, &disk));
        state.drafts.insert(path.to_string(), Draft::new(&disk));
        (state, disk)
    }

    #[test]
    fn a_new_edit_has_nothing_to_save_and_its_caret_at_the_top() {
        let (state, _) = with_draft("a.rs", "fn a() {}\n");
        let draft = &state.drafts["a.rs"];
        assert!(!draft.dirty());
        assert_eq!((draft.edit.anchor, draft.edit.focus), (0, 0));
    }

    #[test]
    fn typing_makes_it_unsaved_and_undoing_it_by_hand_makes_it_clean_again() {
        let (mut state, _) = with_draft("a.rs", "abc");
        state.edit_of("a.rs").text.push('d');
        assert!(state.drafts["a.rs"].dirty());
        state.edit_of("a.rs").text.pop();
        assert!(!state.drafts["a.rs"].dirty());
    }

    #[test]
    fn a_file_changed_on_disk_under_a_clean_edit_is_followed() {
        let (mut state, _) = with_draft("a.rs", "one");
        let disk: Arc<str> = Arc::from("two");
        state.follow(&snapshot("a.rs", &disk));
        let draft = &state.drafts["a.rs"];
        assert_eq!(draft.edit.text, "two");
        assert!(draft.disk.is_none() && !draft.dirty());
    }

    #[test]
    fn a_file_changed_on_disk_under_typed_text_keeps_the_text_and_says_so() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.edit_of("a.rs").text.push_str(" mine");
        let disk: Arc<str> = Arc::from("two");
        state.follow(&snapshot("a.rs", &disk));
        let draft = &state.drafts["a.rs"];
        assert_eq!(draft.edit.text, "one mine");
        assert_eq!(draft.disk.as_deref(), Some("two"));
        assert!(draft.dirty());
    }

    #[test]
    fn reloading_takes_the_disk_text_and_clears_the_conflict() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.edit_of("a.rs").text.push_str(" mine");
        state.follow(&snapshot("a.rs", &Arc::from("two")));
        let disk = state.drafts["a.rs"].disk.clone().unwrap();
        state.drafts.get_mut("a.rs").unwrap().reset(&disk);
        let draft = &state.drafts["a.rs"];
        assert_eq!(draft.edit.text, "two");
        assert!(draft.disk.is_none() && !draft.dirty());
    }

    #[test]
    fn a_save_read_back_is_the_new_base_and_not_a_conflict() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.edit_of("a.rs").text.push_str(" more");
        state.follow(&snapshot("a.rs", &Arc::from("one more")));
        let draft = &state.drafts["a.rs"];
        assert!(draft.disk.is_none() && !draft.dirty());
        assert_eq!(&*draft.base, "one more");
    }

    #[test]
    fn the_same_text_arriving_again_is_no_conflict() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.edit_of("a.rs").text.push('!');
        state.follow(&snapshot("a.rs", &Arc::from("one")));
        assert!(state.drafts["a.rs"].disk.is_none());
        assert_eq!(state.drafts["a.rs"].edit.text, "one!");
    }

    #[test]
    fn typed_text_survives_the_pane_showing_something_else() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.edit_of("a.rs").text.push('!');
        // Another file, then the preview of nothing, then back.
        state.follow(&snapshot("b.rs", &Arc::from("b")));
        assert!(state.drafts.contains_key("a.rs"));
        state.follow(&Snapshot {
            title: "a.md".into(),
            body: Body::Notice("too large".into()),
            ..Snapshot::default()
        });
        assert_eq!(state.drafts["a.rs"].edit.text, "one!");
        state.follow(&snapshot("a.rs", &Arc::from("one")));
        assert_eq!(state.drafts["a.rs"].edit.text, "one!");
    }

    #[test]
    fn a_clean_edit_ends_when_its_file_is_no_longer_shown() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.follow(&snapshot("b.rs", &Arc::from("b")));
        assert!(state.drafts.is_empty());
    }

    #[test]
    fn a_late_edit_after_done_goes_to_no_draft() {
        let (mut state, _) = with_draft("a.rs", "one");
        state.drafts.clear();
        state.edit_of("a.rs").text.push_str("late");
        assert!(state.drafts.is_empty());
    }

    #[test]
    fn a_char_boundary_is_kept_when_the_disk_text_replaces_a_clean_edit() {
        let (mut state, _) = with_draft("a.txt", "héllo");
        state.drafts.get_mut("a.txt").unwrap().edit.focus = 2; // inside é? é is bytes 1..3
        state.follow(&snapshot("a.txt", &Arc::from("a")));
        let draft = &state.drafts["a.txt"];
        assert!(draft.edit.text.is_char_boundary(draft.edit.focus));
    }
}
