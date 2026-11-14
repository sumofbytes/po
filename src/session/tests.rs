use crate::files::watch::*;
use crate::reader::state::*;
use crate::session::chat::*;
use crate::session::project::*;
use crate::session::thread::*;

#[test]
fn a_thread_keeps_its_sidebar_row_while_its_agent_works() {
    let now = 1_800_000_000_000;
    let day = 24 * 3_600_000;
    let mut thread = Thread::blank(1);
    thread.active_at = now;
    // On screen: keeps its row whatever it is doing. Folded and idle: only the agent's doing
    // keeps it there, so idle with no agent folds however fresh it is.
    assert!(listed(now, 1, &thread));
    assert!(!listed(now, 0, &thread));
    thread.busy = true;
    thread.active_at = now - 2 * day;
    // Running keeps even an old one on the list.
    assert!(listed(now, 0, &thread));
    let mut old = Thread::blank(2);
    old.active_at = now - day - 1;
    // Idle past a day: search and the history find it; the fold doesn't.
    assert!(!listed(now, 0, &old));
    // The one on screen is the exception.
    assert!(listed(now, 2, &old));
}

#[test]
fn a_thread_title_opens_with_a_capital_unless_it_is_code() {
    let cap = |t: &str| capitalized(t.to_string());
    assert_eq!(cap("fix the build"), "Fix the build");
    assert_eq!(cap("why, though?"), "Why, though?");
    assert_eq!(cap("Already fine"), "Already fine");
    for as_typed in [
        "src/main.rs is slow",
        "cargo test fails",
        "snake_case rename",
        "v2 plan",
        "#115 review",
        "…",
        "",
    ] {
        assert_eq!(cap(as_typed), as_typed);
    }
}

use crate::agent::dex::Queue;

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
        "A paragraph about a value and who owns it meanwhile, long enough to count.\n\n".repeat(10);
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

#[test]
fn the_sidebar_says_how_long_ago_in_a_word() {
    let now = 1_800_000_000_000;
    let before = |ms: u64| ago(now, now - ms);
    assert_eq!(before(30_000), None);
    assert_eq!(before(5 * 60_000).as_deref(), Some("5m"));
    assert_eq!(before(3 * 3_600_000).as_deref(), Some("3h"));
    assert_eq!(before(2 * 86_400_000).as_deref(), Some("2d"));
    assert_eq!(before(21 * 86_400_000).as_deref(), Some("3w"));
    assert_eq!(before(120 * 86_400_000).as_deref(), Some("4mo"));
    // A counter from before the time was kept, and a time in the future, say nothing.
    assert_eq!(ago(now, 7), None);
    assert_eq!(ago(now, now + 1), None);
}

#[test]
fn a_thread_idle_for_more_than_a_day_is_stale() {
    let now = 1_800_000_000_000;
    assert!(!stale(now, now - 23 * 3_600_000));
    assert!(!stale(now, now - STALE_MS));
    assert!(stale(now, now - STALE_MS - 1));
    // A counter from before the time was kept is old; a time in the future is not.
    assert!(stale(now, 7));
    assert!(!stale(now, now + 1));
}

#[test]
fn a_project_remembers_the_threads_closed_in_it_newest_first_once_each_and_not_too_many() {
    let mut project = Reader::build_project(std::path::PathBuf::from("/x"), None, vec![]);
    let state = |session: &str| crate::session::workspace::ThreadState {
        agent: "a".into(),
        session: session.into(),
        title: String::new(),
        active_at: 0,
        parent_session: None,
        renamed: false,
    };
    for n in 0..15 {
        project.remember(state(&format!("s{n}")));
    }
    project.remember(state("s10"));
    assert_eq!(project.closed.len(), crate::session::workspace::CLOSED_KEPT);
    assert_eq!(project.closed[0].session, "s10");
    assert_eq!(
        project.closed.iter().filter(|c| c.session == "s10").count(),
        1
    );
    assert_eq!(project.closed[1].session, "s14");
}

#[test]
fn a_blank_thread_or_one_without_a_session_is_not_remembered_when_closed() {
    let mut thread = Thread::blank(1);
    assert!(thread.closed_state().is_none(), "blank");
    thread.blank = false;
    thread.agent = Some("a".into());
    assert!(thread.closed_state().is_none(), "no session");
    thread.session = Some("s".into());
    assert_eq!(
        thread.closed_state().map(|c| c.session),
        Some("s".to_string())
    );
}

