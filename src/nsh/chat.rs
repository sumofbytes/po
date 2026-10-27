//! The conversation: the transcript with its tool cards, thoughts, plan, permission and approval
//! cards, the composer with its pictures and queue, the session picker, and the menus (the agent
//! to start with, the session's settings, what the agent may do through Po). [`Snapshot`] is
//! what [`crate::Reader`] publishes; the screen draws it, and what the user does on it comes
//! back as a [`Cmd`].
use std::sync::LazyLock;

use nsh_base::{menu, menu_item, menu_separator, text_view, Input, input};
use nsh_core::prelude::*;
use tokio::sync::watch;

use super::header::{Control, pane_header};
use super::source::Shared;
use super::{Colors, icon, mono_size, rems, text_base, text_sm, text_xs, tooltip};
use crate::ui::icon::IconName;

/// Whether a step of the agent's is waiting, going, done or failed: a tool call, the plan's
/// entry, a whole run of tool calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Waiting,
    Working,
    Done,
    Failed,
}

impl Phase {
    /// The glyph that stands for it.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Waiting => "○",
            Self::Working => "◐",
            Self::Done => "✓",
            Self::Failed => "✕",
        }
    }
}

/// How far a thread's agent may reach through Po's API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Viewer,
    Orchestrate,
    Global,
}

/// What the thread's agent may do through Po's API: its scope, and the reach it has beyond
/// seeing this project's threads and answering their asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grant {
    pub scope: Scope,
    pub read: bool,
    pub approvals: bool,
}

impl Default for Grant {
    fn default() -> Self {
        Self { scope: Scope::Viewer, read: false, approvals: false }
    }
}

/// One of the thread's session settings, like its model.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Setting {
    /// The id the agent gave it.
    pub id: String,
    /// What it is called, as the agent named it.
    pub name: String,
    /// The choice's value in effect.
    pub current: String,
    /// Each choice, `(value, name)` as the agent gave it.
    pub choices: Vec<(String, String)>,
}

/// One picture attached to the draft, as a data URI so the screen needs no file access.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    pub uri: String,
}

/// What waits to be sent when the turn is over: `kind` says how it got there (nothing is the
/// user's own; "Steer" or "Follow-up" is what the agent's daemon holds for later).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Queued {
    pub at: usize,
    pub kind: String,
    pub text: String,
    /// How many pictures go with it, when there is no text to show.
    pub pictures: usize,
}

impl Setting {
    /// The name of the choice in effect, or its value when the agent gave no name for it.
    pub fn current_name(&self) -> String {
        self.choices.iter().find(|(value, _)| *value == self.current).map_or(self.current.clone(), |(_, name)| name.clone())
    }
}

/// A session of the agent's, offered to resume.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Session {
    /// The id it is resumed by.
    pub id: String,
    /// The name the agent gave it, or nothing if it never said.
    pub title: String,
    /// When it was last at work, human-readable.
    pub when: String,
}

/// A tool call of the agent's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tool {
    /// Which card of the message it is, to press.
    pub index: usize,
    pub title: String,
    pub phase: Phase,
    /// Whether its call and result are shown; the title's click leaves them.
    pub open: bool,
    /// What the call went out as.
    pub arguments: String,
    /// What it came back with.
    pub result: String,
    /// A file the call wrote or changed, which opens in the other pane.
    pub path: Option<String>,
}

/// One entry of the plan.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub text: String,
    pub phase: Phase,
}

/// One way of answering a permission request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AskOption {
    /// Which of the request's options it is, to press.
    pub at: usize,
    pub label: String,
    /// The first way of saying yes, drawn filled.
    pub primary: bool,
}

/// One block of the agent's message. `index` is the block's place in the message, to press.
#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    /// The agent's text, as markdown.
    Markdown(Shared),
    /// Its reasoning, with whether it is expanded.
    Thought { index: usize, text: String, open: bool },
    Tool(Tool),
    /// A run of tool calls folded to one line: how many calls it keeps, how they went, and
    /// where it sits in the message.
    Run { index: usize, count: usize, phase: Phase, folded: bool },
    Plan(Vec<Entry>),
    /// A permission request, with the ways it may be answered; answered, it says what went.
    Ask { index: usize, title: String, options: Vec<AskOption>, answered: Option<String> },
    /// Po's command, offered to be run: what would run, and the commands of it, both below the
    /// plain word of the user.
    Approve { index: usize, question: String, lines: Vec<String>, answered: Option<String> },
}

/// One message: the user's, or the agent's column of blocks.
#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    User { text: String, pictures: Vec<Picture> },
    Agent { index: usize, blocks: Vec<Block> },
}

/// What resuming a session shows: the sessions on offer, or why there are none.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Picking {
    Sessions(Vec<Session>),
    Note(String),
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Snapshot {
    pub glass: bool,
    pub title: String,
    pub lead: Vec<Control>,
    pub trail: Vec<Control>,
    /// The status lines of the header's right, like `3 threads · 1 asking`.
    pub statuses: Vec<String>,
    /// Nothing on screen yet: the agent is picked below, and the thread starts when something
    /// is sent.
    pub blank: bool,
    /// A turn is going: the send button turns into a stop beside it.
    pub busy: bool,
    /// The agent takes messages mid-turn, so a send steers rather than queues.
    pub steer: bool,
    /// The agents a new thread can be started with, `(name, id)`.
    pub agents: Vec<(String, String)>,
    /// The agent picked for a thread that hasn't started.
    pub chosen: Option<String>,
    pub settings: Vec<Setting>,
    /// Whether the thread can start in a git worktree of its own (`None`: not in a repository).
    pub isolate: Option<bool>,
    /// What the thread's agent may do through Po: the Control menu reads and changes it.
    pub grant: Option<Grant>,
    pub queue: Vec<Queued>,
    pub pictures: Vec<Picture>,
    /// Why some of what was pasted or dropped was left out.
    pub note: Option<String>,
    /// The conversation, in order.
    pub items: Vec<Item>,
    /// The sessions to resume, or why there are none to show.
    pub picking: Option<Picking>,
    /// What the draft holds, as the app sees it; the screen keeps what the user types.
    pub draft: String,
    /// Bumped when the app wrote the draft itself (taking a queued message back, clearing it),
    /// so the screen takes that text over.
    pub draft_reset: u64,
    /// Bumped when the field should take the keyboard (a new thread opened for a message).
    pub focus: u64,
    /// Bumped when the transcript should follow its end (the user sent something).
    pub follow: u64,
}

