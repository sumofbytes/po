//! The conversation on Rune: the transcript (the user's messages and the agent's blocks), and the
//! composer under it. Counterpart of `src/nsh/chat.rs`. Not ported yet: pictures on the draft, the
//! Control (grant) menu, the transcript rail, and a virtualised transcript (every message is built).
use std::time::Duration;

use rune::tok::{Family, TextStyle};
use rune::{Color, El, Span, Key, KeyEvent, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::markdown::Markdown;
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, Switch};
use rune_kit::{TextArea};

use crate::model::bus::{Cmd as Bus, send};
use crate::model::chat::{AskOption, Block, Cmd, Entry, Item, Kind, Phase, Picking, Queued, Snapshot, Tool};

const BODY: TextStyle = TextStyle::new(14.0, 400.0);
const SMALL: TextStyle = TextStyle::new(12.0, 400.0);
const LABEL: TextStyle = TextStyle::new(12.0, 500.0);
const MONO: TextStyle = TextStyle { family: Family::SystemMono, line_height: 17.0, ..TextStyle::new(12.0, 400.0) };
/// The column the conversation and the composer share.
const COLUMN: f32 = 760.0;
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
}

impl Default for State {
    fn default() -> Self {
        Self { draft: TextEdit::new(""), reset: 0, focus: 0 }
    }
}

pub type At<S> = fn(&mut S) -> &mut State;

impl State {
    /// Follows a new snapshot: the app's own writes to the draft take over what was typed.
    pub fn follow(&mut self, snapshot: &Snapshot) {
        if snapshot.draft_reset != self.reset {
            self.reset = snapshot.draft_reset;
            self.draft = TextEdit::new(snapshot.draft.clone());
        }
        if snapshot.focus != self.focus {
            self.focus = snapshot.focus;
            self.draft.focused = true;
        }
    }
}

fn tint(k: &Kit, phase: Phase) -> Color {
    match phase {
        Phase::Waiting => k.c.text_muted,
        Phase::Working => k.c.warning.fg,
        Phase::Done => k.c.success.fg,
        Phase::Failed => k.c.danger.fg,
    }
}

fn glyph(k: &Kit, phase: Phase) -> El {
    let mark = match phase {
        Phase::Waiting => "○",
        Phase::Working => "◐",
        Phase::Done => "✓",
        Phase::Failed => "×",
    };
    text(mark).font(SMALL).color(tint(k, phase)).w(14.0)
}

fn muted(k: &Kit, s: impl Into<String>) -> El {
    text(s.into()).font(SMALL).color(k.c.text_muted)
}

/// A pressable row of the transcript.
fn pressable<S: 'static>(k: &Kit, key: String, content: El, press: impl Fn() + 'static) -> El {
    k.interactive(row().key(key).w_full().items_center().gap(8.0).px(8.0).py(4.0).radius(6.0).child(content).on_press(move |_: &mut S| press()))
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

fn user_row(k: &Kit, body: &str, pictures: usize) -> El {
    let shown = if body.is_empty() && pictures > 0 { format!("{pictures} picture{}", if pictures == 1 { "" } else { "s" }) } else { body.to_string() };
    row().w_full().justify_end().child(col().max_w(560.0).px(12.0).py(8.0).radius(12.0).fill(k.c.layer_03).child(text(shown).font(BODY).color(k.c.text_base)))
}

fn thought<S: 'static>(k: &Kit, message: usize, at: usize, body: &str, open: bool) -> El {
    let head = row().grow(1.0).min_w(0.0).child(text(if open { "Thought".to_string() } else { gist(body) }).font(SMALL).color(k.c.text_muted));
    let line = pressable::<S>(k, format!("thought-{message}-{at}"), head, move || tell(Cmd::ToggleThought { message, block: at }));
    if open { col().w_full().child(line).child(col().px(8.0).child(text(body.to_string()).font(SMALL).color(k.c.text_muted).spans(vec![Span::new(0, body.len()).italic()]))) } else { line }
}