#[test]
fn a_name_the_user_gave_stands_in_place_of_the_first_message() {
    let mut thread = started_thread();
    assert_eq!(thread.title(), "fix it");
    assert!(!thread.closed_state().unwrap().renamed);
    thread.renamed = Some("the retry bug".into());
    assert_eq!(thread.title(), "the retry bug");
    let kept = thread.closed_state().unwrap();
    assert_eq!((kept.title.as_str(), kept.renamed), ("the retry bug", true));
    thread.renamed = None;
    assert_eq!(
        thread.title(),
        "fix it",
        "taking the name away gives the first message back"
    );
}

fn started_thread() -> Thread {
    let mut thread = Thread::blank(1);
    thread.blank = false;
    thread.agent = Some("a".into());
    thread.session = Some("s".into());
    thread.messages.push(Msg::User("fix it".into(), vec![]));
    thread
        .messages
        .push(Msg::Agent(vec![Block::Text("fixed".into())]));
    thread
}

// A resumed session replays its history as updates while `loading`, and a turn on a parked
// thread records while nothing shows: an update is recorded either way.
#[test]
fn an_update_is_recorded_while_the_history_replays_or_off_screen() {
    let mut thread = started_thread();
    thread.loading = true;
    let earlier = record(
        &mut thread,
        &serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "t1",
            "title": "Edit src/a.rs",
            "status": "in_progress",
            "rawInput": { "path": "src/a.rs" }
        }),
    );
    assert_eq!(earlier, None, "a tool call starts its own message");
    let mut thread = thread;
    record(
        &mut thread,
        &serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "status": "failed"
        }),
    );
    let Some(Msg::Agent(blocks)) = thread.messages.last() else {
        unreachable!("agent work")
    };
    let Some(Block::Tool(tool)) = blocks
        .iter()
        .find(|b| matches!(b, Block::Tool(t) if t.id == "t1"))
    else {
        unreachable!("the call was recorded")
    };
    assert!(
        matches!(tool.status, Step::Failed),
        "the settled status was taken in"
    );
    assert_eq!(
        tool.path.as_deref(),
        Some("src/a.rs"),
        "the file the call touches is known for the chip"
    );
    record(
        &mut thread,
        &serde_json::json!({ "sessionUpdate": "usage_update", "used": 100, "size": 1000 }),
    );
    assert_eq!(
        thread.usage,
        Some(crate::session::state::Usage {
            used: 100,
            size: 1000
        })
    );
}

#[test]
fn a_thread_switches_agent_only_with_nothing_under_way_and_something_to_pass_on() {
    assert!(started_thread().can_switch_agent());
    assert!(
        !Thread::blank(1).can_switch_agent(),
        "blank: picking starts it instead"
    );
    let mut busy = started_thread();
    busy.busy = true;
    assert!(!busy.can_switch_agent());
    let mut unloaded = started_thread();
    unloaded.messages.clear();
    assert!(
        !unloaded.can_switch_agent(),
        "its conversation hasn't been replayed"
    );
    let mut no_session = started_thread();
    no_session.session = None;
    assert!(!no_session.can_switch_agent());
}

#[test]
fn a_switch_the_new_agent_has_not_heard_of_comes_back_to_the_agent_it_left() {
    let mut thread = started_thread();
    thread.switch = Some(Switch {
        agent: "a".into(),
        session: "s".into(),
        earlier: String::new(),
    });
    thread.agent = Some("b".into());
    thread.session = Some("empty".into());
    assert_eq!(
        thread.closed_state().map(|c| (c.agent, c.session)),
        Some(("a".to_string(), "s".to_string()))
    );
    thread.session = None;
    assert_eq!(
        thread.resumes_from(),
        Some(("a", "s")),
        "kept even if the new agent failed to start"
    );
    assert!(thread.can_switch_agent(), "and the user can pick again");
    thread.switch = None;
    assert_eq!(thread.resumes_from(), None);
}