static SNAPSHOT: LazyLock<watch::Sender<Snapshot>> = LazyLock::new(|| watch::Sender::new(Snapshot::default()));

/// Shows `snapshot`; nothing is redrawn when it is what the screen already has.
pub fn publish(snapshot: Snapshot) {
    SNAPSHOT.send_if_modified(|now| {
        let changed = *now != snapshot;
        if changed {
            *now = snapshot;
        }
        changed
    });
}

/// What the user did, for [`crate::Reader::run`].
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// The draft changed.
    Draft(String),
    /// Send the draft: now, or after the turn (`later`).
    Send { later: bool },
    /// Stop the agent.
    Stop,
    /// Pictures dropped on the pane, by their `file:` URLs.
    Drop(Vec<String>),
    /// Pictures pasted into the composer, with their bytes.
    Paste(Vec<Vec<u8>>),
    /// A picture comes off the draft.
    Detach(usize),
    /// A queued message goes back into the draft, to edit.
    Recall(usize),
    /// The sessions on offer are put away.
    ClosePicker,
    /// The note under the composer about what was left out is dismissed.
    DismissNote,
    /// The user gave the agent one of the ways of answering a permission request.
    Answer { message: usize, block: usize, at: usize },
    /// Po's command was approved (`yes`) or refused.
    Approve { message: usize, block: usize, yes: bool },
    /// A tool card's call and result are shown or hidden.
    ToggleTool { message: usize, block: usize },
    /// A thought is shown or hidden.
    ToggleThought { message: usize, block: usize },
    /// A run of tool calls folds, or unfolds.
    Fold { message: usize, block: usize, folded: bool },
    /// A tool call wrote or changed a file: it opens in the other pane.
    Open(String),
    /// Which session to resume.
    Resume(String),
    /// The agent's sessions are asked for.
    History,
    /// The agent a new thread starts with.
    PickAgent(String),
    /// A setting of the session is changed.
    Setting { id: String, value: String },
    /// Whether a new thread gets a git worktree of its own.
    Isolate(bool),
    /// What the thread's agent may do through Po.
    Grant(GrantChange),
}

/// What the user asked for on the Control menu: a new grant in whole, or one of its reach
/// opt-ins tipped over.
#[derive(Clone, Debug, PartialEq)]
pub enum GrantChange {
    Set(Grant),
    Read,
    Approvals,
    Stop,
}

/// Asks the Reader ([`super::bus`]); the conversation is [`super::bus::Cmd::Chat`].
fn tell(cmd: Cmd) {
    super::bus::send(super::bus::Cmd::Chat(cmd));
}

/// What a phase is drawn with: quiet while it waits, principal while it goes, and what it
/// reached in the tone it had.
fn phase_tint(phase: Phase) -> AdaptiveColor {
    match phase {
        Phase::Waiting => Colors::muted()(),
        Phase::Working => Colors::primary()(),
        Phase::Done => Colors::added()(),
        Phase::Failed => Colors::danger()(),
    }
}

/// The font the code the tools take is set in.
fn mono_family() -> SharedString {
    nsh_base::theme().font_mono.clone().unwrap_or_else(|| SharedString::from("monospace"))
}

/// A line between the groups of a menu's items.
fn separator() -> AnyElement {
    menu_separator().into_element()
}

/// The glyph that walks with it, in its tone.
fn step_glyph(phase: Phase) -> Text {
    text(phase.glyph().to_string()).text_color(phase_tint(phase))
}

/// The popovers of the menus: the theme's own surface, with a shadow.
const MENU: &str = "bg-popover border border-border rounded-lg shadow-lg py-1 min-w-44";

/// A menu item's row: the check when it is the one on, else the label alone.
fn choice(label: String, on: bool) -> String {
    if on {
        format!("✓ {label}")
    } else {
        label
    }
}

/// A menu's trigger: its label and the caret, as a ghost row the Menu opens on.
fn menu_trigger(label: String, tip: &'static str) -> View {
    view()
        .role(AccessibilityRole::Button)
        .label(tip)
        .flex_none()
        .flex_row()
        .items_center()
        .gap(rems(0.25))
        .px(rems(0.5))
        .py(rems(0.25))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(tooltip(tip))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(rems(0.25))
                .child(text(label).font_weight(FontWeight::MEDIUM).text_size(text_sm()).text_color(Colors::foreground()))
                .child(icon(IconName::ChevronDown, rems(0.75), Colors::muted())),
        )
}

/// A menu item's own looks, under the popover's.
fn item(what: String, on_press: impl Fn() + 'static) -> AnyElement {
    menu_item()
        .cursor_pointer()
        .px(rems(0.75))
        .py(rems(0.375))
        .hover(|s| s.bg(Colors::hover()))
        .on_select(on_press)
        .child(text(what).text_size(text_sm()))
        .into_element()
}

/// A line in a header, like `3 threads · 1 asking`.
fn status_line(line: String) -> Text {
    text(line).number_of_lines(1).text_size(text_xs()).text_color(Colors::muted())
}

/// The pane header of the conversation: the title, the statuses, the history button, the Control
/// menu, and the window's own controls at the ends.
fn pane(title: String, snap: &Snapshot) -> View {
    let mut right = view().flex_row().items_center().gap(rems(0.75));
    for line in &snap.statuses {
        right = right.child(status_line(line.clone()));
    }
    if snap.picking.is_none() {
        right = right.child(
            view()
                .role(AccessibilityRole::Button)
                .flex_none()
                .px(rems(0.5))
                .py(rems(0.25))
                .rounded(Colors::radius())
                .cursor_pointer()
                .hover(|s| s.bg(Colors::hover()))
                .on_press(|| tell(Cmd::History))
                .tooltip(tooltip("Resume an earlier session (Ctrl+Shift+H)"))
                .child(text("History").text_size(text_sm()).text_color(Colors::foreground())),
        );
    }
    // The Control menu, once the thread's agent may do more than talk.
    if let Some(grant) = snap.grant {
        right = right.child(grant_menu(grant));
    }
    pane_header(title, right, &snap.lead, &snap.trail, snap.glass)
}