fn tool<S: 'static>(k: &Kit, message: usize, at: usize, t: &Tool) -> El {
    let head = row().grow(1.0).min_w(0.0).items_center().gap(8.0).child(glyph(k, t.phase)).child(text(t.title.clone()).font(SMALL).color(k.c.text_muted));
    let mut card = col().w_full().child(pressable::<S>(k, format!("tool-{message}-{at}"), head, move || tell(Cmd::ToggleTool { message, block: at })));
    if t.open {
        let mut detail = col().px(30.0).gap(4.0);
        if !t.arguments.is_empty() {
            detail = detail.child(text(t.arguments.clone()).font(MONO).color(k.c.text_muted));
        }
        if !t.result.is_empty() {
            detail = detail.child(text(t.result.clone()).font(MONO).color(k.c.text_base));
        }
        card = card.child(detail);
    }
    if let Some(path) = &t.path {
        let (name, path) = (path.rsplit('/').next().unwrap_or(path).to_string(), path.clone());
        card = card.child(row().px(30.0).child(chip::<S>(k, format!("open-{message}-{at}"), name, move || tell(Cmd::Open(path.clone())))));
    }
    card
}

fn chip<S: 'static>(k: &Kit, key: String, label: String, press: impl Fn() + 'static) -> El {
    k.interactive(row().key(key).px(8.0).py(2.0).radius(6.0).fill(k.c.layer_02).child(text(label).font(SMALL).color(k.c.text_base)).on_press(move |_: &mut S| press()))
}

#[allow(clippy::too_many_arguments)]
fn run<S: 'static>(k: &Kit, message: usize, at: usize, thoughts: usize, calls: usize, failed: usize, phase: Phase, folded: bool, task: Option<&str>, spent: Option<Duration>, latest: Option<&str>, kinds: &[(Kind, usize)], edits: &[String]) -> El {
    let label = task.map(str::to_string).unwrap_or_else(|| match (phase, latest, spent) {
        (Phase::Working, Some(latest), _) => format!("Working · {latest}"),
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
    let mut head = row().grow(1.0).min_w(0.0).items_center().gap(8.0).child(glyph(k, phase)).child(muted(k, label));
    if !aside.is_empty() {
        head = head.child(muted(k, format!("· {}", aside.join(" · "))));
    }
    head = head.child(row().grow(1.0)).child(muted(k, if folded { "›" } else { "⌄" }));
    let mut card = col().w_full().child(pressable::<S>(k, format!("run-{message}-{at}"), head, move || tell(Cmd::Fold { message, block: at, folded: !folded })));
    if !edits.is_empty() {
        let mut chips = row().px(30.0).gap(6.0).pb(4.0);
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
        list = list.child(row().items_center().gap(8.0).child(glyph(k, entry.phase)).child(text(entry.text.clone()).font(BODY).color(color)));
    }
    list
}

fn card(k: &Kit, waiting: bool) -> El {
    let c = col().w_full().gap(8.0).padding(12.0).radius(10.0).border(1.0, if waiting { k.c.warning.border } else { k.c.border_base });
    c
}

fn ask<S: 'static>(k: &Kit, message: usize, at: usize, title: &str, options: &[AskOption], answered: &Option<String>) -> El {
    let head = row().gap(8.0).child(text("Permission").font(LABEL).color(k.c.text_muted)).child(row().grow(1.0).min_w(0.0).child(Markdown::new(title.to_string()).build(k)));
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
    let head = row().gap(8.0).child(text("Approve").font(LABEL).color(k.c.text_muted)).child(row().grow(1.0).min_w(0.0).child(Markdown::new(question.to_string()).build(k)));
    let mut c = card(k, answered.is_none()).child(head);
    if !lines.is_empty() {
        let mut body = col().px(12.0).gap(2.0);
        for line in lines {
            body = body.child(text(line.clone()).font(MONO).color(k.c.text_base));
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
        Block::Markdown { text, .. } => Markdown::new(text.to_string()).selectable(true).build(k),
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

fn transcript<S: 'static>(k: &Kit, snapshot: &Snapshot) -> El {
    let mut list = col().w_full().max_w(COLUMN).gap(16.0).px(16.0).py(16.0);
    for (i, item) in snapshot.items.iter().enumerate() {
        list = list.child(match item {
            Item::User { text, pictures } => user_row(k, text, pictures.len()).key(format!("user-{i}")),
            Item::Agent { index, blocks } => agent::<S>(k, *index, blocks).key(format!("agent-{i}")),
        });
    }
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(k.scroll_bar(col().w_full().grow(1.0).min_h(0.0).items_center().scroll(ScrollSpec { stick_bottom: true }).child(list)))
}

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
        .child(row().grow(1.0).min_w(0.0).child(text(preview).font(SMALL).color(k.c.text_base)))
        .child(Button::new("Edit").variant(ButtonVariant::Ghost).on_press(move |_: &mut S| tell(Cmd::Recall(at))).build(k))
}

fn picker<S: 'static>(k: &Kit, picking: &Picking) -> El {
    let mut c = col().w_full().gap(4.0).padding(8.0).radius(10.0).border(1.0, k.c.border_base);
    c = c.child(row().w_full().items_center().child(row().grow(1.0).child(text("Resume a session").font(LABEL).color(k.c.text_muted))).child(Button::new("Close").variant(ButtonVariant::Ghost).on_press(|_: &mut S| tell(Cmd::ClosePicker)).build(k)));
    match picking {
        Picking::Note(note) => c = c.child(muted(k, note.clone())),
        Picking::Loading => {}
        Picking::Sessions(sessions) => {
            for (i, s) in sessions.iter().enumerate() {
                let id = s.id.clone();
                let title = if s.title.is_empty() { "Untitled".to_string() } else { s.title.clone() };
                let content = row().grow(1.0).items_center().gap(8.0).child(text(title).font(SMALL).color(k.c.text_base)).child(muted(k, s.when.clone()));
                c = c.child(pressable::<S>(k, format!("session-{i}"), content, move || tell(Cmd::Resume(id.clone()))));
            }
        }
    }
    c
}

