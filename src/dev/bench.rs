//! A scripted scenario for profiling with nobody at the keyboard: `PO_BENCH=stream:20:600:25`
//! fills the thread on screen with 20 messages of history, then streams a reply of 600 chunks, one
//! every 25 ms, through the same event code an agent's updates take, and quits. Built with
//! `--features hotpath`, so the report it writes says what a streaming reply costs. See
//! `docs/profiling.md` and `scripts/bench-stream.sh`, which runs it in a scratch config.
use std::time::Duration;

use serde_json::json;

use crate::files::watch::*;
use crate::reader::state::*;
use crate::session::chat::*;
use crate::session::project::*;
use crate::session::thread::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::agent::Client;
use crate::agent::acp::{ConfigOption, Event, PermissionOption, SessionInfo};
use crate::agent::attach::Attachment;
use crate::agent::dex::Queue;
use crate::files::explorer::FileTree;
use crate::files::watch::Prefetch;
use crate::host::rt::{Context, Task, Window};
use crate::vcs::diff::{Change, Kind, Row};
use crate::{
    agent, config, dev, files, host, ipc, issues, model, reader, rune_ui, session, system, vcs,
};
use futures::StreamExt;
use serde_json::Value;

struct Scenario {
    history: usize,
    chunks: usize,
    interval: Duration,
}

/// `stream:<messages of history>:<chunks in the reply>:<ms between chunks>`.
fn scenario() -> Option<Scenario> {
    let spec = std::env::var("PO_BENCH").ok()?;
    let mut parts = spec.split(':');
    if parts.next() != Some("stream") {
        log::warn!("PO_BENCH: `{spec}` is not stream:<history>:<chunks>:<ms>");
        return None;
    }
    let mut number = |default: u64| parts.next().and_then(|n| n.parse().ok()).unwrap_or(default);
    Some(Scenario {
        history: number(20) as usize,
        chunks: number(600) as usize,
        interval: Duration::from_millis(number(25)),
    })
}

/// Markdown like an agent writes it, with headings, prose, a list and a code block, at least
/// `len` bytes long.
fn markdown(seed: usize, len: usize) -> String {
    let mut out = String::new();
    let mut part = 0;
    while out.len() < len {
        out.push_str(&format!(
            "## Part {seed}.{part}: ownership and borrowing\n\n"
        ));
        out.push_str("Rust's ownership model gives every value a single owner, so the compiler can tell when it is freed and who may touch it meanwhile. A **shared** borrow lets many readers look at a value; a *mutable* borrow lets exactly one writer change it, and never both at once.\n\n");
        out.push_str("- A value has one owner at a time\n- Borrows are checked at compile time, with `&T` and `&mut T`\n- Lifetimes say how long a borrow may live\n\n");
        out.push_str("```rust\nfn longest<'a>(a: &'a str, b: &'a str) -> &'a str {\n    if a.len() >= b.len() { a } else { b }\n}\n```\n\n");
        part += 1;
    }
    out
}

/// Runs the scenario named by `PO_BENCH` in the window's reader, if there is one.
pub fn start(reader: &Entity<Reader>, window: &Window, cx: &mut App) {
    let Some(plan) = scenario() else { return };
    let (reader, handle) = (reader.clone(), window.window_handle());
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        // The window is up and has drawn once before anything is put in it.
        executor.timer(Duration::from_secs(2)).await;
        let history = plan.history;
        handle
            .update(cx, |_, _, cx| {
                reader.update(cx, |reader, cx| {
                    reader.thread.blank = false;
                    for i in 0..history {
                        reader.thread.messages.push(Msg::User(format!("Question {i}: explain part {i} of Rust's ownership model."), vec![]));
                        reader.thread.messages.push(Msg::Agent(vec![Block::Text(markdown(i, 1500))]));
                    }
                    reader.begin_turn(&Outgoing::user("Write a detailed guide to Rust's ownership model.".into(), vec![]));
                    cx.notify();
                })
            })
            .ok();
        executor.timer(Duration::from_millis(500)).await;
        let reply = markdown(9999, plan.chunks * 24);
        let size = reply.len().div_ceil(plan.chunks.max(1));
        let mut at = 0;
        while at < reply.len() {
            let end = (at + size).min(reply.len());
            let chunk = reply[at..end].to_string();
            at = end;
            let update = handle.update(cx, |_, window, cx| {
                reader.update(cx, |reader, cx| {
                    let update = json!({ "sessionUpdate": "agent_message_chunk", "content": { "text": chunk } });
                    reader.on_event(crate::agent::acp::Event::Update(update), true, window, cx);
                    cx.notify();
                })
            });
            if update.is_err() {
                return;
            }
            executor.timer(plan.interval).await;
        }
        handle
            .update(cx, |_, window, cx| {
                reader.update(cx, |reader, cx| {
                    reader.on_event(crate::agent::acp::Event::Stopped("end_turn".into()), true, window, cx);
                    cx.notify();
                })
            })
            .ok();
        executor.timer(Duration::from_secs(1)).await;
        crate::dev::profile::finish();
        cx.update(|cx| cx.quit());
    })
    .detach();
}
