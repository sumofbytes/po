//! The file tree beside the viewer, with the strips above it (a PR review's field, a worktree's
//! finish line). Counterpart of `src/nsh/explorer.rs` and `strip.rs`: it draws the snapshots and
//! sends [`Cmd`]s.
use rune::tok::TextStyle;
use rune::{El, Key, KeyEvent, TextEdit, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, Kit, TextInput};

use crate::explorer::Step;
use crate::model::bus::{Cmd, send};
use crate::model::explorer::{Body, Entry, Snapshot};
use crate::model::strip::{Pr, Snapshot as Strips};

const NAME: TextStyle = TextStyle::new(13.0, 400.0);
const CAPTION: TextStyle = TextStyle::new(12.0, 400.0);
const INDENT: f32 = 14.0;

/// What the explorer keeps between frames: the PR field.
pub struct State {
    pub pr: TextEdit,
    /// The strip's `pr_reset` the field was last emptied for.
    seen_reset: u64,
}

impl Default for State {
    fn default() -> Self {
        Self { pr: TextEdit::new(""), seen_reset: 0 }
    }
}

impl State {
    /// Empties the PR field when the strip says one opened since, as the NSH screen does.
    pub fn follow(&mut self, strips: &Strips) {
        if strips.pr_reset != self.seen_reset {
            self.seen_reset = strips.pr_reset;
            self.pr = TextEdit::new("");
        }
    }
}

/// Where the host keeps the explorer's state.
pub type At<S> = fn(&mut S) -> &mut State;

fn mark_color(k: &Kit, mark: char) -> rune::Color {
    match mark {
        'A' => k.c.success.fg,
        'D' => k.c.danger.fg,
        _ => k.c.warning.fg,
    }
}

fn entry_row<S: 'static>(k: &Kit, e: &Entry) -> El {
    let id = e.row.id.clone();
    let glyph = if e.row.folder {
        if e.row.expanded { "▾" } else { "▸" }
    } else {
        " "
    };
    let mut line = row()
        .key(format!("entry-{}", e.row.id))
        .w_full()
        .items_center()
        .gap(6.0)
        .h(26.0)
        .pl(8.0 + INDENT * e.row.depth as f32)
        .pr(8.0)
        .radius(6.0)
        .child(text(glyph).font(NAME).color(k.c.text_muted).w(10.0))
        .child(row().grow(1.0).min_w(0.0).child(text(e.row.name.clone()).font(NAME).color(if e.row.selected { k.c.text_base } else { k.c.text_muted })))
        .on_press(move |_: &mut S| send(Cmd::TreeClick(id.clone())));
    if let Some(mark) = e.mark {
        line = line.child(text(mark.to_string()).font(CAPTION).color(mark_color(k, mark)));
    }
    if e.row.selected {
        line = line.fill(k.c.layer_03);
    }
    k.interactive(line)
}

fn step_of(event: &KeyEvent) -> Option<Step> {
    match event.key {
        Key::Up => Some(Step::Up),
        Key::Down => Some(Step::Down),
        Key::Left => Some(Step::Left),
        Key::Right => Some(Step::Right),
        _ => None,
    }
}

/// The strips above the tree: the worktree's finish line, and the PR review's field or summary.
pub fn strips<S: 'static>(strips: &Strips, state: &State, k: &Kit, at: At<S>) -> El {
    let mut out = col().w_full().gap(6.0).px(8.0);
    if let Some(wt) = &strips.worktree {
        let discard = if wt.confirm_discard { "Discard, for sure?" } else { "Discard" };
        out = out.child(
            row()
                .w_full()
                .items_center()
                .gap(6.0)
                .child(row().grow(1.0).min_w(0.0).child(text(wt.branch.clone()).font(CAPTION).color(k.c.text_muted)))
                .child(Button::new("Merge").variant(ButtonVariant::Submit).on_press(|_: &mut S| send(Cmd::MergeWorktree)).build(k))
                .child(Button::new(discard).variant(ButtonVariant::Danger).on_press(|_: &mut S| send(Cmd::DiscardWorktree)).build(k)),
        );
    }
    match &strips.pr {
        Some(Pr::Opened { label }) => {
            out = out.child(
                row()
                    .w_full()
                    .items_center()
                    .gap(6.0)
                    .child(row().grow(1.0).min_w(0.0).child(text(label.clone()).font(CAPTION).color(k.c.text_muted)))
                    .child(Button::new("Close").variant(ButtonVariant::Ghost).on_press(|_: &mut S| send(Cmd::ClosePr)).build(k)),
            );
        }
        Some(Pr::Asking { status, opening }) => {
            let field = TextInput::new(&state.pr)
                .label("Pull request")
                .placeholder("PR number or URL")
                .width(None)
                .on_edit(move |s: &mut S| &mut at(s).pr)
                .on_key(move |s: &mut S, event: &KeyEvent, _: &mut Tx| {
                    if event.key == Key::Enter && !at(s).pr.text.trim().is_empty() {
                        send(Cmd::OpenPr(at(s).pr.text.trim().to_string()));
                        return true;
                    }
                    false
                })
                .build(k);
            out = out.child(field);
            if let Some(status) = status {
                let tone = if *opening { k.c.text_muted } else { k.c.danger.fg };
                out = out.child(text(status.clone()).font(CAPTION).color(tone));
            }
        }
        None => {}
    }
    out
}

/// The tree, or why there is none.
pub fn tree<S: 'static>(snapshot: &Snapshot, k: &Kit, under_title: El) -> El {
    let title = if snapshot.changes { "Changes" } else { "Explorer" };
    let header = super::header::ruled(k, row().w_full().h(super::header::BAND).items_center().gap(8.0).px(12.0).child(super::header::controls::<S>(k, &snapshot.lead)).child(row().grow(1.0).child(text(title).font(NAME).color(k.c.text_base))).child(super::header::controls::<S>(k, &snapshot.trail)));
    let body = match &snapshot.body {
        Body::Empty(message) => col().w_full().grow(1.0).center().child(text(message.clone()).font(CAPTION).color(k.c.text_muted)),
        Body::Rows(rows) => {
            let mut list = col().key("tree").focusable(true).w_full().px(4.0).on_key(|_: &mut S, event: &KeyEvent, _: &mut Tx| match step_of(event) {
                Some(step) => {
                    send(Cmd::TreeStep(step));
                    true
                }
                None => false,
            });
            for entry in rows {
                list = list.child(entry_row::<S>(k, entry));
            }
            col().w_full().grow(1.0).min_h(0.0).child(k.scroll_bar(col().w_full().grow(1.0).min_h(0.0).scroll(rune::ScrollSpec { stick_bottom: false }).child(list)))
        }
    };
    col().w_full().h_full().min_h(0.0).child(header).child(under_title).child(body)
}
