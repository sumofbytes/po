//! The conversation on Rune: the transcript (the user's messages and the agent's blocks), and the
//! composer under it. Counterpart of `src/nsh/chat.rs`.
use std::time::Duration;

use rune::tok::TextStyle;
use rune::{Color, El, GrowthSpec, Icon, Span, Key, KeyEvent, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::markdown::Markdown;
use rune_kit::{Button, ButtonVariant, Kit, Spinner};

use super::dropdown::{self, Line};
use super::pictures;

use crate::model::bus::{Cmd as Bus, send};
use crate::model::chat::{AskOption, Block, Cmd, Entry, Grant, GrantChange, Item, Kind, Phase, Picking, Queued, Scope, Snapshot, Tool};

fn body_style() -> TextStyle { crate::fonts::ui(14.0, 400.0) }
fn small_style() -> TextStyle { crate::fonts::ui(12.0, 400.0) }
fn label_style() -> TextStyle { crate::fonts::ui(12.0, 500.0) }
fn mono_style() -> TextStyle { crate::fonts::mono(12.0, 17.0) }
/// The column the conversation and the composer share.
const COLUMN: f32 = 760.0;
/// How far in from the composer's edge its text starts (its padding and its field's, which is where the chips' labels start too):
/// the composer is this much wider than the transcript's column at each side, so the two texts
/// start and end on the same lines.
const COMPOSER_INSET: f32 = 16.0;
/// The room the composer keeps at the stage's sides.
const COMPOSER_MARGIN: f32 = 12.0;
/// How many of a stretch's edited files get a chip of their own.
const MAX_CHIPS: usize = 4;

fn tell(cmd: Cmd) {
    send(Bus::Chat(cmd));
}

/// What the host keeps for the screen: the draft the user types.
pub struct State {
    pub draft: TextEdit,
    reset: u64,
    focus: u64,
    /// The rows the transcript's virtual stack now shows, for the rail's marker.
    pub visible: std::ops::Range<usize>,
    /// How wide the transcript's stage is, which says whether the rail has room beside the column.
    stage: f32,
    /// The line of the rail the pointer is over.
    hover: Option<usize>,
    /// The menu a composer chip has open.
    pub menu: super::dropdown::State,
    /// The picture shown large over the window, after its thumbnail was pressed.
    pub preview: Option<crate::model::chat::Picture>,
}

impl Default for State {
    fn default() -> Self {
        Self { draft: TextEdit::new(""), reset: 0, focus: 0, visible: 0..0, stage: 0.0, hover: None, menu: Default::default(), preview: None }
    }
}

pub type At<S> = fn(&mut S) -> &mut State;

impl State {
    /// Follows a new snapshot: the app's own writes to the draft take over what was typed.
    ///
    /// `typing_elsewhere`: another field (a thread's name being typed) has the keyboard, and a
    /// request to give it to the composer is let go rather than taking it from there.
    pub fn follow(&mut self, snapshot: &Snapshot, typing_elsewhere: bool) {
        if snapshot.draft_reset != self.reset {
            self.reset = snapshot.draft_reset;
            self.draft = TextEdit::new(snapshot.draft.clone());
        }
        if snapshot.focus != self.focus {
            self.focus = snapshot.focus;
            self.draft.focused |= !typing_elsewhere;
        }
    }
}

/// A small, thin check: a drawn mark in place of the font's "✓", which is heavy and takes the colour of success.
static CHECK: Icon = Icon { name: "po-check", view: 16.0, body: r#"<path d="M4 8.5L7 11.5L12 5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>"# };

/// A branch: a line down to a node, and a node it forks to.
static BRANCH: Icon = Icon { name: "po-branch", view: 16.0, body: r#"<path d="M4 2.5V10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/><circle cx="12" cy="4" r="1.75" stroke="currentColor" stroke-width="1.5"/><circle cx="4" cy="12" r="1.75" stroke="currentColor" stroke-width="1.5"/><path d="M12 5.75C12 9 9.5 12 5.75 12" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"# };

fn tint(k: &Kit, phase: Phase) -> Color {
    match phase {
        Phase::Waiting => k.c.text_muted,
        Phase::Working => k.c.warning.fg,
        // A finished step recedes: the thing worth the eye is the one still working or failed.
        Phase::Done => k.c.icon_faint,
        Phase::Failed => k.c.danger.fg,
    }
}

fn glyph(k: &Kit, phase: Phase) -> El {
    if phase == Phase::Working {
        return row().w(12.0).h(12.0).center().child(Spinner::new().size(12.0).color(k.c.icon_accent).build(k));
    }
    if phase == Phase::Done {
        return row().w(12.0).h(12.0).center().child(rune::icon(&CHECK, 12.0).color(tint(k, phase)));
    }
    let mark = match phase {
        Phase::Waiting => "○",
        Phase::Working => "",
        Phase::Done => "✓",
        Phase::Failed => "×",
    };
    text(mark).font(small_style()).color(tint(k, phase)).w(12.0)
}

fn muted(k: &Kit, s: impl Into<String>) -> El {
    text(s.into()).font(small_style()).color(k.c.text_muted)
}

/// A pressable row of the transcript.
fn pressable<S: 'static>(k: &Kit, key: String, content: El, press: impl Fn() + 'static) -> El {
    k.interactive(row().key(key).w_full().items_center().gap(8.0).py(4.0).radius(6.0).child(content).on_press(move |_: &mut S| press()))
}

fn took(d: Duration) -> String {
    let secs = d.as_secs().max(1);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

fn gist(text_: &str) -> String {
    let line = text_.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```")).unwrap_or("");
    let line = line.trim_start_matches(['#', '*', '_', '-', '>', ' ']);
    line.replace(['*', '`'], "").trim_matches('_').to_string()
}

fn user_row<S: 'static>(k: &Kit, body: &str, pictures: &[crate::model::chat::Picture], at: At<S>) -> El {
    // The words, then a small thumbnail of each picture at the row's end: pressing one shows it large.
    let mut inner = row().gap(12.0).items_center();
    if !body.is_empty() {
        inner = inner.child(text(body).font(body_style()).color(k.c.text_base).wrap_anywhere().shrink());
    }
    for (i, picture) in pictures.iter().enumerate() {
        let shown = picture.clone();
        let thumb = pictures::el(&picture.uri, picture.size, pictures::THUMB).radius(6.0).key(format!("user-picture-{i}")).on_press(move |s: &mut S| at(s).preview = Some(shown.clone()));
        inner = inner.child(k.interactive(row().key(format!("user-thumb-{i}")).role("button").label("Show picture").radius(6.0).child(thumb)));
    }
    // The user's side: the bubble sits at the right, as wide as its words need up to a limit, and wraps.
    row().w_full().justify_end().child(col().min_w(0.0).max_w(600.0).shrink().px(16.0).py(12.0).radius(10.0).fill(k.c.layer_03).child(inner))
}

/// The picture of a pressed thumbnail, large over the window; a press anywhere puts it away.
pub fn lightbox<S: 'static>(k: &Kit, state: &State, at: At<S>, layer: &mut Vec<El>) {
    let Some(picture) = &state.preview else { return };
    let big = pictures::el(&picture.uri, picture.size, pictures::PREVIEW).key("preview-picture");
    layer.push(rune::el().key("preview").role("dialog").label("Picture").absolute(0.0, 0.0).w_full().h_full().fill(k.c.scrim).center().child(big).on_press(move |s: &mut S| at(s).preview = None));
}

fn thought<S: 'static>(k: &Kit, message: usize, at: usize, body: &str, open: bool) -> El {
    let head = row().grow(1.0).min_w(0.0).child(text(if open { "Thought".to_string() } else { gist(body) }).font(small_style()).color(k.c.text_muted).truncate());
    let line = pressable::<S>(k, format!("thought-{message}-{at}"), head, move || tell(Cmd::ToggleThought { message, block: at }));
    if open { col().w_full().child(line).child(col().w_full().min_w(0.0).child(text(body.to_string()).font(small_style()).color(k.c.text_muted).spans(vec![Span::new(0, body.len()).italic()]).wrap())) } else { line }
}