#[test]
fn a_replayed_handoff_brings_the_earlier_messages_back() {
    let mut said = started_thread();
    said.messages
        .push(Msg::Agent(vec![Block::Text("*Switched to B.*".into())]));
    let earlier = crate::agent::handoff::write(&said.said());
    let mut thread = Thread::blank(1);
    thread.blank = false;
    thread.loading = true;
    let prompt = format!("{earlier}[The user has `a.rs` open in the viewer.]\n\nand now?");
    thread.update(&serde_json::json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": prompt } }));
    let users: Vec<&str> = thread
        .messages
        .iter()
        .filter_map(|msg| match msg {
            Msg::User(text, _) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users, ["fix it", "and now?"]);
    assert_eq!(
        thread.messages.len(),
        4,
        "the two messages, the note and the new message"
    );
    assert_eq!(
        thread.title(),
        "fix it",
        "still named for what it was first asked"
    );
}

#[test]
fn a_handoff_replayed_in_pieces_is_unfolded_when_the_session_is_ready() {
    let mut said = started_thread();
    said.messages.push(Msg::User("again".into(), vec![]));
    let prompt = format!("{}and now?", crate::agent::handoff::write(&said.said()));
    let mut thread = Thread::blank(1);
    thread.blank = false;
    thread.loading = true;
    for piece in prompt.as_bytes().chunks(7) {
        let piece = String::from_utf8_lossy(piece).to_string();
        thread.update(&serde_json::json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": piece } }));
    }
    thread.unfold_handoffs();
    let users: Vec<&str> = thread
        .messages
        .iter()
        .filter_map(|msg| match msg {
            Msg::User(text, _) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users, ["fix it", "again", "and now?"]);
    assert_eq!(thread.title(), "fix it");
}

// A stand-in agent that copies what it is sent to a file, to see that a running thread's
// agent is asked to stop before it is killed, and that an idle one is simply let go.
fn closed_with(busy: bool) -> String {
    let path = std::env::temp_dir().join(format!("po-retire-{}-{busy}", std::process::id()));
    std::fs::write(&path, "").unwrap();
    let (client, _events) = crate::agent::acp::Client::spawn(
        &format!("tee {}", path.display()),
        &std::env::temp_dir(),
        false,
        None,
        |_| vec![],
    )
    .expect("spawns tee");
    client.load("s1");
    let mut thread = Thread::blank(1);
    thread.busy = busy;
    thread.client = Some(crate::agent::Client::Acp(client));
    Thread::retire(thread);
    // Waits for the cancel to arrive; when none is expected, long enough to have seen it.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(if busy { 3000 } else { 400 });
    while std::time::Instant::now() < deadline
        && !(busy
            && std::fs::read_to_string(&path)
                .unwrap()
                .contains("session/cancel"))
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let sent = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).ok();
    sent
}

#[test]
fn a_running_thread_is_asked_to_stop_when_closed_and_an_idle_one_is_not() {
    assert!(closed_with(true).contains("session/cancel"));
    assert!(!closed_with(false).contains("session/cancel"));
}

#[test]
fn only_a_thread_with_work_to_lose_asks_before_closing() {
    let mut thread = Thread::blank(1);
    assert!(!thread.at_risk(), "blank");
    thread.blank = false;
    assert!(!thread.at_risk(), "idle");
    thread.busy = true;
    assert!(thread.at_risk(), "running");
    thread.busy = false;
    thread.queue.push(Queued {
        out: Outgoing {
            text: "next".into(),
            origin: Origin::User,
            images: vec![],
        },
        kind: None,
    });
    assert!(thread.at_risk(), "queued");
}

// submit's paths (§4.3 of the control plan): blank, unstarted, starting, asleep, ready,
// busy for an agent that takes messages mid-turn (dex) and one that doesn't (ACP).
#[test]
fn a_message_goes_where_its_thread_is() {
    assert_eq!(
        route(true, false, false, false, false, false, false),
        Where::Start,
        "blank"
    );
    assert_eq!(
        route(false, true, false, false, false, false, false),
        Where::Start,
        "unstarted"
    );
    assert_eq!(
        route(false, false, false, true, false, false, false),
        Where::Wait,
        "starting"
    );
    assert_eq!(
        route(false, false, true, false, false, false, false),
        Where::Wait,
        "asleep"
    );
    assert_eq!(
        route(false, false, false, false, false, false, false),
        Where::Dispatch,
        "ready"
    );
    assert_eq!(
        route(false, false, false, false, true, true, false),
        Where::Take(Queue::Steer),
        "dex mid-turn"
    );
    assert_eq!(
        route(false, false, false, false, true, true, true),
        Where::Take(Queue::Followup),
        "dex follow-up"
    );
    assert_eq!(
        route(false, false, false, false, true, false, false),
        Where::Queue,
        "ACP mid-turn"
    );
    // A thread that is busy is not starting anything, however the rest stands.
    assert_eq!(
        route(false, false, false, true, true, false, false),
        Where::Queue
    );
}

#[test]
fn an_empty_viewer_says_how_to_get_the_tree_back() {
    use crate::session::project::pick_hint;
    assert_eq!(pick_hint("a file", true), "Pick a file in the tree");
    assert!(pick_hint("a file", false).ends_with("back to the explorer"));
}

#[test]
fn an_agents_layout_is_saved_as_the_users_until_the_user_changes_it() {
    use crate::reader::state::AgentChange;
    // The agent opened the viewer: while it is open, the user's closed one is what is saved.
    let change = AgentChange::after(None, 1, 2);
    assert_eq!(AgentChange::saved(Some(&change), 2), 1);
    // The user moved on from it: what they left is theirs.
    assert_eq!(AgentChange::saved(Some(&change), 3), 3);
    // A second move on top of the agent's keeps the user's from before the first.
    let modes = AgentChange::after(None, 'f', 'd');
    let again = AgentChange::after(Some(modes), 'd', 'c');
    assert_eq!(AgentChange::saved(Some(&again), 'c'), 'f');
    // A move after the user changed it starts from theirs.
    let after_user = AgentChange::after(Some(modes), 'c', 'd');
    assert_eq!(AgentChange::saved(Some(&after_user), 'd'), 'c');
    assert_eq!(AgentChange::saved(Some(&after_user), 'x'), 'x');
    assert_eq!(AgentChange::saved(None, 'x'), 'x');
    // The user moved off the agent's layout and back by hand: the record is gone, so what
    // they came back to is what is saved.
    let change = AgentChange::after(None, 1, 2);
    assert!(AgentChange::settled(Some(change), 2).is_some());
    let gone = AgentChange::settled(Some(change), 3);
    assert!(gone.is_none());
    assert_eq!(AgentChange::saved(gone.as_ref(), 2), 2);
}

#[test]
fn an_agents_own_view_show_does_not_drop_its_pending_move() {
    use crate::ipc::command::Command;
    use crate::reader::state::{PendingMove, Screen};
    use crate::session::project::Mode;
    let from = Screen {
        side_open: false,
        agent_open: false,
        mode: Mode::Source,
        current: None,
        picked: None,
        shown: None,
    };
    // The agent shows a view on the thread whose file open waits: the move follows what the
    // agent touched, so it applies once the pause comes.
    let mut pending = PendingMove {
        thread: 1,
        command: Command::FileOpen {
            path: "a.rs".into(),
            line: None,
        },
        from: from.clone(),
    };
    let shown = Screen {
        side_open: true,
        agent_open: true,
        mode: Mode::Source,
        current: None,
        picked: None,
        shown: Some("v".into()),
    };
    pending.follows_agent(1, &shown);
    assert_eq!(pending.from.shown.as_deref(), Some("v"));
    assert!(pending.from.side_open && pending.from.agent_open);
    // What the agent did not touch stays as deferred, so a user's change to it still counts.
    assert!(matches!(pending.from.mode, Mode::Source));
    // A move of another thread does not follow: it drops when its thread goes off screen.
    let mut other = PendingMove {
        thread: 2,
        command: Command::ViewerClose,
        from,
    };
    other.follows_agent(1, &shown);
    assert_eq!(other.from.shown, None);
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

use crate::agent::acp::PermissionOption;

#[test]
fn file_matches_rank_names_before_paths() {
    let files = vec![
        "src/none.txt".to_string(),
        "needle/lib/a.rs".to_string(),
        "needle.rs".to_string(),
        "deep/needlemod.rs".to_string(),
        "src/Needle-ish.rs".to_string(),
    ];
    // Name hits first (shorter names first), then path-only hits, each in walk order.
    assert_eq!(
        match_files(&files, "needle"),
        vec![
            "needle.rs",
            "deep/needlemod.rs",
            "src/Needle-ish.rs",
            "needle/lib/a.rs"
        ]
    );
    assert!(match_files(&files, "needs").is_empty());
    // An empty query lists everything in walk order.
    assert_eq!(match_files(&files, ""), files);
}

#[test]
fn elixir_files_are_named() {
    assert_eq!(language("lib/app.ex"), "Elixir");
    assert_eq!(language("lib/app.exs"), "Elixir");
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
fn a_parent_never_told_is_told_now_and_one_just_told_waits() {
    use std::time::Duration;
    assert_eq!(notice_wait(None), None);
    assert_eq!(notice_wait(Some(NOTICE_RATE)), None);
    assert_eq!(
        notice_wait(Some(NOTICE_RATE + Duration::from_secs(1))),
        None
    );
    assert_eq!(notice_wait(Some(Duration::ZERO)), Some(NOTICE_RATE));
    assert_eq!(
        notice_wait(Some(Duration::from_secs(4))),
        Some(NOTICE_RATE - Duration::from_secs(4))
    );
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
fn text_after_a_run_ends_it_while_the_turn_goes_on() {
    use crate::model::chat::Phase;
    let blocks = vec![
        tool("a", Step::Done),
        tool("b", Step::Done),
        Block::Text("x".into()),
    ];
    assert_eq!(run_row(&blocks, true).0, Phase::Done);
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
fn only_a_question_or_an_unseen_turn_needs_you() {
    assert!(Activity::Asking.needs_you(false) && Activity::Unread.needs_you(false));
    assert!(!Activity::Running.needs_you(false) && !Activity::Idle.needs_you(false));
    // A child's parent picks up its finished turn; a question is still the user's.
    assert!(!Activity::Unread.needs_you(true) && Activity::Asking.needs_you(true));
}
