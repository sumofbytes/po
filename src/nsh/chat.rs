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
use super::{Colors, body, icon, mono_size, note, scale, text_base, text_sm, text_xs, tooltip, Sizing, Smooth};
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
    /// A run of tool calls folded to one line: how many calls it keeps, how many of them failed, how they went, and
    /// where it sits in the message.
    /// `task` is the plan entry it worked towards, which the row says in place of the count.
    Run { index: usize, count: usize, failed: usize, phase: Phase, folded: bool, task: Option<String> },
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
    /// The agent is waking or the sessions loading: the header says so, so the bar shows nothing.
    Loading,
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
    /// Bumped as the turn goes on; the transcript follows it only when it is already at its end.
    pub follow_soft: u64,
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

/// A link pressed in the transcript opens in the default browser.
fn open_link(press: &nsh_base::TextViewLinkPressEvent) {
    super::bus::send(super::bus::Cmd::OpenUrl(press.url.to_string()));
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
    nsh_base::theme().font_mono.clone().unwrap_or_else(|| SharedString::from(crate::fonts::BUNDLED_MONO_FAMILY))
}

/// A line between the groups of a menu's items.
fn separator() -> AnyElement {
    menu_separator().into_element()
}

/// The glyph that walks with it, in its tone.
fn step_glyph(phase: Phase) -> View {
    let name = match phase {
        Phase::Waiting => IconName::Circle,
        Phase::Working => IconName::CircleHalf,
        Phase::Done => IconName::CircleCheck,
        Phase::Failed => IconName::CircleX,
    };
    // A fixed slot, so the text beside it stays put as the glyph changes from one phase to the next.
    lead_slot(icon(name, STEP_ICON, move || phase_tint(phase)))
}

/// The size of every icon of a step: its status, its chevron and its file.
const STEP_ICON: Points = Points(14.);

/// A fixed slot for the small icon at the start of a step, as wide as [`step_glyph`], so the text
/// of every step starts on one line.
fn lead_slot(content: impl IntoElement) -> View {
    view().flex_none().w(scale::s4()).flex_row().justify_center().child(content)
}

/// A fixed slot for what ends a step, as wide as the button that opens the file of a tool call
/// (its icon and the padding of that button), so the ends of the steps line up too.
fn trail_slot(content: impl IntoElement) -> View {
    view().flex_none().w(scale::s2() + scale::s2() + STEP_ICON).flex_row().justify_center().child(content)
}

/// The popovers of the menus: the theme's own surface, with a shadow.
const MENU: &str = "bg-popover border border-border rounded-xl shadow-2xl p-1 min-w-44";

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
        .gap(scale::s1())
        .px(scale::s3())
        .py(scale::s1())
        .rounded(super::pill())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(tooltip(tip))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s1())
                // What is set reads quieter than what is typed; the pill is what says "press".
                .child(body(label).text_color(|| Colors::foreground()().opacity(crate::theme::tokens::HOVER)))
                .child(icon(IconName::ChevronDown, scale::s3(), Colors::dim(Colors::muted()))),
        )
}

/// A menu item's own looks, under the popover's.
fn item(what: String, on_press: impl Fn() + 'static) -> AnyElement {
    menu_item()
        .cursor_pointer()
        .px(scale::s3())
        .py(scale::s1_5())
        .hover(|s| s.bg(Colors::hover()))
        .on_select(on_press)
        .child(text(what).text_size(text_sm()))
        .into_element()
}

/// A line in a header, like `3 threads · 1 asking`.
fn status_line(line: String) -> View {
    // The height of the History chip beside it, so the two sit on one rhythm.
    view().flex_none().px(scale::s2()).py(scale::s1()).child(text(super::sentence(&line)).sized(text_sm()).text_color(Colors::muted()).number_of_lines(1))
}