fn tool<S: 'static>(k: &Kit, message: usize, at: usize, t: &Tool) -> El {
    // Collapsed, a call is one line: the first of its title, cut off where it runs out of room.
    let first = t.title.lines().next().unwrap_or_default().trim().to_string();
    let head = row().grow(1.0).min_w(0.0).items_center().gap(4.0).child(glyph(k, t.phase)).child(row().grow(1.0).min_w(0.0).child(text(first).font(small_style()).color(k.c.text_muted).truncate()));
    let mut card = col().w_full().child(pressable::<S>(k, format!("tool-{message}-{at}"), head, move || tell(Cmd::ToggleTool { message, block: at })));
    if t.open {
        // Expanded, all of it shows and wraps to the column.
        let rest = t.title.split_once('\n').map(|(_, rest)| rest.trim().to_string()).filter(|rest| !rest.is_empty());
        let wrapped = |s: String, color| text(s).font(mono_style()).color(color).wrap_anywhere().w_full();
        let mut detail = col().w_full().min_w(0.0).px(16.0).gap(4.0);
        if let Some(rest) = rest {
            detail = detail.child(wrapped(rest, k.c.text_muted));
        }
        if !t.arguments.is_empty() {
            detail = detail.child(wrapped(t.arguments.clone(), k.c.text_muted));
        }
        if !t.result.is_empty() {
            detail = detail.child(wrapped(t.result.clone(), k.c.text_base));
        }
        card = card.child(detail);
    }
    if let Some(path) = &t.path {
        let (name, path) = (path.rsplit('/').next().unwrap_or(path).to_string(), path.clone());
        card = card.child(row().px(16.0).child(chip::<S>(k, format!("open-{message}-{at}"), name, move || tell(Cmd::Open(path.clone())))));
    }
    card
}