/// What an agent that runs this thread may do through Po: the scope it reaches by, its reach
/// besides that, and a way back. [`tell`] tells the Reader.
fn grant_menu(grant: Grant) -> AnyElement {
    let label = match grant.scope {
        Scope::Viewer if !(grant.read || grant.approvals) => "Control".to_string(),
        Scope::Viewer => "Control · reads".to_string(),
        Scope::Orchestrate => "Control · orchestrate".to_string(),
        Scope::Global => "Control · global".to_string(),
    };
    let active = grant != Grant::default();
    let mut menu = menu()
        .popup_class(MENU)
        .trigger(menu_trigger(label.clone(), "What this thread's agent may do through Po"));
    for (name, scope) in [("Viewer", Scope::Viewer), ("Orchestrate", Scope::Orchestrate), ("Global", Scope::Global)] {
        let on = grant.scope == scope;
        let set = Grant { scope, ..grant };
        menu = menu.child(item(choice(name.to_string(), on), move || tell(Cmd::Grant(GrantChange::Set(set)))));
    }
    menu = menu.child(separator());
    menu = menu.child(item(choice("May read other threads here".to_string(), grant.read), move || tell(Cmd::Grant(GrantChange::Read))));
    if grant.approvals || grant.scope != Scope::Viewer {
        menu = menu.child(item(choice("May answer its threads' permission asks".to_string(), grant.approvals), move || tell(Cmd::Grant(GrantChange::Approvals))));
    }
    menu = menu.child(separator());
    menu = menu.child(menu_item().disabled(true).px(rems(0.75)).py(rems(0.375)).child(text("A raise takes effect when the agent restarts").text_size(text_sm()).text_color(Colors::muted())).into_element());
    if active {
        menu = menu.child(item("Stop agent control".to_string(), move || tell(Cmd::Grant(GrantChange::Stop))));
    }
    menu.into_element()
}

// ============ the transcript ============

// ============ the transcript ============

/// A message the user sent, in a rounded column at the right: its pictures, then its text.
fn user_row(text_: &str, pictures: &[Picture]) -> AnyElement {
    let mut bubble = view()
        .flex_col()
        .gap(rems(0.5))
        .min_w_0()
        .flex_shrink(1.)
        .max_w(px(700.))
        .px(rems(1.))
        .py(rems(0.75))
        .rounded(Colors::radius())
        .bg(Colors::selected())
        .children(
            pictures
                .iter()
                .map(|picture| {
                    image(picture.uri.clone())
                        .w_full()
                        .max_h(px(280.))
                        .rounded(Colors::radius())
                        .border_1()
                        .border_color(Colors::border())
                        .overflow_hidden()
                        .flex_none()
                })
                .collect::<Vec<_>>(),
        );
    if !text_.is_empty() {
        // Typed text is shown as typed: its `*` and `#` are not markup.
        bubble = bubble.child(text(text_.to_string()).selectable(true).text_base().text_color(Colors::foreground()));
    }
    view().flex_row().w_full().min_w_0().justify_end().child(bubble).into_element()
}

/// The agent's reasoning: a quiet row that opens to it in italics.
fn thought_row(message: usize, at: usize, text_: &str, open: bool) -> AnyElement {
    let mut column = view()
        .flex_col()
        .gap(rems(0.25))
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(rems(0.5))
                .py(rems(0.125))
                .cursor_pointer()
                .rounded(Colors::radius())
                .on_press(move || tell(Cmd::ToggleThought { message, block: at }))
                .child(
                    view()
                        .flex_1()
                        .min_w_0()
                        .child(text_view().markdown(text_.to_string()).italic().text_sm().text_color(Colors::muted()).opacity(crate::theme::tokens::DIM)),
                )
                .child(
                    view()
                        .flex_none()
                        .pt(rems(0.125))
                        .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, rems(0.75), Colors::muted())),
                ),
        );
    if open {
        column = column.child(
            text_view()
                .markdown(text_.to_string())
                .italic()
                .text_sm()
                .px(rems(0.75))
                .py(rems(0.25))
                .pl(rems(1.25))
                .opacity(crate::theme::tokens::DIM)
                .text_color(Colors::muted()),
        );
    }
    column.into_element()
}

/// One call of the agent's tools: its glyph and title, what it sent and what came back once it
/// is open, and a way to the file it touched.
fn tool_row(message: usize, at: usize, tool: &Tool) -> AnyElement {
    let settled = tool.phase == Phase::Done;
    let title = tool.title.clone();
    let mut card = view()
        .flex_col()
        .gap(rems(0.25))
        .py(rems(0.25))
        .px(rems(0.75))
        .rounded(Colors::radius())
        .hover(|s| s.bg(Colors::hover()))
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(rems(0.5))
                .child(
                    view()
                        .flex_row()
                        .items_start()
                        .gap(rems(0.5))
                        .flex_1()
                        .min_w_0()
                        .cursor_pointer()
                        .on_press(move || tell(Cmd::ToggleTool { message, block: at }))
                        .child(step_glyph(tool.phase))
                        .child(
                            text(title)
                                .number_of_lines(1)
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(move || if settled { Colors::muted()() } else { Colors::foreground()() }),
                        ),
                )
                .children(tool.path.clone().map(|path| {
                    view()
                        .role(AccessibilityRole::Button)
                        .label("Open the file it changed")
                        .flex_none()
                        .px(rems(0.5))
                        .py(rems(0.125))
                        .rounded(Colors::radius())
                        .cursor_pointer()
                        .hover(|s| s.bg(Colors::hover()))
                        .on_press(move || tell(Cmd::Open(path.clone())))
                        .child(icon(IconName::FileText, rems(0.75), Colors::muted()))
                })),
        );
    if tool.open {
        for piece in [&tool.arguments, &tool.result] {
            if piece.is_empty() {
                continue;
            }
            card = card.child(
                view()
                    .max_h(px(448.))
                    .border_l_1()
                    .border_color(Colors::border())
                    .pl(rems(0.75))
                    .child(scroll().max_h(px(448.)).child(
                        text(piece.clone())
                            .text_color(Colors::muted())
                            .text_size(px(mono_size()))
                            .line_height(px(mono_size() * 1.5))
                            .font_family(mono_family()),
                    )),
            );
        }
    }
    card.into_element()
}

