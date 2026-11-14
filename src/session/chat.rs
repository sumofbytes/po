use crate::session::thread::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agent::acp::PermissionOption;
use crate::agent::attach::Attachment;
use crate::{config, model};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Step {
    Waiting,
    Working,
    Done,
    Failed,
}

#[derive(Clone)]
pub(crate) struct ToolBlock {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: Step,
    /// What the agent calls it (`read`, `edit`, `execute`…), when it says.
    pub(crate) kind: String,
    /// When the call began, and when it settled (done or failed); a stretch of work says how long it took by these.
    pub(crate) started: Instant,
    pub(crate) ended: Option<Instant>,
    pub(crate) open: bool,
    /// On the first call of a run of [`MIN_RUN`] or more: whether the run is folded to one line. `None`
    /// leaves it to `run_folded`'s default, until the user clicks the run's header.
    pub(crate) fold: Option<bool>,
    /// The plan entry that was in progress when the call began: what the call is working towards.
    pub(crate) task: Option<String>,
    pub(crate) arguments: String,
    pub(crate) result: String,
    pub(crate) path: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Ask {
    pub(crate) request: Value,
    pub(crate) title: String,
    pub(crate) options: Vec<PermissionOption>,
    pub(crate) answered: Option<String>,
}

#[derive(Clone)]
pub(crate) enum Block {
    Text(String),
    /// The agent's reasoning, and whether it is shown; it starts folded.
    Thought(String, bool),
    Tool(ToolBlock),
    Permission(Ask),
    /// Po wants to run an agent command that isn't one of its own, and waits to be told it may.
    /// `new_agent` is whether the agent's own command is the question; `mcp` are the config's MCP
    /// servers it would be handed.
    Approve {
        agent: config::Agent,
        new_agent: bool,
        mcp: Vec<config::McpServer>,
        resume: bool,
        answered: Option<bool>,
    },
    /// The agent's to-do list; each update replaces the whole list.
    Plan(Vec<(String, Step)>),
}

/// Where text headed for an agent came from, so that it is labelled as coming from there at
/// every step: sent at once, waiting for the agent, queued, or taken back. It never ends up in
/// the user's draft, and it never claims "the user has `x` open" when the user didn't send it.
/// The thread and Po origins come from `thread.send` and notify-parent.
#[allow(dead_code)]
pub(crate) enum Origin {
    /// The user's draft.
    User,
    /// Another thread of this window, sending through the API (`thread.send`).
    Thread { id: u64, title: String },
    /// Po itself, telling an orchestrator what its child did (notify-parent).
    Po,
}

#[derive(Clone)]
pub(crate) enum Msg {
    User(String, Vec<Arc<Attachment>>),
    Agent(Vec<Block>),
}

impl From<crate::agent::handoff::Said> for Msg {
    fn from(said: crate::agent::handoff::Said) -> Self {
        match said {
            crate::agent::handoff::Said::User(text) => Msg::User(text, vec![]),
            crate::agent::handoff::Said::Agent(text) => Msg::Agent(vec![Block::Text(text)]),
        }
    }
}

/// The file a tool call names: its locations, a diff, or a path in its input.
pub(crate) fn touched(update: &Value) -> Option<String> {
    let input = &update["rawInput"];
    update["locations"][0]["path"]
        .as_str()
        .or_else(|| {
            update["content"]
                .as_array()?
                .iter()
                .find_map(|item| item["path"].as_str())
        })
        .or_else(|| {
            input
                .as_str()
                .filter(|text| !text.contains(char::is_whitespace))
        })
        .or_else(|| {
            ["path", "file_path", "filePath"]
                .iter()
                .find_map(|key| input[key].as_str())
        })
        .map(String::from)
}

pub(crate) fn step(status: &str) -> Step {
    match status {
        "in_progress" => Step::Working,
        "completed" => Step::Done,
        "failed" => Step::Failed,
        _ => Step::Waiting,
    }
}

pub(crate) fn pretty(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The status of a thing that runs, as the read model says it.
pub(crate) fn step_name(status: Step) -> &'static str {
    match status {
        Step::Waiting => "waiting",
        Step::Working => "working",
        Step::Done => "done",
        Step::Failed => "failed",
    }
}

/// A thread's transcript as entries: the user's text, and the agent's text, thoughts, tool
/// calls and plan. Permission asks show as a call that is waiting or done; approval cards are
/// the user's to answer and are not in the transcript.
#[hotpath::measure]
pub(crate) fn transcript(thread: &Thread, full: bool) -> Vec<crate::session::state::Entry> {
    let mut out = Vec::new();
    for msg in &thread.messages {
        match msg {
            Msg::User(text, _) => out.push(crate::session::state::Entry {
                index: out.len(),
                role: "user",
                text: text.clone(),
                tools: vec![],
                plan: None,
            }),
            Msg::Agent(blocks) => {
                let mut said = Vec::new();
                let mut tools = Vec::new();
                let mut plan = None;
                for block in blocks {
                    match block {
                        Block::Text(text) | Block::Thought(text, _) => said.push(text.clone()),
                        Block::Tool(tool) => tools.push(crate::session::state::ToolCall {
                            title: tool.title.clone(),
                            status: step_name(tool.status).into(),
                            arguments: full.then(|| tool.arguments.clone()),
                            result: full.then(|| tool.result.clone()),
                        }),
                        Block::Plan(items) => {
                            plan = Some(
                                items
                                    .iter()
                                    .map(|(text, step)| (text.clone(), step_name(*step).into()))
                                    .collect(),
                            )
                        }
                        Block::Permission(ask) => tools.push(crate::session::state::ToolCall {
                            title: ask.title.clone(),
                            status: if ask.answered.is_none() {
                                "waiting"
                            } else {
                                "done"
                            }
                            .into(),
                            arguments: None,
                            result: None,
                        }),
                        // The user's click answers this; it is told of by the state.
                        Block::Approve { .. } => {}
                    }
                }
                out.push(crate::session::state::Entry {
                    index: out.len(),
                    role: "agent",
                    text: said.join("\n\n"),
                    tools,
                    plan,
                });
            }
        }
    }
    out
}

/// A diff content block as `-`/`+` lines, without the lines the edit leaves alone.
#[hotpath::measure]
pub(crate) fn diff_text(item: &Value) -> String {
    const LIMIT: usize = 200;
    let old: Vec<&str> = item["oldText"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .collect();
    let new: Vec<&str> = item["newText"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .collect();
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let removed = &old[head..old.len() - tail];
    let added = &new[head..new.len() - tail];
    let mut out = vec![format!("edit {}", item["path"].as_str().unwrap_or("?"))];
    out.extend(
        removed
            .iter()
            .chain(added)
            .enumerate()
            .take(LIMIT)
            .map(|(i, line)| format!("{} {line}", if i < removed.len() { '-' } else { '+' })),
    );
    if removed.len() + added.len() > LIMIT {
        out.push(format!(
            "… {} more lines",
            removed.len() + added.len() - LIMIT
        ));
    }
    out.join("\n")
}

/// A prompt as the user typed it, without the note `send` puts in front about the open file.
pub(crate) fn without_context(text: &str) -> &str {
    text.strip_prefix("[The user has `")
        .and_then(|rest| rest.split_once("]\n\n"))
        .map_or(text, |(_, prompt)| prompt)
}

/// The text a tool call carries: content blocks, diffs, then its raw output.
pub(crate) fn tool_result(update: &Value) -> String {
    let mut out = Vec::new();
    for item in update["content"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("content") => {
                if let Some(text) = item["content"]["text"].as_str() {
                    out.push(text.to_string());
                }
            }
            Some("diff") => out.push(diff_text(item)),
            Some("terminal") => out.push("terminal output".into()),
            _ => {}
        }
    }
    if out.is_empty() {
        return pretty(&update["rawOutput"]);
    }
    out.join("\n")
}

/// The fewest tool calls in a row that fold to one line.
pub(crate) const MIN_RUN: usize = 2;

/// How long a piece of the agent's text gets before the next paragraph ends it: a message that
/// streams past this is split there, so the piece being written stays small and the pieces that
/// came before it are not read again. A paragraph boundary is only taken outside a code fence.
pub(crate) const PIECE: usize = 512;

/// The pieces of the agent's text for the rows drawn: each is a run of paragraphs of [`PIECE`]
/// bytes or more, split at a paragraph break outside a code fence. So while a reply streams, the
/// piece being written stays small, and the pieces before it are not read again. The pieces to
/// gather are ranges of `text`, each as the agent's text is at a byte or the same.
pub(crate) fn pieces_of(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    // A link reference may be defined in a later piece than the one that uses it.
    if lines.iter().any(|line| reference_definition(line)) {
        return vec![text];
    }
    let mut pieces = Vec::new();
    let mut from = 0;
    let mut at = 0;
    let mut fence: Option<(char, usize)> = None;
    for (ix, line) in lines.iter().enumerate() {
        at += line.len();
        // A blank line ends a paragraph, and a fenced block line opens or shuts a fence, which a
        // piece may not sit inside (its two halves would each miss their other). The piece to
        // stand for one that ended before [`PIECE`] bytes was already pushed, so the boundary is
        // taken once what was gathered since the piece's start passes [`PIECE`]. A list or a
        // quote goes on past a blank line, so what comes after it must open a block of its own.
        if fence.is_none()
            && line.trim().is_empty()
            && at - from >= PIECE
            && lines
                .get(ix + 1)
                .is_some_and(|next| next.ends_with('\n') && starts_block(next))
        {
            pieces.push(&text[from..at]);
            from = at;
        }
        fence = fence_after(fence, line);
    }
    if from < at {
        pieces.push(&text[from..]);
    }
    pieces
}

/// The fence the text is inside once the line has been read: one opens with three or more
/// backticks or tildes and shuts with at least as many of the same, and nothing else.
pub(crate) fn fence_after(fence: Option<(char, usize)>, line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = line[indent..].trim_end();
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'));
    let Some(mark) = mark.filter(|_| indent < 4) else {
        return fence;
    };
    let run = rest.chars().take_while(|c| *c == mark).count();
    match fence {
        Some((open, len)) if mark == open && run >= len && rest.len() == run => None,
        Some(_) => fence,
        None if run >= 3 => Some((mark, run)),
        None => None,
    }
}

/// Whether the line opens a block that stands alone, so a piece may begin with it: not an indented
/// line, a list item, a quote or a blank one, which carry on whatever came before.
pub(crate) fn starts_block(line: &str) -> bool {
    let first = line.chars().next();
    if first.is_none_or(|c| c.is_whitespace() || c == '>') {
        return false;
    }
    let item = line.trim_start_matches(|c: char| c.is_ascii_digit());
    let numbered = item.len() < line.len() && (item.starts_with(". ") || item.starts_with(") "));
    let bullet = ["- ", "* ", "+ "].iter().any(|b| line.starts_with(b));
    !(numbered || bullet)
}

/// Whether the line defines a link reference (`[name]: url`).
pub(crate) fn reference_definition(line: &str) -> bool {
    let line = line.trim_start_matches(' ');
    line.strip_prefix('[').is_some_and(|rest| {
        rest.find("]:")
            .is_some_and(|end| !rest[..end].contains(']'))
    })
}

/// A step of the agent's work, as against what it says to the user or asks of them: a tool call
/// or a thought. Steps between two other blocks fold to one line.
pub(crate) fn is_step(block: &Block) -> bool {
    matches!(block, Block::Tool(_) | Block::Thought(..))
}

/// How many steps run on from `at`, and whether the run is one that folds: [`MIN_RUN`] or more
/// of them, a tool call among them (that is where the fold is kept).
pub(crate) fn step_run(blocks: &[Block], at: usize) -> (usize, bool) {
    let len = blocks[at..].iter().take_while(|b| is_step(b)).count();
    let foldable = len >= MIN_RUN
        && blocks[at..at + len]
            .iter()
            .any(|b| matches!(b, Block::Tool(_)));
    (len, foldable)
}

/// The tool call of the run that starts at `at` which keeps whether the run is folded.
pub(crate) fn fold_keeper(blocks: &mut [Block], at: usize) -> Option<&mut ToolBlock> {
    let len = blocks.get(at..)?.iter().take_while(|b| is_step(b)).count();
    blocks[at..at + len].iter_mut().find_map(|b| {
        if let Block::Tool(tool) = b {
            Some(tool)
        } else {
            None
        }
    })
}

/// Whether a run of steps shows as one line. A run the user hasn't clicked starts folded,
/// whether its calls are still going or not, and opens only if it ended in failure: `open` says
/// it has not ended (see [`run_open`]), so a failure the next call may get past moves no rows.
pub(crate) fn run_folded(run: &[Block], open: bool) -> bool {
    let Some(first) = run.iter().find_map(|b| {
        if let Block::Tool(tool) = b {
            Some(tool)
        } else {
            None
        }
    }) else {
        return false;
    };
    first
        .fold
        .unwrap_or(open || run_outcome(run).0 != Step::Failed)
}

/// Whether the run of `len` steps at `at` is still going, though no call of it may be: `live` says
/// the agent is at work on this message, and nothing has come after the run. Between two calls
/// the agent is thinking of the next one, which is no end of the work.
pub(crate) fn run_open(blocks: &[Block], at: usize, len: usize, live: bool) -> bool {
    live && at + len == blocks.len()
}

/// How a run of tool calls went, and how many of its calls failed. A failure the later calls got
/// past is only counted; the run is failed when none of them went well or the last one failed.
pub(crate) fn run_outcome(run: &[Block]) -> (Step, usize) {
    let steps: Vec<Step> = run
        .iter()
        .filter_map(|b| {
            if let Block::Tool(t) = b {
                Some(t.status)
            } else {
                None
            }
        })
        .collect();
    let failed = steps.iter().filter(|s| **s == Step::Failed).count();
    let step = if steps
        .iter()
        .any(|s| matches!(s, Step::Waiting | Step::Working))
    {
        Step::Working
    } else if failed == steps.len() || steps.last() == Some(&Step::Failed) {
        Step::Failed
    } else {
        Step::Done
    };
    (step, failed)
}

/// Where each run of steps shown folded starts in `blocks`; `live` as in [`run_open`].
pub(crate) fn folded_runs(blocks: &[Block], live: bool) -> Vec<usize> {
    let mut starts = vec![];
    let mut n = 0;
    while n < blocks.len() {
        if !is_step(&blocks[n]) {
            n += 1;
            continue;
        }
        let (len, foldable) = step_run(blocks, n);
        if foldable && run_folded(&blocks[n..n + len], run_open(blocks, n, len, live)) {
            starts.push(n);
        }
        n += len;
    }
    starts
}

/// Opens every block `flag` picks out, or closes them all when none is left to open. Returns
/// the first message that changed, if any.
pub(crate) fn toggle_all(
    messages: &mut [Msg],
    flag: fn(&mut Block) -> Option<&mut bool>,
) -> Option<usize> {
    let open = messages.iter_mut().any(|m| match m {
        Msg::Agent(blocks) => blocks.iter_mut().any(|b| flag(b).is_some_and(|f| !*f)),
        Msg::User(..) => false,
    });
    let mut first = None;
    for (ix, msg) in messages.iter_mut().enumerate() {
        let Msg::Agent(blocks) = msg else { continue };
        for shown in blocks.iter_mut().filter_map(flag) {
            if *shown != open {
                *shown = open;
                first.get_or_insert(ix);
            }
        }
    }
    first
}

/// The one option in a permission request rendered as the filled button: the first way of saying
/// yes, however the agent orders the rest. Options that describe themselves in `kind` (ACP) are
/// picked by it; agents that send no kinds at all get their first "allow" in the name honoured.
pub(crate) fn primary_option(options: &[PermissionOption]) -> Option<usize> {
    if options.iter().any(|o| !o.kind.is_empty()) {
        options.iter().position(|o| o.kind == "allow_once")
    } else {
        options
            .iter()
            .position(|o| o.name.to_lowercase().contains("allow"))
    }
}

pub(crate) fn file_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // What follows the host (empty or `localhost`) is the path, with its leading slash.
    let path = match rest.find('/') {
        Some(at) if rest[..at].is_empty() || rest[..at].eq_ignore_ascii_case("localhost") => {
            &rest[at..]
        }
        _ => return None,
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let decoded = (bytes[at] == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match decoded {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The message of the conversation as the screen drawn by NSH shows it: the user's text with its
/// pictures, or the agent's blocks. `busy` says the agent is at work, on the last message.
pub(crate) fn chat_items(messages: &[Msg], busy: bool) -> Vec<model::chat::Item> {
    // What the screen holds now: a message whose blocks would be built again exactly as they
    // stand keeps them, so a frame spends its rebuild on the message a chunk went into, not on
    // every message in the history (the engine compares each message's blocks itself, and the
    // pieces here are a byte-for-byte match, so nothing shows twice).
    let now = model::chat::shown();
    messages
        .iter()
        .enumerate()
        .map(|(ix, msg)| match msg {
            Msg::User(text, pictures) => model::chat::Item::User {
                text: text.clone(),
                pictures: pictures
                    .iter()
                    .map(|a| model::chat::Picture {
                        uri: a.data_uri(),
                        size: a.size,
                    })
                    .collect(),
            },
            Msg::Agent(blocks) => {
                let held = match now.items.get(ix) {
                    Some(model::chat::Item::Agent { blocks, .. }) => &**blocks,
                    _ => &[] as &[model::chat::Block],
                };
                model::chat::Item::Agent {
                    index: ix,
                    blocks: std::sync::Arc::new(chat_blocks(
                        blocks,
                        busy && ix + 1 == messages.len(),
                        held,
                    )),
                }
            }
        })
        .collect()
}

/// Whether the run's calls waited, were all told to go, all went well, or one failed.
pub(crate) fn chat_phase(step: Step) -> model::chat::Phase {
    match step {
        Step::Waiting => model::chat::Phase::Waiting,
        Step::Working => model::chat::Phase::Working,
        Step::Done => model::chat::Phase::Done,
        Step::Failed => model::chat::Phase::Failed,
    }
}

/// The plan entry in progress in the latest plan, which a call that begins now works towards.
pub(crate) fn task_in_progress(messages: &[Msg]) -> Option<String> {
    let entries = messages.iter().rev().find_map(|msg| match msg {
        Msg::Agent(blocks) => blocks.iter().find_map(|b| {
            if let Block::Plan(entries) = b {
                Some(entries)
            } else {
                None
            }
        }),
        _ => None,
    })?;
    entries
        .iter()
        .find(|(_, step)| *step == Step::Working)
        .map(|(text, _)| text.clone())
}

/// How long a finished run of steps took: from its first call's start to its last one's end. Nothing
/// while a call is still going.
pub(crate) fn run_took(run: &[Block]) -> Option<Duration> {
    let tools: Vec<&ToolBlock> = run
        .iter()
        .filter_map(|b| {
            if let Block::Tool(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect();
    let start = tools.iter().map(|t| t.started).min()?;
    let end = tools
        .iter()
        .map(|t| t.ended)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()?;
    Some(end.saturating_duration_since(start))
}

/// What a call does: by the kind the agent gave it, or by the first word of its title when it gave
/// none (or only `other`).
pub(crate) fn tool_kind(tool: &ToolBlock) -> model::chat::Kind {
    use crate::model::chat::Kind;
    match tool.kind.as_str() {
        "read" => return Kind::Read,
        "search" => return Kind::Search,
        "edit" | "delete" | "move" => return Kind::Edit,
        "execute" => return Kind::Command,
        "fetch" => return Kind::Fetch,
        _ => {}
    }
    match tool
        .title
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "read" | "cat" | "view" | "open" => Kind::Read,
        "grep" | "glob" | "find" | "search" | "list" | "ls" | "rg" => Kind::Search,
        "edit" | "write" | "patch" | "apply_patch" | "multiedit" | "delete" | "move" => Kind::Edit,
        "bash" | "shell" | "run" | "execute" | "sh" => Kind::Command,
        "fetch" | "webfetch" | "websearch" => Kind::Fetch,
        _ => Kind::Other,
    }
}

/// How many calls of each kind a run made, and the files its edits touched, once each.
pub(crate) fn run_kinds(run: &[Block]) -> (Vec<(model::chat::Kind, usize)>, Vec<String>) {
    let mut kinds: Vec<(model::chat::Kind, usize)> = vec![];
    let mut edits: Vec<String> = vec![];
    for tool in run.iter().filter_map(|b| {
        if let Block::Tool(t) = b {
            Some(t)
        } else {
            None
        }
    }) {
        let kind = tool_kind(tool);
        match kinds.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => kinds.push((kind, 1)),
        }
        if kind == model::chat::Kind::Edit
            && let Some(path) = &tool.path
            && !edits.contains(path)
        {
            edits.push(path.clone());
        }
    }
    (kinds, edits)
}

/// What a run of steps is at: the call going now, else the last one made.
pub(crate) fn run_latest(run: &[Block]) -> Option<String> {
    let mut tools = run.iter().filter_map(|b| {
        if let Block::Tool(t) = b {
            Some(t)
        } else {
            None
        }
    });
    let all: Vec<&ToolBlock> = tools.by_ref().collect();
    all.iter()
        .rev()
        .find(|t| matches!(t.status, Step::Waiting | Step::Working))
        .or(all.last())
        .map(|t| t.title.clone())
}

/// The plan task a run of calls worked towards: the one task its calls name, none when they name
/// no task or span several.
pub(crate) fn run_task(run: &[Block]) -> Option<String> {
    let mut tasks = run.iter().filter_map(|b| {
        if let Block::Tool(tool) = b {
            tool.task.as_deref()
        } else {
            None
        }
    });
    let first = tasks.next()?;
    tasks.all(|task| task == first).then(|| first.to_string())
}

/// One agent message's blocks for the conversation drawn by NSH. Runs of [`MIN_RUN`] or more steps
/// (tool calls and thoughts) that hold a call also carry the row that folds them; the steps stay in place
/// either way, so the indexes their answers are told by hold. `live` as in [`run_open`].
pub(crate) fn chat_blocks(
    blocks: &[Block],
    live: bool,
    held: &[model::chat::Block],
) -> Vec<model::chat::Block> {
    let mut out: Vec<model::chat::Block> = Vec::with_capacity(blocks.len());
    let mut n = 0;
    while n < blocks.len() {
        // A step in the middle of a run too short to fold is not the start of another.
        let inside = n > 0 && is_step(&blocks[n - 1]);
        let (len, foldable) = if is_step(&blocks[n]) && !inside {
            step_run(blocks, n)
        } else {
            (0, false)
        };
        if foldable {
            let run = &blocks[n..n + len];
            let open = run_open(blocks, n, len, live);
            let (outcome, failed) = run_outcome(run);
            let phase = if open { Step::Working } else { outcome };
            let task = run_task(run);
            let calls = run.iter().filter(|b| matches!(b, Block::Tool(_))).count();
            let (kinds, edits) = run_kinds(run);
            out.push(model::chat::Block::Run {
                index: n,
                count: len,
                calls,
                failed,
                phase: chat_phase(phase),
                folded: run_folded(run, open),
                task,
                took: run_took(run),
                latest: run_latest(run),
                kinds,
                edits,
            });
            for (k, block) in run.iter().enumerate() {
                out.push(match block {
                    Block::Tool(tool) => model::chat::Block::Tool(chat_tool(n + k, tool)),
                    Block::Thought(text, open) => model::chat::Block::Thought {
                        index: n + k,
                        text: text.clone(),
                        open: *open,
                    },
                    _ => continue,
                });
            }
            n += len;
            continue;
        }
        out.push(match &blocks[n] {
            Block::Text(text) => {
                // A piece of long text is a row of its own, so the one being written is the only
                // one the engine makes again while a reply streams (see [`pieces_of`]). A piece
                // that could be built again exactly as it is keeps `held`'s (see `chat_items`).
                for (stitch, piece) in pieces_of(text).into_iter().enumerate() {
                    let piece_text = match held.get(out.len()) {
                        Some(model::chat::Block::Markdown { text, .. })
                            if text.as_ref() == piece =>
                        {
                            text.clone()
                        }
                        _ => piece.into(),
                    };
                    out.push(model::chat::Block::Markdown {
                        text: piece_text,
                        stitch: stitch > 0,
                    });
                }
                n += 1;
                continue;
            }
            Block::Thought(text, open) => model::chat::Block::Thought {
                index: n,
                text: text.clone(),
                open: *open,
            },
            Block::Tool(tool) => model::chat::Block::Tool(chat_tool(n, tool)),
            Block::Permission(ask) => {
                let primary = primary_option(&ask.options);
                model::chat::Block::Ask {
                    index: n,
                    title: ask.title.clone(),
                    options: ask
                        .options
                        .iter()
                        .enumerate()
                        .map(|(at, option)| model::chat::AskOption {
                            at,
                            label: option.name.clone(),
                            primary: primary == Some(at),
                        })
                        .collect(),
                    answered: ask.answered.clone(),
                }
            }
            Block::Plan(entries) => model::chat::Block::Plan(
                entries
                    .iter()
                    .map(|(text, step)| model::chat::Entry {
                        text: text.clone(),
                        phase: chat_phase(*step),
                    })
                    .collect(),
            ),
            Block::Approve {
                agent,
                new_agent,
                mcp,
                resume: _,
                answered,
            } => {
                let (question, lines) = approval_text(agent, *new_agent, mcp);
                model::chat::Block::Approve {
                    index: n,
                    question,
                    lines,
                    answered: answered.map(|said| match (said, new_agent) {
                        (true, _) => "Approved".into(),
                        (false, true) => "Not run".into(),
                        (false, false) => "Left out".into(),
                    }),
                }
            }
        });
        n += 1;
    }
    out
}

/// One tool call for the conversation drawn by NSH.
pub(crate) fn chat_tool(at: usize, tool: &ToolBlock) -> model::chat::Tool {
    model::chat::Tool {
        index: at,
        title: tool.title.clone(),
        phase: chat_phase(tool.status),
        open: tool.open,
        arguments: tool.arguments.clone(),
        result: tool.result.clone(),
        path: tool.path.clone(),
    }
}

/// What the approval card says: the question Po asks, and the commands it would hand over.
pub(crate) fn approval_text(
    agent: &config::Agent,
    new_agent: bool,
    mcp: &[config::McpServer],
) -> (String, Vec<String>) {
    let from = |plugin: &Option<String>| {
        plugin
            .as_ref()
            .map_or_else(String::new, |id| format!("  (plugin {id})"))
    };
    let mut lines = Vec::new();
    if new_agent {
        let source = from(&agent.plugin);
        lines.push(match &agent.daemon {
            Some(url) => format!("{}  (daemon {url}){source}", agent.command),
            None => format!("{}{source}", agent.command),
        });
    }
    lines.extend(
        mcp.iter()
            .map(|server| format!("{}: {}{}", server.name, server.line(), from(&server.plugin))),
    );
    let agent_source = agent.plugin.as_ref().map_or_else(
        || "your config".to_string(),
        |id| format!("the plugin {id}"),
    );
    let question = match (new_agent, mcp.is_empty()) {
        (true, true) => format!("“{}” is not one of Po's own agents: it comes from {agent_source}. Run this command?", agent.name),
        (true, false) => format!("“{}” is not one of Po's own agents, and your config or plugins list MCP servers for it. Run these commands?", agent.name),
        (false, _) => "Your config or plugins list MCP servers for the agent to start, which run as you. Hand them over?".to_string(),
    };
    (question, lines)
}

/// What a thread is doing, in a line: the question it waits on, the tool it is
/// running, or the last thing it said.
#[hotpath::measure]
pub(crate) fn preview_of(messages: &[Msg], busy: bool) -> String {
    if let Some(Msg::Agent(blocks)) = messages.last() {
        if let Some(ask) = blocks.iter().find_map(|b| match b {
            Block::Permission(ask) if ask.answered.is_none() => Some(ask),
            _ => None,
        }) {
            return format!("Approve: {}", ask.title);
        }
        if let Some(what) = blocks.iter().find_map(|b| match b {
            Block::Approve {
                agent,
                new_agent: true,
                answered: None,
                ..
            } => Some(agent.command.clone()),
            Block::Approve {
                mcp,
                answered: None,
                ..
            } => Some(format!(
                "MCP servers: {}",
                mcp.iter()
                    .map(|server| server.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            _ => None,
        }) {
            return format!("Approve running: {what}");
        }
        // A tool still going is the latest thing, whatever text came after it.
        if let Some(tool) = blocks.iter().rev().find_map(|b| match b {
            Block::Tool(tool) if tool.status == Step::Working => Some(tool),
            _ => None,
        }) {
            return tool.title.clone();
        }
        for block in blocks.iter().rev() {
            if let Block::Text(text) = block
                && let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty())
            {
                return line.trim().to_string();
            }
        }
    }
    if busy {
        "Working…".into()
    } else {
        String::new()
    }
}

/// `text` with its first letter in capitals, as a sentence starts.
pub(crate) fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod file_url_tests {
    use super::file_of;

    #[test]
    fn file_urls_become_absolute_paths() {
        assert_eq!(
            file_of("file:///home/aks/a.png").as_deref(),
            Some("/home/aks/a.png")
        );
        assert_eq!(
            file_of("file://localhost/tmp/a%20b.png").as_deref(),
            Some("/tmp/a b.png")
        );
        assert_eq!(file_of("file://other/tmp/a.png"), None);
        assert_eq!(file_of("https://example.com/a.png"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::acp::PermissionOption;
    
    
    
    
    
    

    #[test]
    fn a_text_shorter_than_a_piece_is_one_piece_and_long_ones_split_at_paragraphs_only() {
        let one = "## Title\n\nRust gives every value a single owner.\n";
        assert_eq!(pieces_of(one), [one]);
        // Paragraphs, headings, lists and a fenced block, then more prose: splits land at blank
        // lines outside a fence, and the pieces back, joined, give back the text.
        let mut long = String::new();
        for part in 0..40 {
            long.push_str("## Part\n\nA paragraph about a value and who owns it meanwhile.\n\n- A value has one owner\n- Borrows are checked\n\n");
            if part == 10 {
                long.push_str("```rust\nfn longest<'a>(a: &'a str, b: &'a str) -> &'a str { a }\n\n\n\nfn other() {}\n```\n\n");
            }
        }
        let pieces = pieces_of(&long);
        assert!(pieces.len() > 4, "pieces: {}", pieces.len());
        assert_eq!(pieces.concat(), long);
        assert!(
            pieces.iter().all(|piece| piece.len() < 2 * PIECE),
            "a piece is not a small run of paragraphs, longest was {}",
            pieces.iter().map(|p| p.len()).max().unwrap_or(0)
        );
        // No piece sits inside a fence: every fence a piece opens, that piece also shuts.
        for piece in &pieces {
            let opens = piece.match_indices("```rust\n").count();
            let shuts = piece
                .lines()
                .filter(|line| line.trim_end() == "```")
                .count();
            assert_eq!(
                shuts, opens,
                "a piece holds a fence not its other: {:?}",
                piece
            );
        }
    }

    #[test]
    fn pieces_keep_what_markdown_reads_across_a_blank_line_together() {
        let filler =
            "A paragraph about a value and who owns it meanwhile, long enough to count.\n\n"
                .repeat(10);
        let kept = |block: &str| {
            let text = format!("{filler}{block}{filler}");
            let pieces = pieces_of(&text);
            assert_eq!(pieces.concat(), text);
            assert!(pieces.len() > 1, "the text was not split at all");
            assert!(
                pieces.iter().any(|piece| piece.contains(block)),
                "a piece cut {block:?}"
            );
        };
        kept("~~~\nfn a() {}\n\n\nfn b() {}\n~~~\n\n");
        kept("````md\n```\n\nnested\n```\n````\n\n");
        kept("1. a\n\n2. b\n\n3. c\n\n");
        kept("> quoted\n\n> on\n\n");
        // Blank lines of CRLF text and of spaces end a paragraph all the same.
        let crlf = "A paragraph about a value and who owns it meanwhile.\r\n\r\n".repeat(40);
        assert!(pieces_of(&crlf).len() > 1);
        let spaced = "A paragraph about a value and who owns it meanwhile.\n  \n".repeat(40);
        assert!(pieces_of(&spaced).len() > 1);
        // A reference may be defined after the piece that uses it, so the text is not split.
        let linked =
            format!("See [the book].\n\n{filler}[the book]: https://doc.rust-lang.org/book/\n");
        assert_eq!(pieces_of(&linked), [linked.as_str()]);
    }

    fn thought_flag(b: &mut Block) -> Option<&mut bool> {
        if let Block::Thought(_, shown) = b {
            Some(shown)
        } else {
            None
        }
    }

    #[test]
    fn toggling_opens_all_then_folds_all() {
        let mut messages = vec![
            Msg::User("hi".into(), vec![]),
            Msg::Agent(vec![
                Block::Thought("a".into(), true),
                Block::Text("t".into()),
            ]),
            Msg::Agent(vec![Block::Thought("b".into(), false)]),
        ];
        assert_eq!(toggle_all(&mut messages, thought_flag), Some(2));
        assert!(matches!(&messages[1], Msg::Agent(b) if matches!(b[0], Block::Thought(_, true))));
        assert!(matches!(&messages[2], Msg::Agent(b) if matches!(b[0], Block::Thought(_, true))));
        assert_eq!(toggle_all(&mut messages, thought_flag), Some(1));
        assert!(matches!(&messages[1], Msg::Agent(b) if matches!(b[0], Block::Thought(_, false))));
        assert_eq!(
            toggle_all(&mut [Msg::User("x".into(), vec![])], thought_flag),
            None
        );
    }

    fn option(name: &str, kind: &str) -> PermissionOption {
        PermissionOption {
            id: name.to_lowercase(),
            name: name.into(),
            kind: kind.into(),
        }
    }

    // “Always allow” listed first is not the first way of saying yes: the once-option is.
    #[test]
    fn allow_always_first_is_not_primary() {
        let options = vec![
            option("Always allow", "allow_always"),
            option("Allow once", "allow_once"),
            option("Deny", "reject_once"),
        ];
        assert_eq!(primary_option(&options), Some(1));
    }

    #[test]
    fn first_of_several_allow_once_wins() {
        let options = vec![
            option("Allow once", "allow_once"),
            option("Allow once (this project)", "allow_once"),
            option("Deny", "reject_once"),
        ];
        assert_eq!(primary_option(&options), Some(0));
    }

    // No option says yes once, so nothing is primary and all buttons stay plain.
    #[test]
    fn kinds_without_allow_once_leave_nothing_primary() {
        let options = vec![
            option("Always allow", "allow_always"),
            option("Deny once", "reject_once"),
            option("Always deny", "reject_always"),
        ];
        assert_eq!(primary_option(&options), None);
    }

    // Agents from before `kind` was in the protocol describe options in names only.
    #[test]
    fn names_only_fall_back_to_the_first_allow() {
        let options = vec![
            option("Allow once", ""),
            option("Allow for session", ""),
            option("Deny", ""),
        ];
        assert_eq!(primary_option(&options), Some(0));
    }

    fn tool_for(task: Option<&str>) -> Block {
        Block::Tool(ToolBlock {
            id: String::new(),
            title: "call".into(),
            status: Step::Done,
            kind: String::new(),
            started: std::time::Instant::now(),
            ended: None,
            open: false,
            fold: None,
            task: task.map(str::to_string),
            arguments: String::new(),
            result: String::new(),
            path: None,
        })
    }

    #[test]
    fn a_run_is_named_by_the_one_task_its_calls_work_towards() {
        assert_eq!(
            run_task(&[
                tool_for(None),
                tool_for(Some("Fix it")),
                tool_for(Some("Fix it"))
            ]),
            Some("Fix it".to_string())
        );
        assert_eq!(run_task(&[tool_for(None), tool_for(None)]), None);
        assert_eq!(
            run_task(&[
                tool_for(Some("A")),
                tool_for(Some("B")),
                tool_for(Some("B"))
            ]),
            None
        );
    }

    #[test]
    fn a_call_takes_the_task_in_progress_in_the_latest_plan() {
        let mut messages = vec![Msg::Agent(vec![Block::Plan(vec![
            ("Read".into(), Step::Done),
            ("Fix it".into(), Step::Working),
        ])])];
        assert_eq!(task_in_progress(&messages), Some("Fix it".to_string()));
        messages.push(Msg::User("go on".into(), vec![]));
        messages.push(Msg::Agent(vec![Block::Plan(vec![(
            "Fix it".into(),
            Step::Done,
        )])]));
        assert_eq!(task_in_progress(&messages), None);
        assert_eq!(task_in_progress(&[]), None);
    }

    fn tool(title: &str, status: Step) -> Block {
        Block::Tool(ToolBlock {
            id: String::new(),
            title: title.into(),
            status,
            kind: String::new(),
            started: std::time::Instant::now(),
            ended: None,
            open: false,
            fold: None,
            task: None,
            arguments: String::new(),
            result: String::new(),
            path: None,
        })
    }

    #[test]
    fn a_run_of_calls_starts_folded_even_while_it_runs_or_fails() {
        let run = vec![
            tool("a", Step::Done),
            tool("b", Step::Done),
            tool("c", Step::Done),
        ];
        assert!(run_folded(&run, false));
        let failed = vec![
            tool("a", Step::Done),
            tool("b", Step::Failed),
            tool("c", Step::Done),
        ];
        assert!(run_folded(&failed, false));
        let running = vec![
            tool("a", Step::Done),
            tool("b", Step::Working),
            tool("c", Step::Waiting),
        ];
        assert!(run_folded(&running, false));
    }

    #[test]
    fn a_click_on_a_run_overrides_the_default() {
        let mut run = vec![
            tool("a", Step::Done),
            tool("b", Step::Done),
            tool("c", Step::Done),
        ];
        let Block::Tool(first) = &mut run[0] else {
            unreachable!()
        };
        first.fold = Some(false);
        assert!(!run_folded(&run, false));
        let mut failed = vec![
            tool("a", Step::Failed),
            tool("b", Step::Done),
            tool("c", Step::Done),
        ];
        let Block::Tool(first) = &mut failed[0] else {
            unreachable!()
        };
        first.fold = Some(true);
        assert!(run_folded(&failed, false));
    }

    #[test]
    fn only_runs_of_two_or_more_are_found_folded() {
        let blocks = vec![
            tool("a", Step::Done),
            Block::Text("x".into()),
            tool("b", Step::Done),
            tool("c", Step::Done),
        ];
        assert_eq!(folded_runs(&blocks, false), vec![2]);
    }

    #[test]
    fn thoughts_between_calls_fold_into_the_run_and_text_ends_it() {
        let thought = || Block::Thought("hm".into(), false);
        let blocks = vec![
            thought(),
            tool("a", Step::Done),
            thought(),
            tool("b", Step::Done),
            Block::Text("x".into()),
            thought(),
            thought(),
            thought(),
        ];
        assert_eq!(folded_runs(&blocks, false), vec![0]);
        let out = chat_blocks(&blocks, false, &[]);
        assert!(matches!(
            out[0],
            crate::model::chat::Block::Run {
                index: 0,
                count: 4,
                calls: 2,
                ..
            }
        ));
        assert_eq!(
            out.iter()
                .filter(|b| matches!(b, crate::model::chat::Block::Run { .. }))
                .count(),
            1
        );
        // Thoughts alone have no call to keep the fold, so they stay as they are.
        assert_eq!(
            out.iter()
                .filter(|b| matches!(b, crate::model::chat::Block::Thought { .. }))
                .count(),
            5
        );
    }

    #[test]
    fn calls_are_counted_by_the_kind_the_agent_gave_or_their_title() {
        use crate::model::chat::Kind;
        let call = |title: &str, kind: &str, path: Option<&str>| {
            let Block::Tool(mut t) = tool(title, Step::Done) else {
                unreachable!()
            };
            t.kind = kind.into();
            t.path = path.map(str::to_string);
            Block::Tool(t)
        };
        let run = vec![
            call("Read a.rs", "read", Some("a.rs")),
            call("grep foo", "other", None),
            call("bash ls", "", None),
            call("Edit b.rs", "edit", Some("b.rs")),
            call("Edit b.rs again", "edit", Some("b.rs")),
            call("mystery", "", None),
        ];
        let (kinds, edits) = run_kinds(&run);
        assert_eq!(
            kinds,
            vec![
                (Kind::Read, 1),
                (Kind::Search, 1),
                (Kind::Command, 1),
                (Kind::Edit, 2),
                (Kind::Other, 1)
            ]
        );
        // A file read (it has a path too) is no edit, and one edited twice is chipped once.
        assert_eq!(edits, vec!["b.rs".to_string()]);
    }

    /// The phase and fold of the one run in `blocks`.
    fn run_row(blocks: &[Block], live: bool) -> (crate::model::chat::Phase, bool) {
        chat_blocks(blocks, live, &[])
            .iter()
            .find_map(|b| match b {
                crate::model::chat::Block::Run { phase, folded, .. } => Some((*phase, *folded)),
                _ => None,
            })
            .expect("the blocks hold a run")
    }

    #[test]
    fn a_run_is_still_working_between_its_calls() {
        use crate::model::chat::Phase;
        let blocks = vec![tool("a", Step::Done), tool("b", Step::Done)];
        assert_eq!(run_row(&blocks, true).0, Phase::Working);
        // The turn over, it is done, with no call having changed.
        assert_eq!(run_row(&blocks, false).0, Phase::Done);
    }

    #[test]
    fn a_failure_moves_no_rows_until_the_run_ends_in_it() {
        use crate::model::chat::Phase;
        let blocks = vec![tool("a", Step::Done), tool("b", Step::Failed)];
        assert_eq!(run_row(&blocks, true), (Phase::Working, true));
        assert_eq!(folded_runs(&blocks, true), vec![0]);
        assert_eq!(run_row(&blocks, false), (Phase::Failed, false));
        assert_eq!(folded_runs(&blocks, false), Vec::<usize>::new());
    }

    #[test]
    fn a_click_holds_on_a_run_still_going() {
        let mut run = vec![tool("a", Step::Done), tool("b", Step::Done)];
        let Block::Tool(first) = &mut run[0] else {
            unreachable!()
        };
        first.fold = Some(false);
        assert!(!run_folded(&run, true));
    }

    #[test]
    fn only_the_last_message_of_a_busy_thread_has_a_run_going() {
        use crate::model::chat::{Item, Phase};
        let run = || Msg::Agent(vec![tool("a", Step::Done), tool("b", Step::Done)]);
        let phases = |busy| -> Vec<Phase> {
            chat_items(&[run(), run()], busy)
                .iter()
                .map(|item| {
                    let Item::Agent { blocks, .. } = item else {
                        unreachable!()
                    };
                    let crate::model::chat::Block::Run { phase, .. } = blocks[0] else {
                        unreachable!()
                    };
                    phase
                })
                .collect()
        };
        assert_eq!(phases(true), [Phase::Done, Phase::Working]);
        assert_eq!(phases(false), [Phase::Done, Phase::Done]);
    }

    #[test]
    fn a_run_followed_by_text_folds_in_the_latest_message() {
        let blocks = vec![
            tool("a", Step::Done),
            tool("b", Step::Done),
            tool("c", Step::Done),
            Block::Text("x".into()),
        ];
        assert_eq!(folded_runs(&blocks, false), vec![0]);
        assert_eq!(folded_runs(&blocks[..3], false), vec![0]);
    }

    #[test]
    fn a_running_tool_is_the_preview_even_when_text_follows_it() {
        let messages = vec![Msg::Agent(vec![
            tool("Run tests", Step::Working),
            Block::Text("Looking at the output\n".into()),
        ])];
        assert_eq!(preview_of(&messages, true), "Run tests");
    }

    #[test]
    fn the_preview_falls_back_to_the_last_line_then_to_working() {
        let said = vec![Msg::Agent(vec![
            tool("Read", Step::Done),
            Block::Text("first\n\nlast line  \n\n".into()),
        ])];
        assert_eq!(preview_of(&said, true), "last line");
        let silent = vec![Msg::Agent(vec![tool("Read", Step::Done)])];
        assert_eq!(preview_of(&silent, true), "Working…");
        assert_eq!(preview_of(&silent, false), "");
    }

    #[test]
    fn text_after_a_run_ends_it_while_the_turn_goes_on() {
        use crate::model::chat::Phase;
        let blocks = vec![
            tool("a", Step::Done),
            tool("b", Step::Done),
            Block::Text("x".into()),
        ];
        assert_eq!(run_row(&blocks, true).0, Phase::Done);
    }
}