fn chip<S: 'static>(k: &Kit, key: String, label: String, press: impl Fn() + 'static) -> El {
    k.interactive(row().key(key).px(8.0).py(2.0).radius(6.0).fill(k.c.layer_02).child(text(label).font(small_style()).color(k.c.text_base)).on_press(move |_: &mut S| press()))
}

#[allow(clippy::too_many_arguments)]
fn run<S: 'static>(k: &Kit, message: usize, at: usize, thoughts: usize, calls: usize, failed: usize, phase: Phase, folded: bool, task: Option<&str>, spent: Option<Duration>, latest: Option<&str>, kinds: &[(Kind, usize)], edits: &[String]) -> El {
    // One line, whatever the call's title holds (a heredoc runs over many); `truncate` cuts it with an ellipsis.
    let one_line = |s: &str| s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string();
    let label = task.map(one_line).unwrap_or_else(|| match (phase, latest, spent) {
        (Phase::Working, Some(latest), _) => format!("Working · {}", one_line(latest)),
        (Phase::Working, None, _) => "Working…".into(),
        (_, _, Some(d)) => format!("Worked for {}", took(d)),
        _ => "Worked".into(),
    });
    let mut aside: Vec<String> = vec![];
    if kinds.is_empty() {
        if calls > 0 {
            aside.push(format!("{calls} tool {}", if calls == 1 { "call" } else { "calls" }));
        }
    } else {
        aside.extend(Kind::ALL.iter().filter_map(|kind| kinds.iter().find(|(k, _)| k == kind).map(|(kind, n)| kind.counted(*n))));
    }
    if thoughts > 0 {
        aside.push(format!("{thoughts} {}", if thoughts == 1 { "thought" } else { "thoughts" }));
    }
    if failed > 0 && phase != Phase::Failed {
        aside.push(format!("{failed} failed"));
    }
    let mut head = row().grow(1.0).min_w(0.0).items_center().gap(4.0).child(glyph(k, phase)).child(muted(k, label).truncate().shrink());
    if !aside.is_empty() {
        head = head.child(muted(k, format!("· {}", aside.join(" · "))));
    }
    head = head.child(row().grow(1.0)).child(muted(k, if folded { "›" } else { "⌄" }));
    let mut card = col().w_full().child(pressable::<S>(k, format!("run-{message}-{at}"), head, move || tell(Cmd::Fold { message, block: at, folded: !folded })));
    if !edits.is_empty() {
        let mut chips = row().px(16.0).gap(6.0).pb(4.0);
        for (i, path) in edits.iter().take(MAX_CHIPS).enumerate() {
            let (name, path) = (path.rsplit('/').next().unwrap_or(path).to_string(), path.clone());
            chips = chips.child(chip::<S>(k, format!("edit-{message}-{at}-{i}"), name, move || tell(Cmd::Open(path.clone()))));
        }
        if edits.len() > MAX_CHIPS {
            chips = chips.child(muted(k, format!("+{} more", edits.len() - MAX_CHIPS)));
        }
        card = card.child(chips);
    }
    card
}

fn plan(k: &Kit, entries: &[Entry]) -> El {
    let mut list = col().w_full().gap(6.0).py(4.0);
    for entry in entries {
        let color = if entry.phase == Phase::Done { k.c.text_muted } else { k.c.text_base };
        list = list.child(row().items_center().gap(4.0).child(glyph(k, entry.phase)).child(row().grow(1.0).min_w(0.0).child(text(entry.text.clone()).font(body_style()).color(color).wrap())));
    }
    list
}

fn card(k: &Kit, waiting: bool) -> El {
    col().w_full().gap(8.0).padding(12.0).radius(10.0).border(1.0, if waiting { k.c.warning.border } else { k.c.border_base })
}

fn ask<S: 'static>(k: &Kit, message: usize, at: usize, title: &str, options: &[AskOption], answered: &Option<String>) -> El {
    let head = row().gap(8.0).child(text("Permission").font(label_style()).color(k.c.text_muted)).child(row().grow(1.0).min_w(0.0).child(Markdown::new(title.to_string()).build(k)));
    let mut c = card(k, answered.is_none()).child(head);
    match answered {
        Some(given) => c = c.child(muted(k, format!("→ {given}"))),
        None => {
            let mut ways = row().gap(6.0);
            for option in options {
                let way = option.at;
                let b = Button::new(option.label.clone()).variant(if option.primary { ButtonVariant::Submit } else { ButtonVariant::Ghost });
                ways = ways.child(b.on_press(move |_: &mut S| tell(Cmd::Answer { message, block: at, at: way })).build(k));
            }
            c = c.child(ways);
        }
    }
    c
}