/// A run of tool calls, folded to one row: what is in it and how it went; a press on the row
/// folds or unfolds it.
fn run_row(message: usize, at: usize, count: usize, phase: Phase, folded: bool) -> AnyElement {
    let label = format!("{count} tool calls");
    view()
        .role(AccessibilityRole::Button)
        .label(label.clone())
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .py(rems(0.25))
        .px(rems(0.75))
        .rounded(Colors::radius())
        .cursor_pointer()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || tell(Cmd::Fold { message, block: at, folded: !folded }))
        .child(step_glyph(phase))
        .child(text(label).flex_1().text_sm().text_color(Colors::muted()))
        .child(icon(if folded { IconName::ChevronRight } else { IconName::ChevronDown }, rems(0.75), Colors::muted()))
        .into_element()
}

/// The plan: each entry with its phase; the ones done, quietly.
fn plan_rows(entries: &[Entry]) -> AnyElement {
    view()
        .flex_col()
        .gap(rems(0.375))
        .py(rems(0.25))
        .children(entries.iter().map(|entry| {
            let (entry_text, entry_phase) = (entry.text.clone(), entry.phase);
            view()
                .flex_row()
                .items_start()
                .gap(rems(0.5))
                .child(
                    text_view()
                        .markdown(entry_text)
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(move || if entry_phase == Phase::Done { Colors::muted()() } else { Colors::foreground()() }),
                )
        }))
        .into_element()
}

/// A question of the agent's: what it is, the ways the user may answer, and what it got when it
/// has one.
fn ask_row(message: usize, at: usize, title: &str, options: &[AskOption], answered: &Option<String>) -> AnyElement {
    let waiting = answered.is_none();
    let title = title.to_string();
    let mut card = view()
        .flex_col()
        .gap(rems(0.5))
        .py(rems(0.75))
        .px(rems(0.75))
        .rounded(Colors::radius())
        .border_1()
        .border_color(move || if waiting { Colors::warning()().opacity(0.4) } else { Colors::border()() })
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(rems(0.5))
                .child(text("Permission").flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
                .child(view().flex_1().min_w_0().child(text_view().markdown(title).text_sm().text_color(Colors::foreground()))),
        );
    match answered {
        Some(given) => {
            let said = format!("→ {given}");
            card = card.child(text(said).text_sm().text_color(Colors::muted()));
        }
        None => {
            let mut row = view().flex_row().flex_wrap().gap(rems(0.375));
            for option in options {
                let (way, label, primary) = (option.at, option.label.clone(), option.primary);
                let mut reply = view()
                    .role(AccessibilityRole::Button)
                    .label(label.clone())
                    .px(rems(0.75))
                    .py(rems(0.25))
                    .rounded(Colors::radius())
                    .cursor_pointer()
                    .on_press(move || tell(Cmd::Answer { message, block: at, at: way }));
                reply = if primary {
                    reply.bg(Colors::primary())
                } else {
                    reply.border_1().border_color(Colors::input_border()).hover(|s| s.bg(Colors::hover()))
                };
                row = row.child(
                    reply.child(text(label).text_sm().text_color(move || if primary { Colors::primary_foreground()() } else { Colors::foreground()() })),
                );
            }
            card = card.child(row);
        }
    }
    card.into_element()
}

/// What Po asks the user to let it do: what would run and the commands of it, and the run and
/// leave-out buttons the user answers by. Answered, it says what went.
fn approve_row(message: usize, at: usize, question: &str, lines: &[String], answered: &Option<String>) -> AnyElement {
    let waiting = answered.is_none();
    let title = question.to_string();
    let mut card = view()
        .flex_col()
        .gap(rems(0.5))
        .py(rems(0.75))
        .px(rems(0.75))
        .rounded(Colors::radius())
        .border_1()
        .border_color(move || if waiting { Colors::warning()().opacity(0.4) } else { Colors::border()() })
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(rems(0.5))
                .child(text("Approve").flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
                .child(view().flex_1().min_w_0().child(text_view().markdown(title).text_sm().text_color(Colors::foreground()))),
        )
        .children((!lines.is_empty()).then(|| {
            view()
                .flex_col()
                .max_h(px(224.))
                .overflow_hidden()
                .border_l_1()
                .border_color(Colors::border())
                .pl(rems(0.75))
                .children(
                    lines
                        .iter()
                        .map(|line| {
                            text(line.clone())
                                .text_color(Colors::foreground())
                                .text_size(px(mono_size()))
                                .line_height(px(mono_size() * 1.5))
                                .font_family(mono_family())
                        })
                        .collect::<Vec<_>>(),
                )
        }));
    match answered {
        Some(said) => {
            let said = format!("→ {said}");
            card = card.child(text(said).text_sm().text_color(Colors::muted()));
        }
        None => {
            card = card.child(
                view()
                    .flex_row()
                    .flex_wrap()
                    .gap(rems(0.375))
                    .child(
                        view()
                            .role(AccessibilityRole::Button)
                            .label("Run the command")
                            .px(rems(0.75))
                            .py(rems(0.25))
                            .rounded(Colors::radius())
                            .cursor_pointer()
                            .bg(Colors::primary())
                            .on_press(move || tell(Cmd::Approve { message, block: at, yes: true }))
                            .child(text("Run it").text_sm().text_color(Colors::primary_foreground())),
                    )
                    .child(
                        view()
                            .role(AccessibilityRole::Button)
                            .label("Leave the command out")
                            .px(rems(0.75))
                            .py(rems(0.25))
                            .rounded(Colors::radius())
                            .cursor_pointer()
                            .border_1()
                            .border_color(Colors::input_border())
                            .hover(|s| s.bg(Colors::hover()))
                            .on_press(move || tell(Cmd::Approve { message, block: at, yes: false }))
                            .child(text("Leave out").text_sm().text_color(Colors::foreground())),
                    ),
            );
        }
    }
    card.into_element()
}

