//! The Ctrl+P finder: a field and the files that match it. Counterpart of `src/nsh/finder.rs`.
use rune::tok::TextStyle;
use rune::{El, Key, KeyEvent, TextEdit, Tx, col, row, text};
use rune_kit::{Kit, TextInput};

use crate::model::bus::{Cmd, send};
use crate::model::finder::Snapshot;

fn name_style() -> TextStyle {
    crate::fonts::ui(13.0, 400.0)
}
fn caption_style() -> TextStyle {
    crate::fonts::ui(12.0, 400.0)
}
/// The most matches drawn: the list is as long as the repository.
const SHOWN: usize = 12;

pub struct State {
    pub query: TextEdit,
    /// The row the arrow keys are on.
    pub at: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            query: TextEdit::new(""),
            at: 0,
        }
    }
}

/// Where the host keeps the finder's state.
pub type At<S> = fn(&mut S) -> &mut State;

/// A path as the folder in muted type and the file's name after it.
fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>) -> El {
    let matches = snapshot.matches.clone();
    let field = TextInput::new(&state.query)
        .label("Find a file")
        .placeholder("Find a file")
        .width(None)
        .on_edit(move |s: &mut S| &mut at(s).query)
        .on_change(move |s: &mut S, _: &mut Tx| {
            at(s).at = 0;
            send(Cmd::FinderQuery(at(s).query.text.clone()));
        })
        .on_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| {
            let last = matches.len().min(SHOWN).saturating_sub(1);
            match event.key {
                Key::Down => at(s).at = (at(s).at + 1).min(last),
                Key::Up => at(s).at = at(s).at.saturating_sub(1),
                Key::Enter => match matches.get(at(s).at) {
                    Some(path) => send(Cmd::FinderPick(path.clone())),
                    None => return false,
                },
                Key::Escape => send(Cmd::CloseFinder),
                _ => return false,
            }
            true
        })
        .build(k);

    let mut list = col().w_full().px(4.0).pb(4.0);
    for (i, path) in snapshot.matches.iter().take(SHOWN).enumerate() {
        let (folder, name) = split(path);
        let pick = path.clone();
        let line = row()
            .key(format!("match-{i}"))
            .w_full()
            .items_center()
            .h(28.0)
            .px(8.0)
            .radius(6.0)
            .child(
                text(name.to_string())
                    .font(name_style())
                    .color(k.c.text_base),
            )
            .child(
                text(format!("  {folder}"))
                    .font(caption_style())
                    .color(k.c.text_muted),
            )
            .on_press(move |_: &mut S| send(Cmd::FinderPick(pick.clone())));
        list = list.child(if i == state.at {
            k.interactive(line.fill(k.c.layer_03))
        } else {
            k.interactive(line)
        });
    }
    if let Some(cap) = snapshot.capped {
        list = list.child(
            text(format!("Only the first {cap} files are searched"))
                .font(caption_style())
                .color(k.c.text_muted)
                .px(8.0)
                .pt(4.0),
        );
    }
    col()
        .w_full()
        .gap(8.0)
        .padding(8.0)
        .child(field)
        .child(list)
}