fn approve<S: 'static>(k: &Kit, message: usize, at: usize, question: &str, lines: &[String], answered: &Option<String>) -> El {
    let head = row().gap(8.0).child(text("Approve").font(label_style()).color(k.c.text_muted)).child(row().grow(1.0).min_w(0.0).child(Markdown::new(question.to_string()).build(k)));
    let mut c = card(k, answered.is_none()).child(head);
    if !lines.is_empty() {
        let mut body = col().w_full().min_w(0.0).px(12.0).gap(2.0);
        for line in lines {
            body = body.child(text(line.clone()).font(mono_style()).color(k.c.text_base).wrap_anywhere());
        }
        c = c.child(body);
    }
    match answered {
        Some(given) => c = c.child(muted(k, format!("→ {given}"))),
        None => {
            c = c.child(
                row()
                    .gap(6.0)
                    .child(Button::new("Run").variant(ButtonVariant::Submit).on_press(move |_: &mut S| tell(Cmd::Approve { message, block: at, yes: true })).build(k))
                    .child(Button::new("Leave out").variant(ButtonVariant::Ghost).on_press(move |_: &mut S| tell(Cmd::Approve { message, block: at, yes: false })).build(k)),
            );
        }
    }
    c
}

fn block<S: 'static>(k: &Kit, message: usize, at: usize, b: &Block) -> El {
    match b {
        Block::Markdown { text, .. } => Markdown::new(text.to_string())
            .selectable(true)
            // Only web and mail links go to the system; a message must not open arbitrary schemes.
            .on_link_tx(|_: &mut S, url: &str, tx: &mut Tx| {
                if ["https://", "http://", "mailto:"].iter().any(|p| url.starts_with(p)) {
                    tx.open_url(url).ok();
                }
            })
            .build(k),
        Block::Thought { index, text, open } => thought::<S>(k, message, *index, text, *open),
        Block::Tool(t) => tool::<S>(k, message, t.index, t),
        Block::Run { index, count: _, calls, failed, phase, folded, task, took, latest, kinds, edits } => {
            // Thoughts a run folds away are counted from the blocks it covers: see `agent_blocks`.
            run::<S>(k, message, *index, 0, *calls, *failed, *phase, *folded, task.as_deref(), *took, latest.as_deref(), kinds, edits)
        }
        Block::Plan(entries) => plan(k, entries),
        Block::Ask { index, title, options, answered } => ask::<S>(k, message, *index, title, options, answered),
        Block::Approve { index, question, lines, answered } => approve::<S>(k, message, *index, question, lines, answered),
    }
    .key(format!("block-{message}-{at}"))
}

/// One message of the agent's: a row per block, a folded run's own row standing for the blocks it
/// hides.
fn agent<S: 'static>(k: &Kit, message: usize, blocks: &[Block]) -> El {
    let mut c = col().w_full().gap(8.0);
    let mut skip = 0;
    for (at, b) in blocks.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if let Block::Run { count, folded: true, .. } = b {
            skip = *count;
        }
        c = c.child(block::<S>(k, message, at, b));
    }
    c
}

/// How many bars the rail holds at most; more and messages share one.
const MAX_BARS: usize = 24;

fn transcript<S: 'static>(k: &Kit, snapshot: &Snapshot, state: &State, at: At<S>) -> El {
    let count = snapshot.items.len();
    // The rows build at layout, only those in the viewport (and two viewports either side), so a
    // long transcript costs the rows it shows: every row keyed by its place, built from the
    // snapshot this pass took.
    let items = snapshot.items.clone();
    let kit = k.clone();
    let stack = col().key("transcript").w_full().growth(GrowthSpec::default()).virtual_rows::<S>(count, 96.0, |_: &S, i| format!("item-{i}").into(), move |_: &S, i| {
        let inner = match items.get(i) {
            Some(Item::User { text, pictures }) => user_row::<S>(&kit, text, pictures, at),
            Some(Item::Agent { index, blocks }) => agent::<S>(&kit, *index, blocks),
            _ => col(),
        };
        // As far in as the composer's text, whatever the width, which also keeps it clear of the scroll bar.
        row().w_full().px(COMPOSER_MARGIN + COMPOSER_INSET).justify_center().child(col().w_full().max_w(COLUMN).py(8.0).child(inner))
    }).on_visible_rows::<S>(move |s: &mut S, range| {
        at(s).visible = range;
        true
    });
    let viewport = col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: true }).child(stack);
    // The rail floats over the right edge, so the column is centred on the stage as the composer is.
    let stage = state.stage;
    let right = ((stage - COLUMN) / 2.0 - RAIL_GUTTER).max(RAIL_INSET);
    let rail = (stage >= COLUMN + 2.0 * (RAIL_GUTTER + RAIL_INSET)).then(|| rail::<S>(k, state, count, at).pin(None, Some(0.0), Some(right), Some(0.0)));
    col().w_full().grow(1.0).min_h(0.0).on_layout_tx::<S>(move |s: &mut S, r: [f32; 4], tx: &mut Tx| {
        if (at(s).stage - r[2]).abs() > 0.5 {
            at(s).stage = r[2];
        } else {
            tx.live_only();
        }
    }).child(k.scroll_bar(viewport)).children(rail)
}