/// The pane header of the conversation: the title, the statuses, the history button, the Control
/// menu, and the window's own controls at the ends.
fn pane(title: String, snap: &Snapshot) -> View {
    let mut right = view().flex_row().items_center().gap(scale::s1());
    for line in &snap.statuses {
        right = right.child(status_line(line.clone()));
    }
    if snap.picking.is_none() {
        right = right.child(
            view()
                .role(AccessibilityRole::Button)
                .label("History")
                .flex_none()
                .flex_row()
                .items_center()
                .gap(scale::s1_5())
                .px(scale::s3())
                .py(scale::s1())
                .rounded(super::pill())
                .bg(Colors::hover())
                .cursor_pointer()
                .smooth()
                .hover(|s| s.bg(Colors::selected()))
                .on_press(|| tell(Cmd::History))
                .tooltip(tooltip("Resume an earlier session (Ctrl+Shift+H)"))
                .child(icon(IconName::History, scale::s3(), Colors::muted()))
                .child(body("History").text_color(Colors::muted())),
        );
    }
    // The Control menu, once the thread's agent may do more than talk.
    if let Some(grant) = snap.grant.filter(|grant| *grant != Grant::default()) {
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
    menu = menu.child(menu_item().disabled(true).px(scale::s3()).py(scale::s1_5()).child(note("A raise takes effect when the agent restarts")).into_element());
    if active {
        menu = menu.child(item("Stop agent control".to_string(), move || tell(Cmd::Grant(GrantChange::Stop))));
    }
    menu.into_element()
}

// ============ the transcript ============

// ============ the transcript ============

/// The widths the conversation and the composer share, so the text of each starts and ends on
/// the same lines: the column's, the space outside it, and from the composer card's edge to its
/// text (the card's 1px border, its 0.5rem padding and the field's 0.5rem): `scale::s3` outside the
/// column and `scale::composer_text_inset` inside.
const COLUMN: f32 = 760.;

/// A message the user sent, in a rounded column at the right: its pictures, then its text.
fn user_row(text_: &str, pictures: &[Picture]) -> AnyElement {
    let mut bubble = view()
        .flex_col()
        .gap(scale::s2())
        .min_w_0()
        .flex_shrink(1.)
        .max_w(px(700.))
        .px(scale::s4())
        .py(scale::s2_5())
        .rounded(super::soft_radius())
        .bg(Colors::hover())
        .children(
            pictures
                .iter()
                .map(|picture| {
                    image(picture.uri.clone())
                        .w_full()
                        .max_h(px(280.))
                        .rounded(Colors::radius())
                        .border_1()
                        .border_color(super::hairline())
                        .overflow_hidden()
                        .flex_none()
                })
                .collect::<Vec<_>>(),
        );
    if !text_.is_empty() {
        // Typed text is shown as typed: its `*` and `#` are not markup.
        bubble = bubble.child(text(text_.to_string()).selectable(true).sized(text_base()).text_color(Colors::foreground()));
    }
    view().flex_row().w_full().min_w_0().justify_end().child(bubble).into_element()
}

/// The first line of a thought, without its markup, for the row it folds to.
fn thought_gist(text_: &str) -> String {
    // A fence line only names a language, so the gist is the first line of prose after it.
    let line = text_.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with("```")).unwrap_or("");
    let line = line.trim_start_matches(['#', '*', '_', '-', '>', ' ']);
    // A numbered item reads without its number.
    let line = line.split_once(". ").filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).map_or(line, |(_, rest)| rest);
    // Emphasis marks go; an underscore inside a word is part of its name.
    let gist = line.replace(['*', '`'], "");
    let gist = gist.trim_matches('_');
    gist.to_string()
}

/// The agent's reasoning: a quiet one-line row that opens to all of it in italics.
fn thought_row(message: usize, at: usize, text_: &str, open: bool) -> AnyElement {
    let mut column = view().flex_col().gap(scale::s1()).child(
        view()
            .role(AccessibilityRole::Button)
            .label(match thought_gist(text_) {
                gist if gist.is_empty() => "Thinking".to_string(),
                gist => format!("Thinking: {gist}"),
            })
            .flex_row()
            .items_center()
            .gap(scale::s2())
            .py(scale::s1())
            .px(scale::s3())
            .mx(-scale::s3())
            .cursor_pointer()
            .rounded(super::soft_radius())
            .smooth()
            .hover(|s| s.bg(Colors::hover()))
            .on_press(move || tell(Cmd::ToggleThought { message, block: at }))
            .child(lead_slot(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, STEP_ICON, Colors::muted())))
            .child(text("Thinking").flex_none().sized(text_sm()).font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
            .children((!open).then(|| {
                text(thought_gist(text_))
                    .number_of_lines(1)
                    .flex_1()
                    .min_w_0()
                    .sized(text_sm())
                    .text_color(Colors::muted())
                    .opacity(crate::theme::tokens::DIM)
            })),
    );
    if open {
        column = column.child(
            text_view()
                .markdown(text_.to_string())
                .on_link_press(open_link)
                .italic()
                .sized(text_sm())
                .px(scale::s3())
                .py(scale::s1())
                .pl(scale::s5())
                .opacity(crate::theme::tokens::DIM)
                .text_color(Colors::muted()),
        );
    }
    column.into_element()
}

/// Whether `block` is a step of the agent's work, which sits close to the steps beside it.
fn is_step(block: &Block) -> bool {
    matches!(block, Block::Tool(_) | Block::Run { .. } | Block::Thought { .. })
}

