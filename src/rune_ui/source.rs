//! The viewer's source pane on Rune: the open file as numbered lines coloured by rune-kit's
//! scanners, or a markdown file rendered. Counterpart of `src/nsh/source.rs`.
use std::rc::Rc;
use std::sync::Arc;

use rune::tok::{Family, TextStyle};
use rune::{El, ScrollSpec, col, row, text};
use rune_kit::syntax::{self, Highlight, SyntaxTheme};
use rune_kit::Kit;
use rune_kit::markdown::Markdown;

use crate::model::bus::{Cmd, send};
use crate::model::source::{Body, Snapshot};

const MONO: TextStyle = TextStyle { family: Family::SystemMono, line_height: 19.0, ..TextStyle::new(13.0, 400.0) };
const CAPTION: TextStyle = TextStyle::new(12.0, 400.0);
const NAME: TextStyle = TextStyle::new(13.0, 400.0);
const LINE: f32 = 19.0;

/// A file split into lines, each with the colours of its own bytes: made once when the file
/// arrives, so drawing a row costs a slice.
pub struct Lines {
    text: Arc<str>,
    /// Where each line starts, and a last entry where the text ends.
    starts: Vec<usize>,
    marks: Vec<Vec<Highlight>>,
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
        let extension = path.rsplit('.').next().filter(|e| *e != path);
        let found = syntax::highlight(extension, &text);
        let mut marks = vec![Vec::new(); starts.len() - 1];
        let mut line = 0;
        for (range, paint) in found.iter() {
            // Highlights come in order; one that crosses lines is cut at each.
            let mut from = range.start;
            while from < range.end {
                while line + 1 < marks.len() && starts[line + 1] <= from {
                    line += 1;
                }
                let end = range.end.min(starts[line + 1]);
                marks[line].push((from - starts[line]..end - starts[line], *paint));
                from = end;
            }
        }
        Self { text, starts, marks }
    }

    pub fn len(&self) -> usize {
        self.marks.len()
    }

    fn line(&self, i: usize) -> &str {
        self.text[self.starts[i]..self.starts[i + 1]].trim_end_matches(['\n', '\r'])
    }
}

/// What the host keeps for the pane: the lines of the file shown, and the file they are of.
#[derive(Default)]
pub struct State {
    shown: Option<(Arc<str>, Rc<Lines>)>,
}

impl State {
    /// Follows a new snapshot, splitting its file when it is a different one.
    pub fn follow(&mut self, snapshot: &Snapshot) {
        match &snapshot.body {
            Body::Code { path, text } => {
                if !self.shown.as_ref().is_some_and(|(held, _)| Arc::ptr_eq(held, &text.0)) {
                    self.shown = Some((text.0.clone(), Rc::new(Lines::new(path, text.0.clone()))));
                }
            }
            _ => self.shown = None,
        }
    }
}

fn centered(k: &Kit, message: &str) -> El {
    col().w_full().grow(1.0).center().padding(16.0).child(text(message.to_string()).font(CAPTION).color(k.c.text_muted))
}

fn pill<S: 'static>(k: &Kit, label: &'static str, key: &'static str, on: bool, cmd: Cmd) -> El {
    let r = row().key(key).px(8.0).py(2.0).radius(6.0).child(text(label).font(CAPTION).color(k.c.text_base)).on_press(move |_: &mut S| send(cmd.clone()));
    k.interactive(if on { r.fill(k.c.layer_03) } else { r })
}

fn header<S: 'static>(k: &Kit, snapshot: &Snapshot) -> El {
    let mut bar = row().w_full().items_center().gap(8.0).px(12.0).py(8.0).child(super::header::controls::<S>(k, &snapshot.lead));
    bar = bar.child(row().grow(1.0).min_w(0.0).child(text(snapshot.title.clone()).font(NAME).color(k.c.text_base)));
    if !snapshot.language.is_empty() {
        bar = bar.child(text(snapshot.language.clone()).font(CAPTION).color(k.c.text_muted));
    }
    if let Some(rendered) = snapshot.preview {
        bar = bar.child(pill::<S>(k, if rendered { "Source" } else { "Preview" }, "preview", false, Cmd::ToggleRendered));
    }
    bar.child(super::header::controls::<S>(k, &snapshot.trail))
}

pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit) -> El {
    let theme = SyntaxTheme::from_kit(k);
    let body = match (&snapshot.body, &state.shown) {
        (Body::Hint(message) | Body::Notice(message), _) => centered(k, message),
        (Body::Markdown(source), _) => col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(k.scroll_bar(col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).padding(16.0).child(Markdown::new(source.0.to_string()).selectable(true).build(k)))),
        (Body::Code { .. }, Some((_, lines))) => {
            let lines = lines.clone();
            let width = lines.len().to_string().len() as f32 * 8.0 + 16.0;
            let (muted, base) = (k.c.text_muted, k.c.text_base);
            let rows = col().key("source-lines").w_full().growth(rune::GrowthSpec::default()).virtual_rows(
                lines.len(),
                LINE,
                |_: &S, i| i.to_string().into(),
                move |_: &S, i| {
                    let spans = theme.spans(&lines.marks[i], 0).into_iter().map(|s| s.mono()).collect::<Vec<_>>();
                    row()
                        .w_full()
                        .h(LINE)
                        .child(text((i + 1).to_string()).font(MONO).color(muted).w(width).px(4.0))
                        .child(text(lines.line(i).to_string()).font(MONO).color(base).spans(spans))
                },
            );
            col().w_full().grow(1.0).min_h(0.0).child(k.scroll_bar(col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(rows)))
        }
        (Body::Code { .. }, None) => centered(k, ""),
    };
    col().w_full().h_full().min_h(0.0).child(header::<S>(k, snapshot)).child(body)
}