/// The rail beside the column: a bar per message (messages shared when there are too many), a
/// press on one scrolls there, and the marker over the rows that show. Nothing while the column
/// fits the stage, which is when every row is in view.
fn rail<S: 'static>(k: &Kit, state: &State, count: usize, at: At<S>) -> El {
    if count < 2 || state.visible.len() >= count {
        return col();
    }
    let shown = count.min(MAX_BARS);
    let step = count as f64 / shown as f64;
    // As the stage beside the column has room for it: short lines, a quiet one for each message and
    // a firmer one where the view is, in the middle of the edge.
    // Where the view is: how far down the messages it has come, from the first bar to the last, so
    // the end of the page is the last bar. That line is the firm one, with only the next either
    // side taking a little of it; the pointer, on the rail, says otherwise.
    let room = count.saturating_sub(state.visible.len()).max(1);
    let down = (state.visible.start as f64 / room as f64).clamp(0.0, 1.0);
    let focus = state.hover.unwrap_or((down * (shown - 1) as f64).round() as usize);
    let mut bars = col().key("rail").w_full().h_full().min_h(0.0).justify_center().items_center();
    for at_ in 0..shown {
        let from = (at_ as f64 * step) as usize;
        let press = move |_: &mut S, tx: &mut Tx| tx.scroll_into_view(format!("item-{from}"));
        // The line under the pointer swells, and the ones beside it a little less, the further they
        // are, easing as the pointer moves along; the slot is the target, so the space between the
        // lines takes a press too.
        // The size follows the pointer only; the colour follows it too, and else the view.
        let swell = state.hover.map(|h| h.abs_diff(at_)).filter(|gap| *gap <= 2);
        let (len, thick) = match swell {
            Some(0) => (RAIL_WIDTH, 5.0),
            Some(1) => (RAIL_BAR * 1.6, 3.5),
            Some(_) => (RAIL_BAR * 1.25, 2.5),
            None => (RAIL_BAR, 2.0),
        };
        // Firm at the focus, a step down one either side, and the rest quiet, in view or not.
        let color = match focus.abs_diff(at_) {
            0 => k.c.text_base,
            1 => k.c.icon_faint,
            _ => k.c.border_base,
        };
        let line = row().w(len).h(thick).radius(thick / 2.0).fill(color).motion(rune::Spring::SNAPPY);
        let slot = col().key(format!("bar-{at_}")).w(RAIL_WIDTH).h(8.0).min_h(8.0).justify_center().items_center().child(line).on_press_tx(press).on_hover_change::<S>(move |s: &mut S, inside: bool, _: &mut Tx| {
            let state = at(s);
            if inside {
                state.hover = Some(at_);
            } else if state.hover == Some(at_) {
                state.hover = None;
            }
        });
        bars = bars.child(k.interactive(slot));
    }
    bars.w(RAIL_WIDTH)
}

/// A line of the rail, and the width of its target.
const RAIL_WIDTH: f32 = 36.0;
const RAIL_BAR: f32 = 18.0;
/// The room the rail wants beside the column: its width and the margin from the column, and the
/// least it keeps from the stage's edge.
const RAIL_GUTTER: f32 = RAIL_WIDTH + 12.0;
const RAIL_INSET: f32 = 4.0;

fn queued<S: 'static>(k: &Kit, q: &Queued) -> El {
    let preview = if q.text.is_empty() { format!("{} picture{}", q.pictures, if q.pictures == 1 { "" } else { "s" }) } else { q.text.clone() };
    let at = q.at;
    row()
        .w_full()
        .items_center()
        .gap(8.0)
        .px(8.0)
        .py(2.0)
        .child(if q.kind.is_empty() { row() } else { row().child(muted(k, q.kind.clone())) })
        .child(row().grow(1.0).min_w(0.0).child(text(preview).font(small_style()).color(k.c.text_base)))
        .child(Button::new("Edit").variant(ButtonVariant::Ghost).on_press(move |_: &mut S| tell(Cmd::Recall(at))).build(k))
}

