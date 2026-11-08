//! The viewer's diff pane on Rune: one changed file, old against new, drawn by rune-kit's
//! `DiffView` (word changes, folded context, split or inline). Counterpart of `src/nsh/diff.rs`.
use std::sync::Arc;

use rune::tok::TextStyle;
use rune::{El, Tx, col, row, text};
use rune_kit::{Kit, SegmentedControl};
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};

use crate::model::bus::{Cmd, send};
use crate::model::diff::{Body, Snapshot};

const CAPTION: TextStyle = TextStyle::new(12.0, 400.0);
const NAME: TextStyle = TextStyle::new(13.0, 400.0);

/// The diff the pane draws, and the two versions it was made from.
#[derive(Default)]
pub struct State {
    pub diff: DiffState,
    from: Option<(Arc<str>, Arc<str>)>,
}

impl State {
    /// Follows a new snapshot: compares the versions again only when they are others.
    pub fn follow(&mut self, snapshot: &Snapshot) {
        self.diff.set_mode(if snapshot.split { DiffMode::Split } else { DiffMode::Unified });
        match &snapshot.body {
            Body::Versions { path, old, new } => {
                let same = self.from.as_ref().is_some_and(|(o, n)| Arc::ptr_eq(o, &old.0) && Arc::ptr_eq(n, &new.0));
                if !same {
                    self.diff.set_files(vec![FileDiff::from_texts(path.clone(), &old.0, &new.0)]);
                    self.from = Some((old.0.clone(), new.0.clone()));
                }
            }
            _ => self.from = None,
        }
    }
}

fn centered(k: &Kit, message: &str) -> El {
    col().w_full().grow(1.0).center().padding(16.0).child(text(message.to_string()).font(CAPTION).color(k.c.text_muted))
}

/// `get` and `get_mut` find the pane's [`State`] in the host's.
pub fn view<S: 'static>(snapshot: &Snapshot, host: &S, k: &Kit, get: fn(&S) -> &State, get_mut: fn(&mut S) -> &mut State) -> El {
    let (added, removed) = snapshot.counts;
    let bar = super::header::ruled(k, row()
        .w_full()
        .items_center()
        .gap(8.0)
        .px(12.0)
        .h(super::header::BAND)
        .child(super::header::controls::<S>(k, &snapshot.lead))
        .child(row().grow(1.0).min_w(0.0).child(text(snapshot.title.clone()).font(NAME).color(k.c.text_base)))
        .child(text(format!("+{added}")).font(CAPTION).color(k.c.success.fg))
        .child(text(format!("-{removed}")).font(CAPTION).color(k.c.danger.fg))
        .child(SegmentedControl::new(&["Inline", "Split"], Some(usize::from(snapshot.split))).label("Diff layout").fit().on_pick(|_: &mut S, i: usize, _: &mut Tx| send(Cmd::SetSplit(i == 1))).build(k))
        .child(super::header::controls::<S>(k, &snapshot.trail)));
    let body = match &snapshot.body {
        Body::Hint(message) => centered(k, message),
        Body::NoChanges => centered(k, "No textual changes"),
        Body::Loading => col().w_full().grow(1.0),
        Body::Versions { .. } => DiffView::new(move |s: &S| &get(s).diff, move |s: &mut S| &mut get_mut(s).diff).build(host, k),
    };
    col().w_full().h_full().min_h(0.0).child(bar).child(body)
}