/// One block of the agent's message. What it reports is its own place in the message
/// ([`Block`]'s `index`), not its place among the rows drawn: a folded run adds a row of its own.
fn block_row(message: usize, block: &Block) -> AnyElement {
    match block {
        Block::Markdown(markdown) => text_view()
            .markdown(markdown.0.to_string())
            .selectable(true)
            .text_base()
            .text_color(Colors::foreground())
            .into_element(),
        Block::Thought { index, text: thought, open } => thought_row(message, *index, thought, *open),
        Block::Tool(tool) => tool_row(message, tool.index, tool),
        Block::Run { index, count, phase, folded } => run_row(message, *index, *count, *phase, *folded),
        Block::Plan(entries) => plan_rows(entries),
        Block::Ask { index, title, options, answered } => ask_row(message, *index, title, options, answered),
        Block::Approve { index, question, lines, answered } => approve_row(message, *index, question, lines, answered),
    }
}

/// One message of the agent's: its blocks, a run's own row standing in for the tools it hides.
fn agent_row(message: usize, blocks: &[Block]) -> AnyElement {
    let mut column = view().flex_col().min_w_0().gap(rems(0.75));
    let mut skip = 0;
    if blocks.is_empty() {
        column = column.child(text("Thinking…").text_sm().text_color(Colors::muted()));
    }
    for block in blocks {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if let Block::Run { count, folded: true, .. } = block {
            // The tools the run hides; its own row takes their place.
            skip = *count;
        }
        column = column.child(block_row(message, block));
    }
    column.into_element()
}

/// The conversation: the messages, in order, as a center column.
fn transcript(items: &[Item]) -> AnyElement {
    let mut rows = view().w_full().min_w_0().max_w(px(760.)).flex_col().gap(rems(1.5)).px(rems(1.5)).py(rems(1.5));
    for (message, item) in items.iter().enumerate() {
        let row = match item {
            Item::User { text, pictures } => user_row(text, pictures),
            Item::Agent { blocks, .. } => agent_row(message, blocks),
        };
        rows = rows.child(row);
    }
    view()
        .flex_1()
        .min_h_0()
        .flex()
        .justify_center()
        .overflow_hidden()
        .child(scroll().size_full().child(view().flex_col().items_center().w_full().min_w_0().child(rows.into_element())).element_ref(&transcript_slot()))
        .into_element()
}

/// The ref the transcript's scroll is found by, to follow its end.
fn transcript_slot() -> ElementRef {
    element_ref()
}

// ============ the composer ============