fn picker<S: 'static>(k: &Kit, picking: &Picking) -> El {
    let mut c = col().w_full().gap(4.0).padding(8.0).radius(10.0).border(1.0, k.c.border_base);
    c = c.child(row().w_full().items_center().child(row().grow(1.0).child(text("Resume a session").font(label_style()).color(k.c.text_muted))).child(Button::new("Close").variant(ButtonVariant::Ghost).on_press(|_: &mut S| tell(Cmd::ClosePicker)).build(k)));
    match picking {
        Picking::Note(note) => c = c.child(muted(k, note.clone())),
        Picking::Loading => {}
        Picking::Sessions(sessions) => {
            for (i, s) in sessions.iter().enumerate() {
                let id = s.id.clone();
                let title = if s.title.is_empty() { "Untitled".to_string() } else { s.title.clone() };
                let content = row().grow(1.0).items_center().gap(8.0).child(text(title).font(small_style()).color(k.c.text_base)).child(muted(k, s.when.clone()));
                c = c.child(pressable::<S>(k, format!("session-{i}"), content, move || tell(Cmd::Resume(id.clone()))));
            }
        }
    }
    c
}

/// What is chosen before and around the field: the agent of a thread not started, the settings, the
/// worktree switch.
/// The name a scope has on the control chip and in its menu.
fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Viewer => "Basic",
        Scope::Orchestrate => "Delegate",
        Scope::Global => "Full access",
    }
}

/// What an agent that runs this thread may do through Po: one level of reach, picked from a menu,
/// and two extras switched on or off beside it. [`tell`] tells the Reader.
fn control_lines(grant: Grant, pending: bool) -> (Vec<Line>, Vec<usize>) {
    let pick = |label: &str, on: bool, change: GrantChange| Line::choice(label, on, Bus::Chat(Cmd::Grant(change)));
    let mut lines = vec![
        pick("Basic · show files and views", grant.scope == Scope::Viewer, GrantChange::Stop),
        pick("Delegate · also run its own threads, edit issues", grant.scope == Scope::Orchestrate, GrantChange::Set(Grant { scope: Scope::Orchestrate, ..grant })),
        pick("Full access · also open projects, use worktrees", grant.scope == Scope::Global, GrantChange::Set(Grant { scope: Scope::Global, ..grant })),
        pick("Read other threads", grant.read, GrantChange::Read),
        pick("Answer its threads' asks", grant.approvals, GrantChange::Approvals),
    ];
    let mut rules = vec![3];
    if pending {
        rules.push(lines.len());
        lines.push(Line::note("Restart the agent to apply"));
    }
    (lines, rules)
}

/// Where a chip's menu is kept: in the conversation's state.
fn menu_of<S: 'static>(at: At<S>) -> dropdown::At<S> {
    std::rc::Rc::new(move |s: &mut S| &mut at(s).menu)
}

/// A chip that only says: a choice that is not the user's to change now.
fn still_chip(k: &Kit, label: String) -> El {
    row().px(8.0).py(2.0).radius(6.0).child(text(label).font(small_style()).color(k.c.text_muted))
}

/// The Control chip: names what the thread's agent may do, and opens the menu that changes it.
fn control_chip<S: 'static>(k: &Kit, snapshot: &Snapshot, at: At<S>) -> El {
    let Some(grant) = snapshot.grant else { return col() };
    let label = format!("{}{}", scope_name(grant.scope), if snapshot.grant_pending { " · restart to apply" } else { "" });
    let (lines, rules) = control_lines(grant, snapshot.grant_pending);
    // Full reach is the one setting worth a second look, so its name takes the scheme's warning colour.
    let ink = if grant.scope == Scope::Global { k.c.warning.fg } else { k.c.text_base };
    dropdown::chip_inked::<S>(k, "control", label, ink, lines, rules, menu_of(at))
}