/// What is chosen before and around the field: the agent of a thread not started, the settings, the
/// worktree switch.
fn chips<S: 'static>(k: &Kit, snapshot: &Snapshot) -> El {
    let mut bar = row().grow(1.0).min_w(0.0).items_center().gap(6.0).flex_wrap();
    if snapshot.blank && !snapshot.agents.is_empty() {
        let names: Vec<&str> = snapshot.agents.iter().map(|(n, _)| n.as_str()).collect();
        let current = snapshot.chosen.as_ref().and_then(|c| snapshot.agents.iter().position(|(_, id)| id == c));
        let ids: Vec<String> = snapshot.agents.iter().map(|(_, id)| id.clone()).collect();
        bar = bar.child(SegmentedControl::new(&names, current).label("Agent").on_pick(move |_: &mut S, i: usize, _: &mut Tx| tell(Cmd::PickAgent(ids[i].clone()))).build(k));
    } else if let Some((name, id)) = snapshot.chosen.as_ref().and_then(|c| snapshot.agents.iter().find(|(_, id)| id == c)).map(|(n, i)| (n.clone(), i.clone())) {
        let _ = id;
        bar = bar.child(muted(k, name));
    }
    for setting in &snapshot.settings {
        let next = setting.choices.iter().position(|(v, _)| *v == setting.current).map(|i| (i + 1) % setting.choices.len()).and_then(|i| setting.choices.get(i)).map(|(v, _)| v.clone());
        let (id, busy) = (setting.id.clone(), snapshot.busy);
        let label = format!("{}: {}", setting.name, setting.current_name());
        bar = bar.child(Button::new(label).variant(ButtonVariant::Ghost).disabled(busy || next.is_none()).on_press(move |_: &mut S| {
            if let Some(value) = &next {
                tell(Cmd::Setting { id: id.clone(), value: value.clone() });
            }
        }).build(k));
    }
    if snapshot.blank
        && let Some(on) = snapshot.isolate
    {
        bar = bar.child(Switch::new(on).label("Own worktree").on_toggle(move |_: &mut S| tell(Cmd::Isolate(!on))).build(k)).child(muted(k, "Own worktree"));
    }
    bar
}