/// The pictures of the draft: their strip, each with a way to take it off.
fn pictures_row(pictures: &[Picture]) -> AnyElement {
    view()
        .flex_row()
        .flex_wrap()
        .items_start()
        .gap(rems(0.5))
        .children(
            pictures
                .iter()
                .enumerate()
                .map(|(at, picture)| {
                    view()
                        .relative()
                        .flex_none()
                        .child(
                            image(picture.uri.clone())
                                .rounded(Colors::radius())
                                .border_1()
                                .border_color(Colors::border())
                                .max_h(px(72.))
                                .max_w(px(120.))
                                .overflow_hidden(),
                        )
                        .child(
                            view()
                                .role(AccessibilityRole::Button)
                                .label("Remove the picture")
                                .absolute()
                                .top(rems(-0.25))
                                .right(rems(-0.25))
                                .p(rems(0.125))
                                .rounded_full()
                                .bg(Colors::background())
                                .border_1()
                                .border_color(Colors::border())
                                .cursor_pointer()
                                .hover(|s| s.bg(Colors::hover()))
                                .on_press(move || tell(Cmd::Detach(at)))
                                .child(icon(IconName::Close, rems(0.625), Colors::muted())),
                        )
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// One message waiting to be sent when the turn is over: its kind, what it says, and the
/// take-back and drop buttons.
fn queue_row(queued: Queued) -> AnyElement {
    let mut row = view()
        .flex_row()
        .items_start()
        .gap(rems(0.5))
        .px(rems(0.75))
        .py(rems(0.375))
        .rounded(Colors::radius())
        .bg(Colors::selected());
    row = if queued.kind.is_empty() {
        row.child(text("Queued").flex_none().pt(rems(0.0625)).text_xs().font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
    } else {
        row.child(text(queued.kind.clone()).flex_none().pt(rems(0.0625)).text_xs().font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
    };
    row = if queued.text.is_empty() {
        let word = if queued.pictures == 1 { "1 picture".to_string() } else { format!("{} pictures", queued.pictures) };
        row.child(text(word).flex_1().min_w_0().text_sm().text_color(Colors::muted()))
    } else {
        row.child(text_view().markdown(queued.text.clone()).flex_1().min_w_0().text_sm().text_color(Colors::foreground()))
    };
    row = row
        .child(
            view()
                .role(AccessibilityRole::Button)
                .label("Take back into the draft")
                .flex_none()
                .p(rems(0.125))
                .rounded_full()
                .cursor_pointer()
                .hover(|s| s.bg(Colors::hover()))
                .on_press(move || tell(Cmd::Recall(queued.at)))
                .child(icon(IconName::Pencil, rems(0.625), Colors::muted())),
        );
    row.into_element()
}

/// Why some of what was pasted or dropped was left out, with a way to dismiss it.
fn note_row(note: &str) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(rems(0.5))
        .child(text(note.to_string()).flex_1().min_w_0().text_xs().text_color(Colors::muted()))
        .child(
            view()
                .role(AccessibilityRole::Button)
                .label("Dismiss")
                .flex_none()
                .p(rems(0.125))
                .rounded_full()
                .cursor_pointer()
                .hover(|s| s.bg(Colors::hover()))
                .on_press(|| tell(Cmd::DismissNote))
                .child(icon(IconName::Close, rems(0.625), Colors::muted())),
        )
        .into_element()
}

/// The sessions to resume, above the composer while the picker is open.
fn picker(picking: &Picking) -> AnyElement {
    let (title, rows) = match picking {
        Picking::Sessions(sessions) => ("Resume".to_string(), {
            let mut column = view().flex_col();
            for session in sessions {
                let (id, name, when) = (session.id.clone(), session.title.clone(), session.when.clone());
                let label = if name.is_empty() { "Untitled".to_string() } else { name };
                column = column.child(
                    view()
                        .role(AccessibilityRole::Button)
                        .label(label.clone())
                        .flex_row()
                        .items_center()
                        .gap(rems(0.75))
                        .px(rems(0.75))
                        .py(rems(0.375))
                        .rounded(Colors::radius())
                        .cursor_pointer()
                        .hover(|s| s.bg(Colors::hover()))
                        .on_press(move || tell(Cmd::Resume(id.clone())))
                        .child(text(label).flex_1().min_w_0().number_of_lines(1).text_sm().text_color(Colors::foreground()))
                        .child(text(when).flex_none().text_xs().text_color(Colors::muted())),
                );
            }
            column.into_element()
        }),
        Picking::Note(note) => (note.clone(), view().into_element()),
    };
    view()
        .flex_col()
        .mb(rems(0.625))
        .py(rems(0.375))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .overflow_hidden()
        .child(
            view()
                .flex_row()
                .items_center()
                .px(rems(0.375))
                .pb(rems(0.25))
                .child(text(title).flex_1().text_xs().font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
                .child(
                    view()
                        .role(AccessibilityRole::Button)
                        .label("Close")
                        .flex_none()
                        .p(rems(0.125))
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| s.bg(Colors::hover()))
                        .on_press(|| tell(Cmd::ClosePicker))
                        .child(icon(IconName::Close, rems(0.625), Colors::muted())),
                ),
        )
        .child(scroll().max_h(px(320.)).child(rows))
        .into_element()
}

/// What a thread waits on before it starts: the agent to run it, and whether it starts in a git
/// worktree of its own.
fn setup(snap: &Snapshot) -> AnyElement {
    let mut column = view().flex_col().gap(rems(0.625)).pb(rems(0.625));
    let name = snap
        .chosen
        .as_ref()
        .map(|id| snap.agents.iter().find(|(_, key)| key == id).map_or(id.clone(), |(name, _)| name.clone()))
        .unwrap_or_else(|| "Pick an agent".to_string());
    let mut agents = menu().popup_class(MENU).trigger(menu_trigger(name, "Which agent the thread runs"));
    let pick = snap.chosen.clone();
    for (agent_name, id) in snap.agents.clone() {
        let on = pick.as_ref() == Some(&id);
        agents = agents.child(item(choice(agent_name, on), move || {
            let id = id.clone();
            tell(Cmd::PickAgent(id))
        }));
    }
    column = column.child(agents.into_element());
    if let Some(isolate) = snap.isolate {
        let tip = "Start the thread in a git worktree of its own";
        let box_ = view()
            .w(rems(0.875))
            .h(rems(0.875))
            .rounded(Colors::radius())
            .border_1()
            .border_color(Colors::input_border())
            .bg(Colors::input_background())
            .flex_none()
            .flex_row()
            .items_center()
            .justify_center()
            .style_when(move || isolate, |s| s.bg(Colors::primary()).border_color(Colors::primary()))
            .children(isolate.then(|| icon(IconName::SquareCheck, rems(0.75), Colors::primary_foreground())));
        column = column.child(
            view()
                .role(AccessibilityRole::Button)
                .label(tip)
                .flex_row()
                .items_center()
                .gap(rems(0.5))
                .cursor_pointer()
                .hover(|s| s.bg(Colors::hover()))
                .tooltip(tooltip(tip))
                .on_press(move || tell(Cmd::Isolate(!isolate)))
                .child(box_)
                .child(text("Start in a worktree of its own").text_sm().text_color(Colors::foreground())),
        );
    }
    column.into_element()
}

/// The settings of the session, as menus named for each.
/// A menu for each of the session's settings. While a turn runs they wait: agents differ on
/// changing them mid-turn.
fn settings_rows(settings: &[Setting], busy: bool) -> AnyElement {
    view()
        .flex_row()
        .flex_wrap()
        .gap(rems(0.5))
        .children(
            settings
                .iter()
                .map(|setting| {
                    if busy {
                        return menu_trigger(setting.current_name(), "Available when the agent is idle").opacity(0.5).into_element();
                    }
                    let mut menu = menu().popup_class(MENU).trigger(menu_trigger(setting.current_name(), "What the session uses"));
                    for (value, name) in &setting.choices {
                        let on = *value == setting.current;
                        let (decided, id) = (value.clone(), setting.id.clone());
                        menu = menu.child(item(choice(name.clone(), on), move || {
                            let (id, decided) = (id.clone(), decided.clone());
                            tell(Cmd::Setting { id, value: decided })
                        }));
                    }
                    menu.into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// Sends the draft, as `later`.
fn send_row(draft: &Signal<String>, state: &Signal<Snapshot>, later: bool) {
    let empty = draft.get_untracked().trim().is_empty();
    if empty && state.get_untracked().pictures.is_empty() {
        return;
    }
    tell(Cmd::Send { later });
}

/// While a turn runs, what is typed is sent into it (steered) or queued for its end.
fn steer_button(steer: bool, draft: Signal<String>, has_pictures: bool) -> AnyElement {
    let (label, tip) = if steer {
        ("Steer", "Steer: the agent reads it before its next step (Alt+Enter: after this turn)")
    } else {
        ("Queue", "Queue: sent when this turn is over")
    };
    let typed = draft.clone();
    view()
        .role(AccessibilityRole::Button)
        .label(tip)
        .px(rems(0.75))
        .py(rems(0.375))
        .rounded(Colors::radius())
        .border_1()
        .border_color(Colors::input_border())
        .cursor_pointer()
        .style_when(move || typed.get().trim().is_empty() && !has_pictures, |s| s.opacity(0.4))
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || {
            let text = draft.get_untracked();
            if !text.trim().is_empty() || has_pictures {
                tell(Cmd::Draft(text));
                tell(Cmd::Send { later: false });
            }
        })
        .child(text(label).text_sm().font_weight(FontWeight::MEDIUM).text_color(Colors::foreground()))
        .into_element()
}

/// The send button: says what it does now, and does it.
fn send_button(snap: Snapshot, draft: Signal<String>) -> AnyElement {
    let (label, tip) = if snap.blank {
        ("Start", "Start the thread")
    } else if snap.busy {
        ("Stop", "Stop the agent")
    } else {
        ("Send", "Send now")
    };
    let busy = snap.busy;
    view()
        .role(AccessibilityRole::Button)
        .label(tip)
        .px(rems(0.75))
        .py(rems(0.375))
        .rounded(Colors::radius())
        .cursor_pointer()
        .style_when(
            move || !busy && draft.get_untracked().trim().is_empty(),
            |s| s.opacity(0.4),
        )
        .hover(|s| s.opacity(0.85))
        .bg(Colors::primary())
        .on_press(move || {
            if busy {
                tell(Cmd::Stop);
            } else {
                pressed_send(&draft);
            }
        })
        .child(text(label).text_sm().font_weight(FontWeight::MEDIUM).text_color(Colors::primary_foreground()))
        .into_element()
}

/// The field the draft is typed in, reporting what the user pasted:
/// the pictures and files it came with, and the text it says.
fn draft_field(draft: &Signal<String>, field_ref: ElementRef) -> Input {
    let held = draft.clone();
    input()
        .test_id("chat-draft")
        .element_ref(&field_ref)
        .flex_1()
        .min_w_0()
        .max_h(px(160.))
        .text_size(text_base())
        .multiline(true)
        .value(move || held.get())
        .text_color(Colors::foreground())
        .selection_color(Colors::one(Colors::caret()))
        .auto_correct(false)
        .placeholder("Message the agent")
        .placeholder_color(Colors::one(Colors::muted()))
        .submit_behavior(nsh_core::element::SubmitBehavior::Submit)
        .paste_behavior(nsh_core::element::PasteBehavior::Report)
        .on_change(move |text: &str| {
            let text = text.to_string();
            held.set(text.clone());
            tell(Cmd::Draft(text));
        })
        .on_paste(|paste: &PasteEvent| {
            if !paste.files.is_empty() {
                let data = paste.files.iter().map(|file| file.data.clone()).collect::<Vec<_>>();
                tell(Cmd::Paste(data));
            } else if !paste.uris.is_empty() {
                tell(Cmd::Drop(paste.uris.clone()));
            }
        })
        .on_submit(|text: &str| {
            let text = text.to_string();
            tell(Cmd::Draft(text.clone()));
            tell(Cmd::Send { later: false });
        })
        .on_file_drop(move |drop: &FileDropEvent| tell(Cmd::Drop(drop.uris.clone())))
}

/// The composer: the note, the strip of pictures, the setup of a thread that waits to start,
/// the field, and the send.
fn bar(snap: Snapshot, draft: Signal<String>, field_ref: ElementRef) -> AnyElement {
    let field = draft.clone();
    let low = draft.clone();
    let has_pictures = !snap.pictures.is_empty();
    let mut column = view().flex_none().flex_col().gap(rems(0.375)).px(rems(0.75)).py(rems(0.625)).border_t_1().border_color(Colors::border());
    if let Some(note) = &snap.note {
        column = column.children([note_row(note.as_str())]);
    }
    if !snap.pictures.is_empty() {
        column = column.children([pictures_row(&snap.pictures)]);
    }
    if !snap.queue.is_empty() {
        column = column.child(view().flex_col().gap(rems(0.25)).child(snap.queue.iter().cloned().map(queue_row).collect::<Vec<_>>()));
    }
    if let Some(picking) = &snap.picking {
        column = column.children([picker(picking)]);
    }
    if snap.blank {
        column = column.children([setup(&snap), settings_rows(&snap.settings, snap.busy)]);
    } else if !snap.settings.is_empty() {
        column = column.child(settings_rows(&snap.settings, snap.busy));
    }
    column = column.child(
        view()
            .flex_row()
            .items_end()
            .gap(rems(0.5))
            .px(rems(0.75))
            .py(rems(0.5))
            .rounded(Colors::radius() * 2.)
            .border_1()
            .border_color(Colors::input_border())
            .bg(Colors::input_background())
            .on_file_drop(move |drop: &FileDropEvent| tell(Cmd::Drop(drop.uris.clone())))
            .child(draft_field(&field, field_ref))
            .children(snap.busy.then(|| steer_button(snap.steer, low.clone(), has_pictures)))
            .child(send_button(snap, low)),
    );
    column.into_element()
}

/// Sends the draft's text back to the app one last time and then takes the app's copy as truth.
fn pressed_send(draft: &Signal<String>) {
    let text = draft.get_untracked();
    tell(Cmd::Draft(text.clone()));
    tell(Cmd::Send { later: false });
}

// ============ the screen ============

/// The conversation: its header, the transcript from [`Snapshot::items`], and the composer bar,
/// which follows what its [`Snapshot`] says as it changes.
#[component]
pub fn Chat() -> impl IntoElement {
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        while snapshot.changed().await.is_ok() {
            state.set(snapshot.borrow_and_update().clone());
        }
    });

    // What the draft field holds: the user's typing, taken over by the app's own writes (a
    // cleared draft after a send, a taken-back queue row), which the sentinel marks.
    let draft = signal(state.with_untracked(|s| s.draft.clone()));
    let taken = stored(u64::MAX);
    effect(move || {
        let (sentinel, text) = state.with(|s| (s.draft_reset, s.draft.clone()));
        if taken.replace(sentinel) != sentinel && text != draft.get_untracked() {
            draft.set(text);
        }
    });

    // The field takes the keyboard whenever the app asks it to.
    let field_ref = element_ref();
    let focused = stored(0_u64);
    effect(move || {
        let bump = state.with(|s| s.focus);
        if bump > 0 && focused.replace(bump) != bump {
            field_ref.focus();
        }
    });

    // Alt+Enter sends the draft after the turn rather than into it.
    on_key_with("alt+enter", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, move |_| {
        if state.with_untracked(|s| s.busy) {
            let text = draft.get_untracked();
            tell(Cmd::Draft(text));
            send_row(&draft, &state, true);
        }
    });

    // A press of Ctrl+Shift+H opens the picker of the agent's earlier sessions.
    on_key_with(
        "mod+shift+h",
        KeyOptions { when: nsh_core::keymap::When::Always, repeat: false },
        |_| tell(Cmd::History),
    );

    // The transcript follows its end whenever the app asks it to (a message sent, a turn going).
    let scrolled = element_ref();
    let seen = stored(u64::MAX);
    effect(move || {
        let bump = state.with(|s| s.follow);
        if bump > 0 && seen.replace(bump) != bump {
            scrolled.scroll_to_end(false);
        }
    });

    let header = move || {
        let (title, snap) = (state.with(|s| s.title.clone()), state.get());
        pane(title, &snap)
    };

    let body = move || {
        let snap = state.get();
        // A thread that has not started says so where the transcript will be; its composer, and
        // the choice of agent in it, are there all the same.
        let top = if snap.items.is_empty() && snap.blank {
            view()
                .flex_1()
                .min_h_0()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(rems(0.75))
                .px(rems(1.))
                .children([text("What is this thread about?").text_base().text_color(Colors::muted()), text("Message an agent to begin.").text_sm().text_color(Colors::muted())])
                .into_element()
        } else {
            transcript(&snap.items)
        };
        view()
            .flex_1()
            .min_h_0()
            .flex_col()
            .children([top, bar(snap, draft, field_ref)])
            .into_element()
    };

    view()
        .flex_1()
        .min_h_0()
        .flex_col()
        .bg(Colors::background())
        .child(dynamic(header))
        .child(dynamic(body))
}

#[cfg(test)]
mod tests {
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    use super::*;
    use crate::nsh::bus::Cmd as Bus;

    #[test]
    fn queued_messages_can_be_taken_back_and_a_busy_turn_offers_steer_and_stop() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        publish(Snapshot {
            title: "thread".into(),
            busy: true,
            steer: true,
            queue: vec![Queued { at: 0, kind: "Follow-up".into(), text: "then run the tests".into(), pictures: 0 }],
            ..Snapshot::default()
        });
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Chat>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(700.), Points(600.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        assert!(app.shows("Follow-up"), "{:?}", app.screen().texts());
        let mut drain = |app: &mut Headless| {
            app.settle();
            std::iter::from_fn(|| commands.try_recv().ok()).collect::<Vec<_>>()
        };
        app.press("label:Take back into the draft").unwrap();
        assert_eq!(drain(&mut app), [Bus::Chat(Cmd::Recall(0))]);
        app.press("label:Stop the agent").unwrap();
        assert_eq!(drain(&mut app), [Bus::Chat(Cmd::Stop)]);
        // Nothing typed: Steer does nothing. Typed, it sends into the turn.
        app.press("label:Steer: the agent reads it before its next step (Alt+Enter: after this turn)").unwrap();
        assert!(drain(&mut app).is_empty());
        app.type_text("id:chat-draft", "also fix the typo").unwrap();
        app.press("label:Steer: the agent reads it before its next step (Alt+Enter: after this turn)").unwrap();
        let sent = drain(&mut app);
        assert!(sent.contains(&Bus::Chat(Cmd::Send { later: false })), "{sent:?}");
    }

    fn start(snapshot: Snapshot) -> Headless {
        publish(snapshot);
        let mut config = EngineConfig::new(nsh_platform_gpui::Root::of::<Chat>());
        config.ui_sets = vec![nsh_base::registry()];
        config.viewport = Size::new(Points(700.), Points(700.));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.os = nsh_core::platform::Os::Linux;
        let mut app = Headless::start(config).expect("engine");
        app.first_render().expect("first render");
        app.settle();
        app
    }

    fn tool(index: usize) -> Block {
        Block::Tool(Tool { index, title: format!("call {index}"), phase: Phase::Done, open: false, arguments: String::new(), result: String::new(), path: None })
    }

    fn drain(app: &mut Headless, commands: &mut tokio::sync::mpsc::UnboundedReceiver<Bus>) -> Vec<Bus> {
        app.settle();
        std::iter::from_fn(|| commands.try_recv().ok()).collect()
    }

    #[test]
    fn presses_name_the_block_of_the_message_not_the_row_drawn() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        // Four calls in one run (the run's row stands before them), then a permission request at
        // the message's block 4, then a second run of three calls at 5..8.
        let mut blocks = vec![Block::Run { index: 0, count: 4, phase: Phase::Done, folded: false }];
        blocks.extend((0..4).map(tool));
        blocks.push(Block::Ask { index: 4, title: "Run rm?".into(), options: vec![AskOption { at: 0, label: "Allow once".into(), primary: true }], answered: None });
        blocks.push(Block::Run { index: 5, count: 3, phase: Phase::Done, folded: false });
        blocks.extend((5..8).map(tool));
        let mut app = start(Snapshot { items: vec![Item::Agent { index: 0, blocks }], ..Snapshot::default() });
        app.press("label:Allow once").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::Answer { message: 0, block: 4, at: 0 })]);
        // Two runs say "tool calls": the first is four, the second three.
        app.press("label:3 tool calls").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::Fold { message: 0, block: 5, folded: true })]);
    }

    #[test]
    fn the_picker_closes_without_stopping_and_a_note_is_dismissed_on_its_own() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut app = start(Snapshot {
            picking: Some(Picking::Note("Loading sessions…".into())),
            note: Some("Left out a big file".into()),
            ..Snapshot::default()
        });
        assert!(app.shows("Loading sessions…") && !app.shows("No sessions to resume"), "{:?}", app.screen().texts());
        app.press("label:Close").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::ClosePicker)]);
        app.press("label:Dismiss").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::DismissNote)]);
    }

    #[test]
    fn settings_stay_after_the_thread_starts_wait_while_it_is_busy_and_a_new_turn_says_it_is_thinking() {
        let mut guard = crate::nsh::bus::lock();
        let _commands = guard.as_mut().expect("the channel");
        let settings = vec![Setting { id: "model".into(), name: "Model".into(), current: "b".into(), choices: vec![("a".into(), "Fast".into()), ("b".into(), "Smart".into())] }];
        let mut app = start(Snapshot { settings: settings.clone(), items: vec![Item::Agent { index: 0, blocks: vec![] }], ..Snapshot::default() });
        assert!(app.shows("Smart"), "{:?}", app.screen().texts());
        assert!(app.shows("Thinking…"), "{:?}", app.screen().texts());
        publish(Snapshot { settings, busy: true, items: vec![Item::Agent { index: 0, blocks: vec![] }], ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert!(app.shows("Smart"), "{:?}", app.screen().texts());
    }

    #[test]
    fn a_thread_that_has_not_started_still_has_its_composer_and_agent_choice() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut app = start(Snapshot {
            blank: true,
            agents: vec![("Claude".into(), "claude".into())],
            chosen: Some("claude".into()),
            ..Snapshot::default()
        });
        assert!(app.shows("Message an agent to begin."), "{:?}", app.screen().texts());
        assert!(app.shows("Claude"), "{:?}", app.screen().texts());
        app.type_text("id:chat-draft", "hello").unwrap();
        app.press("label:Start the thread").unwrap();
        let sent = drain(&mut app, commands);
        assert!(sent.contains(&Bus::Chat(Cmd::Send { later: false })), "{sent:?}");
    }
}