fn chips<S: 'static>(k: &Kit, snapshot: &Snapshot, at: At<S>) -> El {
    let menu = menu_of(at);
    let mut bar = row().grow(1.0).min_w(0.0).items_center().gap(6.0).flex_wrap();
    let chosen = snapshot.chosen.as_ref().and_then(|c| snapshot.agents.iter().find(|(_, id)| id == c)).map(|(n, _)| n.clone());
    if (snapshot.blank || snapshot.switchable) && !snapshot.agents.is_empty() {
        let lines = snapshot.agents.iter().map(|(name, id)| Line::choice(name.as_str(), snapshot.chosen.as_ref() == Some(id), Bus::Chat(Cmd::PickAgent(id.clone())))).collect();
        bar = bar.child(dropdown::chip::<S>(k, "agent-pick", chosen.unwrap_or_else(|| "Agent".to_string()), lines, vec![], menu.clone()));
    } else if let Some(name) = chosen {
        bar = bar.child(still_chip(k, name));
    }
    for setting in &snapshot.settings {
        let label = setting.current_name().to_string();
        if snapshot.busy || setting.choices.len() < 2 {
            bar = bar.child(still_chip(k, label));
            continue;
        }
        let lines = setting.choices.iter().map(|(value, name)| Line::choice(name.as_str(), *value == setting.current, Bus::Chat(Cmd::Setting { id: setting.id.clone(), value: value.clone() }))).collect();
        bar = bar.child(dropdown::chip::<S>(k, &format!("setting-{}", setting.id), label, lines, vec![], menu.clone()));
    }
    if snapshot.blank
        && let Some(on) = snapshot.isolate
    {
        // A pill that is on while the thread gets a worktree of its own: a branch and its name, outlined
        // when off and filled, with the text at full ink, when on.
        let (ink, fill, line) = if on { (k.c.text_accent, k.c.layer_03, k.c.bg_accent) } else { (k.c.text_muted, Color::TRANSPARENT, k.c.border_base) };
        let face = row().key("worktree").px(8.0).py(1.0).radius(10.0).gap(4.0).items_center().fill(fill).border(1.0, line).child(rune::icon(&BRANCH, 12.0).color(ink)).child(text("Worktree").font(small_style()).color(ink));
        bar = bar.child(k.interactive(face.on_press(move |_: &mut S| tell(Cmd::Isolate(!on)))));
    }
    bar.child(control_chip::<S>(k, snapshot, at))
}

fn composer<S: 'static>(k: &Kit, snapshot: &Snapshot, state: &State, at: At<S>) -> El {
    let steer = snapshot.steer;
    // The field is built here, not by the kit's `TextArea`: that has no paste hook, and `on_input_paste`
    // only takes on the El that holds the input (the kit's is a wrapper around it), so an image never arrived.
    let placeholder = if snapshot.blank { "Message the agent" } else { "Ask for follow-up changes" };
    let style = TextStyle { line_height: (k.ty.ui.size * 1.35 * 2.0).round() / 2.0, ..k.ty.ui };
    let input = rune::text_input(&state.draft, style)
        .key("input")
        .w_full()
        .color(k.c.text_base)
        .placeholder(placeholder, k.c.text_faint)
        .caret(k.c.text_base, k.selection())
        .label("Message")
        .description("")
        .on_edit(move |s: &mut S| &mut at(s).draft)
        .on_input_change(move |s: &mut S, _: &mut Tx| tell(Cmd::Draft(at(s).draft.text.clone())))
        .on_input_key(move |_: &mut S, event: &KeyEvent, _: &mut Tx| {
            // Enter sends; Shift+Enter is a new line; Ctrl+Enter queues behind the turn.
            if event.key == Key::Enter && !event.mods.shift {
                tell(Cmd::Send { later: event.mods.ctrl });
                return true;
            }
            false
        })
        // What the clipboard held beyond text goes to the draft: files attach, an image pastes
        // (the clipboard offers it as PNG). Plain text pastes as the field always does.
        .on_input_paste(|_: &mut S, content: &rune::ClipboardContent, _: &mut Tx| {
            let paths: Vec<String> = content.files().iter().map(|path| format!("file:{}", path.to_string_lossy())).collect();
            match (content.image(), paths) {
                (Some(image), paths) if paths.is_empty() => {
                    tell(Cmd::Paste(vec![image.png().to_vec()]));
                    true
                }
                (_, paths) if !paths.is_empty() => {
                    tell(Cmd::Drop(paths));
                    true
                }
                _ => false,
            }
        });
    // One row at rest; it grows with the text (and the pictures strip and note add their own room only when present).
    // Padded so the text starts where the chips' labels do (their boxes pad 8 too) and as far from the top.
    let field = col().role("textbox").padding(8.0).child(rune::auto_grow(input, 1, 8));
    let mut actions = row().gap(6.0);
    if snapshot.busy {
        actions = actions.child(Button::new(if steer { "Steer" } else { "Queue" }).variant(ButtonVariant::Ghost).on_press(|_: &mut S| tell(Cmd::Send { later: true })).build(k));
        actions = actions.child(Button::new("■").help("Stop").on_press(|_: &mut S| tell(Cmd::Stop)).build(k));
    } else {
        actions = actions.child(Button::new("↑").help("Send").variant(ButtonVariant::Submit).on_press(|_: &mut S| tell(Cmd::Send { later: false })).build(k));
    }
    let mut core = col().w_full().max_w(COLUMN + 2.0 * COMPOSER_INSET).gap(4.0).padding(8.0).pb(16.0).radius(14.0).fill(k.c.bg_base).border(1.0, k.c.border_strong);
    if let Some(note) = &snapshot.note {
        core = core.child(row().w_full().items_center().gap(8.0).child(row().grow(1.0).child(muted(k, note.clone()))).child(Button::new("Dismiss").variant(ButtonVariant::Ghost).on_press(|_: &mut S| tell(Cmd::DismissNote)).build(k)));
    }
    if !snapshot.pictures.is_empty() {
        // Inset as the field is: the pictures start where the text does, and as far from the top.
        let mut strip = row().w_full().px(8.0).pt(8.0).flex_wrap().gap(8.0);
        for (i, picture) in snapshot.pictures.iter().enumerate() {
            // A thumbnail with its own way off: a cross over its top right corner, not the strip as a
            // whole being one button.
            let thumb = pictures::el(&picture.uri, picture.size, pictures::SMALL).key(format!("draft-picture-{i}"));
            let cross = k.interactive(row().key(format!("detach-{i}")).pin(None, Some(4.0), Some(4.0), None).px(6.0).py(1.0).radius(6.0).fill(k.c.layer_03).child(text("×").font(small_style()).color(k.c.text_base)).on_press(move |_: &mut S| tell(Cmd::Detach(i))));
            strip = strip.child(col().child(thumb).child(cross));
        }
        core = core.child(strip);
    }
    core = core.child(field).child(row().w_full().items_end().gap(8.0).pr(8.0).child(chips::<S>(k, snapshot, at)).child(actions));
    let mut column = col().w_full().items_center().gap(6.0).px(COMPOSER_MARGIN).pt(6.0).pb(10.0);
    if let Some(picking) = snapshot.picking.as_ref().filter(|p| **p != Picking::Loading) {
        column = column.child(col().w_full().max_w(COLUMN + 2.0 * COMPOSER_INSET).child(picker::<S>(k, picking)));
    }
    if !snapshot.queue.is_empty() {
        let mut tray = col().w_full().max_w(COLUMN + 2.0 * COMPOSER_INSET).py(4.0).radius(10.0).fill(k.c.layer_02);
        for q in &snapshot.queue {
            tray = tray.child(queued::<S>(k, q));
        }
        column = column.child(tray);
    }
    column.child(core)
}