fn composer<S: 'static>(k: &Kit, snapshot: &Snapshot, state: &State, at: At<S>) -> El {
    let steer = snapshot.steer;
    let field = TextArea::new(&state.draft)
        .label("Message")
        .placeholder(if snapshot.blank { "What should the agent do?" } else { "Reply" })
        .auto_grow(2, 8)
        .width(None)
        .on_edit(move |s: &mut S| &mut at(s).draft)
        .on_change(move |s: &mut S, _: &mut Tx| tell(Cmd::Draft(at(s).draft.text.clone())))
        .on_key(move |_: &mut S, event: &KeyEvent, _: &mut Tx| {
            // Enter sends; Shift+Enter is a new line; Ctrl+Enter queues behind the turn.
            if event.key == Key::Enter && !event.mods.shift {
                tell(Cmd::Send { later: event.mods.ctrl });
                return true;
            }
            false
        })
        .build(k);
    let mut actions = row().gap(6.0);
    if snapshot.busy {
        actions = actions.child(Button::new(if steer { "Steer" } else { "Queue" }).variant(ButtonVariant::Ghost).on_press(|_: &mut S| tell(Cmd::Send { later: true })).build(k));
        actions = actions.child(Button::new("Stop").on_press(|_: &mut S| tell(Cmd::Stop)).build(k));
    } else {
        actions = actions.child(Button::new("Send").variant(ButtonVariant::Submit).on_press(|_: &mut S| tell(Cmd::Send { later: false })).build(k));
    }
    let mut core = col().w_full().max_w(COLUMN).gap(8.0).padding(8.0).radius(12.0).border(1.0, k.c.border_base);
    if let Some(note) = &snapshot.note {
        core = core.child(row().w_full().items_center().gap(8.0).child(row().grow(1.0).child(muted(k, note.clone()))).child(Button::new("Dismiss").variant(ButtonVariant::Ghost).on_press(|_: &mut S| tell(Cmd::DismissNote)).build(k)));
    }
    core = core.child(field).child(row().w_full().items_end().gap(8.0).child(chips::<S>(k, snapshot)).child(actions));
    let mut column = col().w_full().items_center().gap(6.0).px(12.0).pt(8.0).pb(12.0);
    if let Some(picking) = snapshot.picking.as_ref().filter(|p| **p != Picking::Loading) {
        column = column.child(col().w_full().max_w(COLUMN).child(picker::<S>(k, picking)));
    }
    if !snapshot.queue.is_empty() {
        let mut tray = col().w_full().max_w(COLUMN).py(4.0).radius(10.0).fill(k.c.layer_02);
        for q in &snapshot.queue {
            tray = tray.child(queued::<S>(k, q));
        }
        column = column.child(tray);
    }
    column.child(core)
}

/// The header's title and status lines.
fn header<S: 'static>(k: &Kit, snapshot: &Snapshot) -> El {
    let mut bar = row().w_full().items_center().gap(12.0).px(16.0).py(8.0).child(super::header::controls::<S>(k, &snapshot.lead)).child(row().grow(1.0).min_w(0.0).child(text(snapshot.title.clone()).font(BODY).color(k.c.text_base)));
    for status in &snapshot.statuses {
        bar = bar.child(muted(k, status.clone()));
    }
    bar.child(super::header::controls::<S>(k, &snapshot.trail))
}

pub fn view<S: 'static>(snapshot: &Snapshot, state: &State, k: &Kit, at: At<S>) -> El {
    col().w_full().h_full().min_h(0.0).child(header::<S>(k, snapshot)).child(transcript::<S>(k, snapshot)).child(composer::<S>(k, snapshot, state, at))
}