/// One call of the agent's tools: its glyph and title, what it sent and what came back once it
/// is open, and a way to the file it touched.
fn tool_row(message: usize, at: usize, tool: &Tool) -> AnyElement {
    let settled = tool.phase == Phase::Done;
    let title = tool.title.clone();
    let mut card = view()
        .flex_col()
        .gap(scale::s1())
        .py(scale::s1())
        .px(scale::s3())
        .mx(-scale::s3())
        .rounded(super::soft_radius())
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(scale::s2())
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(scale::s2())
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
                                .sized(text_sm())
                                .text_color(move || if settled { Colors::muted()() } else { Colors::foreground()() }),
                        ),
                )
                .children(tool.path.clone().map(|path| {
                    view()
                        .role(AccessibilityRole::Button)
                        .label("Open the file it changed")
                        .flex_none()
                        .px(scale::s2())
                        .py(scale::s0_5())
                        .rounded(Colors::radius())
                        .cursor_pointer()
                        .hover(|s| s.bg(Colors::hover()))
                        .on_press(move || tell(Cmd::Open(path.clone())))
                        .child(icon(IconName::FileText, STEP_ICON, Colors::muted()))
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
                    // Under the title, not under its glyph.
                    .ml(scale::s4() + scale::s2())
                    .border_l_1()
                    .border_color(super::hairline())
                    .pl(scale::s3())
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
fn run_row(message: usize, at: usize, count: usize, failed: usize, phase: Phase, folded: bool, task: Option<&str>) -> AnyElement {
    let calls = format!("{count} tool calls");
    // The task the calls work towards, when the plan names one; the count then follows it.
    let label = task.map_or_else(|| calls.clone(), str::to_string);
    // A failure the run got past is a quiet count after the label, not a failed run.
    let mut aside: Vec<String> = Vec::new();
    if task.is_some() {
        aside.push(calls);
    }
    if failed > 0 && phase != Phase::Failed {
        aside.push(format!("{failed} failed"));
    }
    let aside = (!aside.is_empty()).then(|| format!("· {}", aside.join(" · ")));
    // What is read out is all the row says.
    let spoken = aside.as_ref().map_or_else(|| label.clone(), |aside| format!("{label} {aside}"));
    view()
        .role(AccessibilityRole::Button)
        .label(spoken)
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .py(scale::s1())
        .px(scale::s3())
        .mx(-scale::s3())
        .rounded(super::soft_radius())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.bg(Colors::hover()))
        .on_press(move || tell(Cmd::Fold { message, block: at, folded: !folded }))
        .child(step_glyph(phase))
        .child(text(label).min_w_0().sized(text_sm()).text_color(Colors::muted()))
        .children(aside.map(|aside| text(aside).sized(text_sm()).text_color(Colors::muted())))
        .child(view().flex_1())
        .child(trail_slot(icon(if folded { IconName::ChevronRight } else { IconName::ChevronDown }, STEP_ICON, Colors::muted())))
        .into_element()
}

/// The plan: each entry with its phase; the ones done, quietly.
fn plan_rows(entries: &[Entry]) -> AnyElement {
    view()
        .flex_col()
        .gap(scale::s1_5())
        .py(scale::s1())
        .children(entries.iter().map(|entry| {
            let (entry_text, entry_phase) = (entry.text.clone(), entry.phase);
            view()
                .flex_row()
                .items_start()
                .gap(scale::s2())
                .child(
                    text_view()
                        .markdown(entry_text)
                        .on_link_press(open_link)
                        .flex_1()
                        .min_w_0()
                        .sized(text_sm())
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
        .gap(scale::s2())
        .py(scale::s3())
        .px(scale::s3())
        .rounded(super::card_radius())
        .border_1()
        .border_color(move || if waiting { Colors::warning()().opacity(crate::theme::tokens::ALERT_BORDER) } else { super::hairline()() })
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(scale::s2())
                .child(text("Permission").flex_none().sized(text_xs()).font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
                .child(view().flex_1().min_w_0().child(text_view().markdown(title).sized(text_sm()).text_color(Colors::foreground()))),
        );
    match answered {
        Some(given) => {
            let said = format!("→ {given}");
            card = card.child(text(said).sized(text_sm()).text_color(Colors::muted()));
        }
        None => {
            let mut row = view().flex_row().flex_wrap().gap(scale::s1_5());
            for option in options {
                let (way, label, primary) = (option.at, option.label.clone(), option.primary);
                let mut reply = view()
                    .role(AccessibilityRole::Button)
                    .label(label.clone())
                    .px(scale::s4())
                    .py(scale::s1())
                    .rounded(super::pill())
                    .cursor_pointer()
                    .smooth()
                    .on_press(move || tell(Cmd::Answer { message, block: at, at: way }));
                reply = if primary {
                    reply.bg(Colors::primary())
                } else {
                    reply.border_1().border_color(Colors::input_border()).hover(|s| s.bg(Colors::hover()))
                };
                row = row.child(
                    reply.child(text(label).sized(text_sm()).text_color(move || if primary { Colors::primary_foreground()() } else { Colors::foreground()() })),
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
        .gap(scale::s2())
        .py(scale::s3())
        .px(scale::s3())
        .rounded(super::card_radius())
        .border_1()
        .border_color(move || if waiting { Colors::warning()().opacity(crate::theme::tokens::ALERT_BORDER) } else { super::hairline()() })
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(scale::s2())
                .child(text("Approve").flex_none().sized(text_xs()).font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
                .child(view().flex_1().min_w_0().child(text_view().markdown(title).sized(text_sm()).text_color(Colors::foreground()))),
        )
        .children((!lines.is_empty()).then(|| {
            view()
                .flex_col()
                .max_h(px(224.))
                .overflow_hidden()
                .border_l_1()
                .border_color(super::hairline())
                .pl(scale::s3())
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
            card = card.child(text(said).sized(text_sm()).text_color(Colors::muted()));
        }
        None => {
            card = card.child(
                view()
                    .flex_row()
                    .flex_wrap()
                    .gap(scale::s1_5())
                    .child(
                        view()
                            .role(AccessibilityRole::Button)
                            .label("Run the command")
                            .px(scale::s3())
                            .py(scale::s1())
                            .rounded(Colors::radius())
                            .cursor_pointer()
                            .bg(Colors::primary())
                            .on_press(move || tell(Cmd::Approve { message, block: at, yes: true }))
                            .child(text("Run it").sized(text_sm()).text_color(Colors::primary_foreground())),
                    )
                    .child(
                        view()
                            .role(AccessibilityRole::Button)
                            .label("Leave the command out")
                            .px(scale::s3())
                            .py(scale::s1())
                            .rounded(Colors::radius())
                            .cursor_pointer()
                            .border_1()
                            .border_color(Colors::input_border())
                            .hover(|s| s.bg(Colors::hover()))
                            .on_press(move || tell(Cmd::Approve { message, block: at, yes: false }))
                            .child(text("Leave out").sized(text_sm()).text_color(Colors::foreground())),
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
            .on_link_press(open_link)
            .selectable(true)
            .sized(text_base())
            .text_color(Colors::foreground())
            .into_element(),
        Block::Thought { index, text: thought, open } => thought_row(message, *index, thought, *open),
        Block::Tool(tool) => tool_row(message, tool.index, tool),
        Block::Run { index, count, failed, phase, folded, task } => run_row(message, *index, *count, *failed, *phase, *folded, task.as_deref()),
        Block::Plan(entries) => plan_rows(entries),
        Block::Ask { index, title, options, answered } => ask_row(message, *index, title, options, answered),
        Block::Approve { index, question, lines, answered } => approve_row(message, *index, question, lines, answered),
    }
}

/// One message of the agent's: its blocks, a run's own row standing in for the tools it hides.
fn agent_row(message: usize, blocks: &[Block]) -> AnyElement {
    let mut column = view().flex_col().min_w_0();
    let mut skip = 0;
    let mut before: Option<&Block> = None;
    if blocks.is_empty() {
        column = column.child(text("Thinking…").sized(text_sm()).text_color(Colors::muted()));
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
        // Steps sit close to each other; everything else, and the line between a step and text,
        // has room.
        let gap = match before {
            None => Points::ZERO,
            Some(before) if is_step(before) && is_step(block) => scale::s0_5(),
            Some(_) => scale::s4(),
        };
        column = column.child(view().mt(gap).child(block_row(message, block)));
        before = Some(block);
    }
    column.into_element()
}

/// The conversation: the messages, in order, as a center column.
fn transcript(items: &[Item], slot: ElementRef, at_end: Signal<bool>) -> AnyElement {
    // The transcript's column is the composer card's width, and its text starts where the
    // composer's does: the card's border, its padding and the field's padding in (see [`bar`]).
    let mut rows = view().w_full().min_w_0().max_w(px(COLUMN)).flex_col().gap(scale::s6()).px(scale::composer_text_inset()).py(scale::s6());
    for (message, item) in items.iter().enumerate() {
        let row = match item {
            Item::User { text, pictures } => user_row(text, pictures),
            Item::Agent { blocks, .. } => agent_row(message, blocks),
        };
        rows = rows.child(row);
    }
    // The column re-wraps when the window is resized, and its height changes under a scroll
    // offset that stays put: a transcript that was at its end follows it to the new one.
    let size = stored((0., 0.));
    let resized = {
        move |bounds: &Bounds<Points>| {
            let now = (bounds.size.width.0, bounds.size.height.0);
            if size.replace(now) != now && at_end.get_untracked() {
                let slot = slot;
                spawn(async move {
                    slot.scroll_to_end(false);
                });
            }
        }
    };
    view()
        .flex_1()
        .min_h_0()
        .flex()
        .justify_center()
        .overflow_hidden()
        .child(
            scroll()
                .size_full()
                .on_scroll(move |event: &ScrollEvent| at_end.set(event.offset.y + event.size.height >= event.content_size.height - px(24.)))
                .child(view().flex_col().items_center().w_full().min_w_0().px(scale::s3()).on_layout(resized).child(rows.into_element()))
                .element_ref(&slot),
        )
        .into_element()
}

// ============ the composer ============

/// The pictures of the draft: their strip, each with a way to take it off.
fn pictures_row(pictures: &[Picture]) -> AnyElement {
    view()
        .flex_row()
        .flex_wrap()
        .items_start()
        .gap(scale::s2())
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
                                .border_color(super::hairline())
                                .max_h(px(72.))
                                .max_w(px(120.))
                                .overflow_hidden(),
                        )
                        .child(
                            view()
                                .role(AccessibilityRole::Button)
                                .label("Remove the picture")
                                .absolute()
                                .top(-scale::s1())
                                .right(-scale::s1())
                                .p(scale::s0_5())
                                .rounded(super::pill())
                                .bg(Colors::background())
                                .border_1()
                                .border_color(super::hairline())
                                .cursor_pointer()
                                .hover(|s| s.bg(Colors::hover()))
                                .on_press(move || tell(Cmd::Detach(at)))
                                .child(icon(IconName::Close, scale::s2_5(), Colors::muted())),
                        )
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// What a queued message shows on its one row: its text with the line breaks folded into
/// spaces, then how many pictures go with it; the pictures alone when it has no text; nothing when
/// it has neither.
fn queue_preview(text: &str, pictures: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let word = match pictures {
        0 => String::new(),
        1 => "1 picture".to_string(),
        n => format!("{n} pictures"),
    };
    match (line.is_empty(), word.is_empty()) {
        (_, true) => line,
        (true, false) => word,
        (false, false) => format!("{line} · +{word}"),
    }
}

/// One message waiting to be sent when the turn is over: its kind, what it says, and the
/// take-back button. The rows share the tray that sits on the composer (see [`bar`]).
fn queue_row(queued: Queued) -> AnyElement {
    let kind = if queued.kind.is_empty() { "Queued".to_string() } else { queued.kind.clone() };
    let preview = queue_preview(&queued.text, queued.pictures);
    // Said of the take-back button, so the buttons of several rows tell apart.
    let spoken = format!("Take back into the draft: {}", preview.chars().take(40).collect::<String>());
    // Text in the foreground; pictures alone, muted.
    let message = text(preview).number_of_lines(1).flex_1().min_w_0().sized(text_sm());
    let message = if queued.text.split_whitespace().next().is_some() { message.text_color(Colors::foreground()) } else { message.text_color(Colors::muted()) };
    view()
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .px(scale::s3())
        .py(scale::s1_5())
        // At least this wide, so the messages of the rows start at the same place; a longer kind
        // widens it rather than wrapping.
        .child(text(kind).min_w(px(56.)).flex_none().number_of_lines(1).sized(text_xs()).font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
        // One line, whatever the message: the end is cut.
        .child(message)
        .child(
            view()
                .role(AccessibilityRole::Button)
                .label(spoken)
                .flex_none()
                .p(scale::s0_5())
                .rounded(super::pill())
                .cursor_pointer()
                .hover(|s| s.bg(Colors::hover()))
                .on_press(move || tell(Cmd::Recall(queued.at)))
                .child(icon(IconName::Pencil, scale::s2_5(), Colors::muted())),
        )
        .into_element()
}

/// Why some of what was pasted or dropped was left out, with a way to dismiss it.
fn note_row(note: &str) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .child(text(note.to_string()).flex_1().min_w_0().sized(text_xs()).text_color(Colors::muted()))
        .child(
            view()
                .role(AccessibilityRole::Button)
                .label("Dismiss")
                .flex_none()
                .p(scale::s0_5())
                .rounded(super::pill())
                .cursor_pointer()
                .hover(|s| s.bg(Colors::hover()))
                .on_press(|| tell(Cmd::DismissNote))
                .child(icon(IconName::Close, scale::s2_5(), Colors::muted())),
        )
        .into_element()
}

/// What the picker has to say while it has no sessions to list, such as that the agent is waking
/// or the session loading: one quiet line with a dot, not a card with an empty body.
fn picker_note(note: &str) -> AnyElement {
    let shown = super::sentence(note);
    view()
        .flex_row()
        .items_center()
        .gap(scale::s2())
        .mb(scale::s2_5())
        .pl(scale::s3())
        .pr(scale::s1_5())
        .py(scale::s1())
        .rounded(super::soft_radius())
        .bg(Colors::hover())
        .child(view().flex_none().w(scale::s1_5()).h(scale::s1_5()).rounded(super::pill()).bg(Colors::primary()))
        .child(text(shown).flex_1().min_w_0().sized(text_sm()).text_color(Colors::muted()))
        .child(
            view()
                .role(AccessibilityRole::Button)
                .label("Close")
                .flex_none()
                .p(scale::s0_5())
                .rounded(super::pill())
                .cursor_pointer()
                .smooth()
                .hover(|s| s.bg(Colors::selected()))
                .on_press(|| tell(Cmd::ClosePicker))
                .child(icon(IconName::Close, scale::s2_5(), Colors::muted())),
        )
        .into_element()
}

/// The sessions to resume, above the composer while the picker is open.
fn picker(picking: &Picking) -> AnyElement {
    match picking {
        Picking::Note(note) => return picker_note(note),
        Picking::Loading => return view().into_element(),
        Picking::Sessions(_) => {}
    }
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
                        .gap(scale::s3())
                        .px(scale::s3())
                        .py(scale::s1_5())
                        .rounded(Colors::radius())
                        .cursor_pointer()
                        .hover(|s| s.bg(Colors::hover()))
                        .on_press(move || tell(Cmd::Resume(id.clone())))
                        .child(text(label).flex_1().min_w_0().number_of_lines(1).sized(text_sm()).text_color(Colors::foreground()))
                        .child(text(when).flex_none().sized(text_xs()).text_color(Colors::muted())),
                );
            }
            column.into_element()
        }),
        Picking::Note(_) | Picking::Loading => unreachable!("drawn above"),
    };
    view()
        .flex_col()
        .mb(scale::s2_5())
        .py(scale::s1_5())
        .rounded(super::card_radius())
        .border_1()
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        .overflow_hidden()
        .child(
            view()
                .flex_row()
                .items_center()
                .px(scale::s1_5())
                .pb(scale::s1())
                .child(text(title).flex_1().sized(text_xs()).font_weight(FontWeight::MEDIUM).text_color(Colors::muted()))
                .child(
                    view()
                        .role(AccessibilityRole::Button)
                        .label("Close")
                        .flex_none()
                        .p(scale::s0_5())
                        .rounded(super::pill())
                        .cursor_pointer()
                        .hover(|s| s.bg(Colors::hover()))
                        .on_press(|| tell(Cmd::ClosePicker))
                        .child(icon(IconName::Close, scale::s2_5(), Colors::muted())),
                ),
        )
        .child(scroll().max_h(px(320.)).child(rows))
        .into_element()
}

/// The icon and name inside the worktree chip, in `tone`.
fn worktree_label(tone: impl Fn() -> AdaptiveColor + Copy + 'static) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(scale::s1())
        .child(icon(IconName::GitBranch, scale::s3(), tone))
        .child(text("Worktree").font_weight(FontWeight::MEDIUM).text_size(text_sm()).text_color(tone))
        .into_element()
}

/// What a thread waits on before it starts: the agent to run it, and a chip that, when on, starts
/// it in a git worktree of its own. Both sit in the row of chips under the field.
fn setup(snap: &Snapshot) -> AnyElement {
    let mut row = view().flex_row().flex_wrap().items_center().gap(scale::s2());
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
    row = row.child(agents.into_element());
    if let Some(isolate) = snap.isolate {
        let tip = "Start the thread in a git worktree of its own";
        row = row.child(
            view()
                .role(AccessibilityRole::Button)
                .label(tip)
                .flex_none()
                .flex_row()
                .items_center()
                .gap(scale::s1())
                .px(scale::s2())
                .py(scale::s1())
                .rounded(super::pill())
                .border_1()
                .border_color(Colors::input_border())
                .cursor_pointer()
                .style_when(move || isolate, |s| s.bg(Colors::hover()).border_color(Colors::primary()))
                .hover(|s| s.bg(Colors::hover()))
                .tooltip(tooltip(tip))
                .on_press(move || tell(Cmd::Isolate(!isolate)))
                .child(if isolate { worktree_label(Colors::foreground()) } else { worktree_label(Colors::muted()) }),
        );
    }
    row.into_element()
}

/// The settings of the session, as menus named for each.
/// A menu for each of the session's settings. While a turn runs they wait: agents differ on
/// changing them mid-turn.
fn setting_chips(settings: &[Setting], busy: bool) -> Vec<AnyElement> {
    settings
        .iter()
        .map(|setting| {
            if busy {
                return menu_trigger(setting.current_name(), "Available when the agent is idle").opacity(crate::theme::tokens::DISABLED).into_element();
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
        .collect()
}

/// Sends the draft, as `later`.
fn send_row(draft: &Signal<String>, state: &Signal<Snapshot>, later: bool) {
    let empty = draft.get_untracked().trim().is_empty();
    if empty && state.get_untracked().pictures.is_empty() {
        return;
    }
    tell(Cmd::Send { later });
}

/// While a turn runs, what is typed is sent into it (steered) or queued for its end: a small
/// round icon beside Stop.
fn steer_button(steer: bool, draft: Signal<String>, has_pictures: bool) -> AnyElement {
    let (name, tip) = if steer {
        (IconName::CornerDownRight, "Steer: the agent reads it before its next step (Alt+Enter: after this turn)")
    } else {
        (IconName::ListPlus, "Queue: sent when this turn is over")
    };
    let typed = draft;
    view()
        .role(AccessibilityRole::Button)
        .label(tip)
        .flex_none()
        .flex_row()
        .items_center()
        .justify_center()
        .w(scale::s7())
        .h(scale::s7())
        .rounded(super::pill())
        .border_1()
        .border_color(Colors::input_border())
        .cursor_pointer()
        .style_when(move || typed.get().trim().is_empty() && !has_pictures, |s| s.opacity(crate::theme::tokens::DISABLED))
        .hover(|s| s.bg(Colors::hover()))
        .tooltip(tooltip(tip))
        .on_press(move || {
            let text = draft.get_untracked();
            if !text.trim().is_empty() || has_pictures {
                tell(Cmd::Draft(text));
                tell(Cmd::Send { later: false });
            }
        })
        .child(icon(name, scale::s3_5(), Colors::foreground()))
        .into_element()
}

/// The send button: a small round icon that says what it does now (start, send, or stop the
/// turn), and does it.
fn send_button(snap: Snapshot, draft: Signal<String>) -> AnyElement {
    let (name, tip) = if snap.blank {
        (IconName::ArrowUp, "Start the thread")
    } else if snap.busy {
        (IconName::Square, "Stop the agent")
    } else {
        (IconName::ArrowUp, "Send now")
    };
    let busy = snap.busy;
    // Nothing to send while the agent is idle and the draft is blank.
    let empty = {
        move || !busy && draft.get().trim().is_empty()
    };
    let empty2 = empty;
    view()
        .role(AccessibilityRole::Button)
        .label(tip)
        .flex_none()
        .flex_row()
        .items_center()
        .justify_center()
        .w(scale::s8())
        .h(scale::s8())
        .rounded(super::pill())
        .cursor_pointer()
        .smooth()
        .hover(|s| s.opacity(crate::theme::tokens::HOVER))
        // Nothing to send: a neutral disc. Something to send: the accent.
        .bg(move || if empty() { Colors::selected()() } else { Colors::primary()() })
        .tooltip(tooltip(tip))
        .on_press(move || {
            if busy {
                tell(Cmd::Stop);
            } else {
                pressed_send(&draft);
            }
        })
        .child(icon(name, scale::s3_5(), move || if empty2() { Colors::muted()() } else { Colors::primary_foreground()() }))
        .into_element()
}

/// The field the draft is typed in, reporting what the user pasted:
/// the pictures and files it came with; the text it says is inserted by the field itself.
fn draft_field(draft: &Signal<String>, field_ref: ElementRef) -> Input {
    let held = *draft;
    input()
        .test_id("chat-draft")
        .element_ref(&field_ref)
        .flex_1()
        .min_w_0()
        .max_h(px(160.))
        .sized(text_base())
        .multiline(true)
        .value(move || held.get())
        .text_color(Colors::foreground())
        .selection_color(Colors::one(Colors::caret()))
        .auto_correct(false)
        .placeholder("Message the agent")
        .placeholder_color(Colors::one(Colors::muted()))
        .submit_behavior(nsh_core::element::SubmitBehavior::Submit)
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

/// The field in its row. A value the app writes to a field the user has typed in is dropped as
/// stale, so each reset of the draft (a send, a taken-back queue row) builds the field again, from
/// the draft it now holds: the row is one level deeper on every other reset, which makes it a new
/// view rather than the one the user typed in.
fn fresh_field(reset: u64, field: Input) -> AnyElement {
    let row = view().px(scale::s2()).py(scale::s1()).flex_row().child(field);
    if reset.is_multiple_of(2) {
        row.into_element()
    } else {
        view().flex_col().child(row).into_element()
    }
}

/// The composer: the note, the strip of pictures, the setup of a thread that waits to start,
/// the field, and the send.
fn bar(snap: Snapshot, draft: Signal<String>, field_ref: ElementRef) -> AnyElement {
    let field = draft;
    let low = draft;
    let has_pictures = !snap.pictures.is_empty();
    // The chips under the field: before the thread starts its agent is chosen here, after it the
    // agent is named; the settings and the Control menu follow, and the action ends the row.
    let mut chips = view().flex_row().flex_wrap().items_center().gap(scale::s1()).flex_1().min_w_0();
    if snap.blank {
        chips = chips.child(setup(&snap));
    } else {
        let name = snap
            .chosen
            .as_ref()
            .map(|id| snap.agents.iter().find(|(_, key)| key == id).map_or(id.clone(), |(name, _)| name.clone()))
            .unwrap_or_else(|| "Agent".to_string());
        chips = chips.child(text(name).px(scale::s3()).sized(text_sm()).text_color(Colors::dim(Colors::muted())));
    }
    if !snap.settings.is_empty() {
        chips = chips.children(setting_chips(&snap.settings, snap.busy));
    }
    chips = chips.child(grant_menu(snap.grant.unwrap_or_default()));
    // One card: the field, the chips and the send. The inset of its text is
    // `scale::composer_text_inset`.
    let queued = !snap.queue.is_empty();
    let core = view()
        .flex_col()
        .gap(scale::s2())
        .w_full()
        .max_w(px(COLUMN))
        .p(scale::s2())
        .rounded_b(super::surface_radius())
        // Under a queue tray the top edge is flat, so the two read as one piece.
        .rounded_t(if queued { Points(0.) } else { super::surface_radius() })
        .border_1()
        // The input's own border, firmer than the hairlines of the page, so the card reads as
        // the place to type.
        .border_color(Colors::input_border())
        .bg(Colors::input_background())
        // A tight, crisp lift: a wide blur reads as a wash, not an edge, so the shadow is a
        // one-point drop with next to no blur, in a color that adapts to the theme.
        .shadow_color(Colors::shadow())
        .shadow_opacity(1.)
        .shadow_radius(px(0.))
        .shadow_offset(point(px(0.), px(1.)))
        .on_file_drop(move |drop: &FileDropEvent| tell(Cmd::Drop(drop.uris.clone())))
        .children(snap.note.as_deref().map(note_row))
        .children((!snap.pictures.is_empty()).then(|| pictures_row(&snap.pictures)))
        .child(fresh_field(snap.draft_reset, draft_field(&field, field_ref)))
        .child(
            view()
                .flex_row()
                .items_end()
                .justify_between()
                .gap(scale::s2())
                .child(chips)
                .child(view().flex_row().flex_none().gap(scale::s2()).children(snap.busy.then(|| steer_button(snap.steer, low, has_pictures))).child(send_button(snap.clone(), low))),
        );
    let mut column = view().flex_none().flex_col().items_center().gap(scale::s1_5()).px(scale::s3()).pt(scale::s2()).pb(scale::s3());
    if let Some(picking) = snap.picking.as_ref().filter(|picking| **picking != Picking::Loading) {
        column = column.child(view().w_full().max_w(px(COLUMN)).child(picker(picking)));
    }
    if queued {
        // The waiting messages sit on the composer as a tray: its sides and top carry the
        // composer's border, its bottom is the composer's top edge.
        let tray = view()
            .w_full()
            .max_w(px(COLUMN))
            .flex_col()
            .gap(scale::s0_5())
            .py(scale::s1())
            .rounded_t(super::surface_radius())
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(Colors::input_border())
            .bg(Colors::selected())
            .child(snap.queue.iter().cloned().map(queue_row).collect::<Vec<_>>());
        column = column.child(view().w_full().max_w(px(COLUMN)).flex_col().child(tray).child(core));
    } else {
        column = column.child(core);
    }
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
    // The transcript's links show the pointer rather than the text cursor of the selectable text.
    provide_context(nsh_base::TextViewParts::new().part(nsh_base::TextViewPart::Link, |style| style.cursor_pointer()));
    let state = signal(SNAPSHOT.borrow().clone());
    spawn(async move {
        let mut snapshot = SNAPSHOT.subscribe();
        // What was published before this ran would otherwise never arrive.
        snapshot.mark_changed();
        while snapshot.changed().await.is_ok() {
            state.set(snapshot.borrow_and_update().clone());
        }
    });

    // What the draft field holds: the user's typing, taken over by the app's own writes (a
    // cleared draft after a send, a taken-back queue row), which the sentinel marks.
    // The field is built again then (see [`fresh_field`]), so it takes the keyboard back.
    let field_ref = element_ref();
    let draft = signal(state.with_untracked(|s| s.draft.clone()));
    let taken = stored(u64::MAX);
    effect(move || {
        let (sentinel, text) = state.with(|s| (s.draft_reset, s.draft.clone()));
        if taken.replace(sentinel) != sentinel {
            if text != draft.get_untracked() {
                draft.set(text);
            }
            spawn(async move { field_ref.focus() });
        }
    });

    // The field takes the keyboard whenever the app asks it to.
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
    let seen_soft = stored(u64::MAX);
    let at_end = signal(true);
    effect(move || {
        let (bump, soft) = state.with(|s| (s.follow, s.follow_soft));
        let grown = seen_soft.replace(soft) != soft && at_end.get_untracked();
        if (bump > 0 && seen.replace(bump) != bump) || grown {
            // After the rows are laid out: scrolling now would stop short of the new end, and
            // leave the last lines below the edge of the transcript.
            spawn(async move {
                scrolled.scroll_to_end(false);
            });
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
                .gap(scale::s3())
                .px(scale::s4())
                .children([text("What is this thread about?").sized(text_base()).text_color(Colors::muted()), text("Message an agent to begin.").sized(text_sm()).text_color(Colors::muted())])
                .into_element()
        } else {
            transcript(&snap.items, scrolled, at_end)
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

    #[test]
    fn a_thought_folds_to_its_first_line_of_prose() {
        assert_eq!(thought_gist("**Plan** the work\nthen do it"), "Plan the work");
        assert_eq!(thought_gist("```rust\nlet x = 1;\n```"), "let x = 1;");
        assert_eq!(thought_gist("1. Read the file"), "Read the file");
        assert_eq!(thought_gist("\n\n"), "");
    }
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
        app.press("label:Take back into the draft: then run the tests").unwrap();
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

    #[test]
    fn a_queued_message_previews_on_one_line_with_its_pictures() {
        assert_eq!(queue_preview("first\n\n  second\tthird", 0), "first second third");
        assert_eq!(queue_preview("", 2), "2 pictures");
        assert_eq!(queue_preview(" \n ", 1), "1 picture");
        assert_eq!(queue_preview("look at this", 3), "look at this · +3 pictures");
        assert_eq!(queue_preview("  ", 0), "");
    }

    #[test]
    fn each_queued_row_takes_back_its_own_message() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let queue = vec![
            Queued { at: 0, kind: "Steer".into(), text: "use the cache".into(), pictures: 0 },
            Queued { at: 1, kind: String::new(), text: "and\nupdate the docs".into(), pictures: 0 },
        ];
        let mut app = start(Snapshot { title: "thread".into(), busy: true, queue, ..Snapshot::default() });
        app.press("label:Take back into the draft: and update the docs").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::Recall(1))]);
        app.press("label:Take back into the draft: use the cache").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::Recall(0))]);
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

    #[test]
    fn a_run_with_a_task_is_named_by_it_and_says_its_calls_and_failures() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let run = |task: Option<&str>, failed| {
            let blocks = vec![Block::Run { index: 0, count: 4, failed, phase: Phase::Done, folded: true, task: task.map(str::to_string) }];
            start(Snapshot { items: vec![Item::Agent { index: 0, blocks }], ..Snapshot::default() })
        };
        // The row's spoken name is all it says: the task, then the calls and the failures.
        let mut app = run(Some("Fix the parser"), 1);
        app.press("label:Fix the parser · 4 tool calls · 1 failed").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::Fold { message: 0, block: 0, folded: false })]);
        let mut app = run(None, 0);
        app.press("label:4 tool calls").unwrap();
        assert_eq!(drain(&mut app, commands), [Bus::Chat(Cmd::Fold { message: 0, block: 0, folded: false })]);
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
        let mut blocks = vec![Block::Run { index: 0, count: 4, failed: 0, phase: Phase::Done, folded: false, task: None }];
        blocks.extend((0..4).map(tool));
        blocks.push(Block::Ask { index: 4, title: "Run rm?".into(), options: vec![AskOption { at: 0, label: "Allow once".into(), primary: true }], answered: None });
        blocks.push(Block::Run { index: 5, count: 3, failed: 0, phase: Phase::Done, folded: false, task: None });
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

    #[test]
    fn a_started_thread_names_its_agent_and_offers_control_before_any_grant() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut app = start(Snapshot {
            agents: vec![("Claude".into(), "claude".into())],
            chosen: Some("claude".into()),
            grant: Some(Grant::default()),
            items: vec![Item::Agent { index: 0, blocks: vec![] }],
            ..Snapshot::default()
        });
        assert!(app.shows("Claude"), "{:?}", app.screen().texts());
        let texts = app.screen().texts();
        assert_eq!(texts.iter().filter(|t| **t == "Control").count(), 1, "only the composer's menu, not the header's: {texts:?}");
        app.press("label:What this thread's agent may do through Po").unwrap();
        app.settle();
        app.press("text:Orchestrate").unwrap();
        let sent = drain(&mut app, commands);
        let set = Grant { scope: Scope::Orchestrate, ..Grant::default() };
        assert!(sent.contains(&Bus::Chat(Cmd::Grant(GrantChange::Set(set)))), "{sent:?}");
    }

    #[test]
    fn a_new_thread_offers_control_before_it_starts() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut app = start(Snapshot {
            blank: true,
            agents: vec![("Claude".into(), "claude".into())],
            chosen: Some("claude".into()),
            grant: Some(Grant::default()),
            ..Snapshot::default()
        });
        assert!(app.shows("Control"), "{:?}", app.screen().texts());
        app.press("label:What this thread's agent may do through Po").unwrap();
        app.settle();
        app.press("text:Orchestrate").unwrap();
        let sent = drain(&mut app, commands);
        let set = Grant { scope: Scope::Orchestrate, ..Grant::default() };
        assert!(sent.contains(&Bus::Chat(Cmd::Grant(GrantChange::Set(set)))), "{sent:?}");
    }

    #[test]
    fn the_field_empties_when_the_app_clears_the_draft() {
        let mut guard = crate::nsh::bus::lock();
        let commands = guard.as_mut().expect("the channel");
        let mut app = start(Snapshot { items: vec![Item::Agent { index: 0, blocks: vec![] }], ..Snapshot::default() });
        app.type_text("id:chat-draft", "hello there").unwrap();
        drain(&mut app, commands);
        assert_eq!(app.screen().input_text("id:chat-draft").unwrap(), Some("hello there"));
        publish(Snapshot { draft: String::new(), draft_reset: 1, items: vec![Item::Agent { index: 0, blocks: vec![] }], ..Snapshot::default() });
        app.wait(std::time::Duration::from_millis(100)).unwrap();
        assert_eq!(app.screen().input_text("id:chat-draft").unwrap().unwrap_or(""), "");
    }
}