/// The thread's header as the window's toolbar: its leading controls and title, then its status
/// lines and trailing controls. Where the conversation is not in the middle pane it keeps this
/// band inside itself ([`view`] with `framed`).
pub fn toolbar<S: 'static>(k: &Kit, snapshot: &Snapshot, rename: &super::rename::Rename, rename_at: super::rename::At<S>) -> rune_kit::shell::Toolbar {
    let mut bar = rune_kit::shell::Toolbar::new().leading(super::header::controls::<S>(k, &snapshot.lead));
    // The name being typed takes the middle of the band; the title's own slot would hold it to the
    // width of the name.
    bar = match super::rename::line::<S>(k, rename, rename_at, snapshot.thread, body_style()) {
        Some(line) => bar.center(line),
        None => bar.leading(super::rename::title::<S>(k, rename, rename_at, snapshot.thread, &snapshot.title, body_style())),
    };
    for status in &snapshot.statuses {
        bar = bar.trailing(muted(k, status.clone()));
    }
    bar.trailing(super::header::controls::<S>(k, &snapshot.trail))
}

/// The header's title and status lines, for a conversation that is not under the window's toolbar.
fn header<S: 'static>(k: &Kit, snapshot: &Snapshot, rename: &super::rename::Rename, rename_at: super::rename::At<S>) -> El {
    let lead: Vec<_> = snapshot.lead.iter().copied().filter(|c| !matches!(c, crate::model::header::Control::Sidebar(_))).collect();
    let mut bar = row().w_full().h(super::header::BAND).items_center().gap(12.0).px(16.0).child(super::header::controls::<S>(k, &lead)).child(super::rename::title::<S>(k, rename, rename_at, snapshot.thread, &snapshot.title, body_style()));
    for status in &snapshot.statuses {
        bar = bar.child(muted(k, status.clone()));
    }
    super::header::ruled(k, bar.child(super::header::controls::<S>(k, &snapshot.trail)))
}

/// The conversation; `framed` keeps its own header, else the window's toolbar carries it.
pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>, framed: bool, rename: &super::rename::Rename, rename_at: super::rename::At<S>) -> El {
    col().w_full().h_full().min_h(0.0).children(framed.then(|| header::<S>(k, snapshot, rename, rename_at))).child(transcript::<S>(k, snapshot, state, at)).child(composer::<S>(k, snapshot, state, at))
}
