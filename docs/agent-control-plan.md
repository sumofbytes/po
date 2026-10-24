# Plan: an agent-drivable Po

Status: draft, revision 2 · 2026-10-24. Revised after review. §10 lists what changed and why.

Implementation: phases 0–4 are in. Phase 0 brought peer-pid and process-chain authentication
with `Caller::User` (`control.rs`), `Scope`/`Grant` with the reach table and its test
(`command.rs`), the agent's pid taken at spawn (`acp.rs`, `agent.rs`), `run_command` split by
family with `authorize` and per-command timeouts, the `Outgoing`/`Origin` refactor through
`submit` (`main.rs`), and a concurrent `po mcp` (`mcp.rs`). Phase 1 brought `state.rs` with
the `ThreadState` derivation and the snapshots, the nine read commands with their reach, the
Control menu with the grant indicator and Stop agent control (`main.rs`), and `control.info`
at `initialize` with the tool list filtered by grant (`mcp.rs`). Phase 2 brought the
orchestration commands (`thread.create`, `thread.send`, `thread.cancel`, `thread.set_option`,
`thread.close`, `thread.wait`), notify-parent with 2 s coalescing and a 10 s rate limit,
`observed`/`changed` with the `Answer::Later` waiter machinery and `MAX_WAITS`, `MAX_CHILDREN`,
the child `↳` markers and the parent's children status line, and `overrides`/`api_options`
(API-set settings are applied but never remembered). One deviation from the plan's letter:
overrides live on `Thread` rather than in `Applying`, and notify-parent deliberately does not
quote the child's text (§9's safe answer) — the parent reads it with `last_reply`. Still open
from the plan's test lists: the Reader-level behaviours (create with an unknown agent, send to
an asleep child, cancel returning dropped text, coalescing and wait wake-ups) need a window to
test and are covered by the pure pieces (`route`, the reach table, `ThreadState`) plus a manual
check; `thread.answer` (phase 4) and `notifications/cancelled`'s deeper cancel path are later.
Phase 3 is in: `project.open` (a `path`, or a `pr`), `project.close`, `project.offer`,
`thread.create_isolated` and `worktree.merge`, all Global (`command.rs`, `run_project` and
`create_isolated` in `main.rs`, five tools in `mcp.rs`). `project.open` returns an already-open
project; a new folder or a PR is put to the user as a suggestion in the sidebar ("Open", "Check
out", "Not now"), and the thread that asked is told what they chose. `project.offer` is the same
row with "Go", and `worktree.merge` the same with "Merge" (discarding stays the user's). Deviation
from §5/§7: these are sidebar rows rather than cards in the orchestrator's thread, which avoids a
new `Block` and keeps them visible when that thread is not on screen. `project.close` only closes a
project the API opened and only when it holds no used thread without a parent. Folders, PRs and
worktrees are added as slots by `add_slot`, which never swaps the screen; `Pr::open` and
`Worktree::create` run off the UI thread. `thread.create_isolated` replies
`{creating, request}`, then tells the parent `{thread, project}` or the error; one is made at a
time (`isolating`), and it is refused while a merge or discard (`finishing`) runs. Still open:
tests of the window-side pieces, which need a window.

Phase 4 is in: `thread.answer` (Orchestrate plus `approvals`, children only for a thread; any
thread for the user; it matches `Block::Permission` alone), `thread.find`, `window.info`
(`Family::Window`, user only), tools for the first two, `src/ctl.rs` for `po ctl`, and
`parent_session` saved in `workspace.rs` and relinked by `relink_children` on restore. `po ctl`
speaks as `Caller::User`, so it reads, answers, stops, closes and sets options on any thread, but
`thread.create`, `create_isolated`, `send` and `wait` are refused for it (`needs_parent`): they
act as a parent, and the user's connection has none. `po ctl wait thread=N [until=…]` polls
`thread.get` instead, with no 50 s cap. Windows are found by their sockets; one that refuses the
connection is removed. Not tested: a child relinked in a live window (needs one), and the
refusal from inside an agent's tree beyond the existing `classify` test. Phase 5 is in except two items. The audit log keeps the control commands that change something,
and any that were refused, in memory (the last 500, `Reader.audit`) with caller, name, outcome and
a count of identical repeats; answered reads are not kept, so a caller can't push the rest out.
`audit.list` (user only, so `po ctl audit.list limit=20`) reads it, and `events.subscribe` is
audited too. There is no UI view yet.
`events.subscribe` (user only) turns the connection into a stream: the first reply says
`subscribed`, then every `changed` publishes a `thread.state` line (`thread`, `state`, `title`,
`project`, `at`); `po ctl watch` prints them, and a subscriber that hung up is dropped at the next
event (`control::answer_stream`, tested). Token usage is read from ACP's `usage_update` session
update into `Thread.usage` and shown as `usage: {used, size}` in `ThreadInfo`; an agent that
doesn't send it shows `null`, and cost is not read. Not done: named-pipe control on Windows (no
Windows toolchain here to build or test it), and control for dex threads: Po creates a dex session
with `POST /api/sessions {cwd}` and the client has no way to hand the daemon an MCP server, and
the daemon may be remote, out of reach of the unix socket. That needs a change to the dex
daemon's protocol first.

## 1. Goal

An agent running in a Po thread (an "orchestrator"), or a script outside Po, can drive Po:

- open projects
- start threads with a chosen agent and settings
- send them prompts
- hear when they finish or ask
- read what they did
- put things on screen

The user stays in charge. An agent can do only what the user granted. Anything that widens
trust, can't be undone, or takes over the screen needs a click.

Non-goals:

- Approving an agent command or an MCP server through the API. That stays a click in the UI.
- Control from Windows (named pipes) and from dex-daemon threads. Both are listed as later.
- Remote access. The socket stays local, in a directory only the user can enter.

### 1.1 Threat model

Agents run as the user, with a shell. They can read other processes' environments under
`/proc` and edit `config.json`, `workspace.json` and the agent approvals (`config.rs:226`).
That is why:

- **Scopes are guardrails, not a sandbox.** They stop a confused or prompt-injected model from
  doing damage *through Po's tools*. They do not stop an agent that sets out to escape through
  its shell. That agent already has the user's whole account. The docs and the Control menu say
  this plainly.
- **Within that limit, nothing an agent can read or write may raise its own reach.**
  - Grants are held in memory and set only from the UI. They are not read from files the agent
    can write.
  - Callers are identified by their process, not only by a secret.
  - The `agent` argument only names agents already configured.

## 2. Where we are

```
agent ──stdio MCP──▶ `po mcp` (src/mcp.rs) ──unix socket + PO_TOKEN──▶ control.rs
      ──▶ Command::parse (src/command.rs) ──▶ Reader::run_command (src/main.rs:1844)
```

- There are 6 commands: `view.show`, `view.update`, `view.close`, `file.open`, `diff.show` and
  `viewer.close`. The only caller is `Caller::Thread(id)`, and it acts on its own thread.
- Replies are synchronous. The pump at `main.rs:1170` sends whatever `run_command` returns, and
  `control::answer` waits at most 10 s.
- `po mcp` handles one request at a time (`mcp.rs:140`). The socket allows 16 connections and
  drops any beyond that without answering (`control.rs:76`).
- Tokens travel in the environment of `po mcp`, so any process of the user can read them from
  `/proc/<pid>/environ`.

Most of the building blocks exist as UI methods on `Reader`, but they assume the thread is on
screen and that the text comes from its draft. §4.3 lists each place where that matters.

## 3. Design principles

1. **One vocabulary.** `Command` stays the only list of things Po can be asked to do. MCP tools
   and `po ctl` both map onto it. `command.rs` and the new `state.rs` stay free of GPUI.
2. **The window is the authority.** Callers get handles and snapshots, and every write runs on
   the UI thread.
3. **Ownership, not adjacency.** An agent changes only threads it started. Reading the user's
   other threads is a separate opt-in.
4. **Messages carry their origin.** Text an agent sends is labelled as coming from that agent at
   every step: when it is sent at once, waits for the agent to be ready, sits in the queue, or is
   cancelled. It never ends up in the user's draft, and it never claims "the user has X open".
5. **The screen is the user's.** API work never switches the screen. Anything on-screen is
   offered.
6. **Push, then poll.** By default an orchestrator hears from its children as a message in its
   own thread ("notify parent"). A blocking wait is a short fallback.
7. **Nothing the API does changes the user's defaults.** That covers the remembered model and
   other choices, and the agent new threads start with.

## 4. Architecture

### 4.1 Callers and authentication (`control.rs`)

```rust
pub enum Caller {
    Thread(u64), // a `po mcp` started under that thread's agent
    User,        // `po ctl`, run by the user outside any agent
}
```

**A thread is identified by token and process.**

- `Control::register(id)` stays, and becomes `register(id, agent_pid)` once the agent is
  spawned. `acp.rs:363` has the child, and `child.id()` gives its pid.
- On each connection, the server reads the peer's pid:
  - Linux: `SO_PEERCRED`
  - macOS: `LOCAL_PEERPID`
- The server walks the peer's parent chain:
  - Linux: `/proc/<pid>/stat`
  - macOS: `proc_pidinfo`
- The token only counts if the chain reaches the pid registered for that token. A token taken
  from another thread's environment is useless from outside that thread's agent.
- If the peer's pid can't be read, the request is refused. Viewer tools keep the old token-only
  check behind a cfg, so they still work on a platform that lacks both.

**The user is identified by process alone.**

- There is no token file.
- A connection with no token is `Caller::User` only if:
  - the peer's uid is ours
  - none of the peer's ancestors is an agent process Po spawned (`Reader` keeps the set of
    pids)
- An agent running `po ctl` in its shell is refused.

### 4.2 Grants (`command.rs`, `main.rs`)

```rust
pub enum Scope {
    Viewer,      // today: its own views, the viewer while on screen, its own state and options
    Orchestrate, // also start, steer, read, wait on and close threads it started
    Global,      // also open, close and offer projects
}

pub struct Grant {
    scope: Scope,
    read_project: bool, // read (never write) any thread of its own project
    approvals: bool,    // answer permission asks of threads it started
}
```

- `Reader.grants: HashMap<u64, Grant>` is held **in memory only**, so it is lost on restart and
  is not shared between windows. A thread without an entry gets
  `Grant { Viewer, false, false }`.
- The grant can only be changed by:
  - the Control menu in the composer (§7)
  - `thread.create`, which gives a child `Viewer`, always
- Lowering a grant takes effect on the next request and cancels the thread's waiters. Raising it
  takes effect when the agent next starts, because the agent's tool list is fixed at start
  (§4.8). The menu says "applies when the agent restarts", and offers a restart if the thread is
  idle.
- **Reach:**
  - A thread acts on itself, and on threads whose `parent` is itself. A child is always started
    with `Viewer`, so depth stays at 1 by construction. Children of children can't exist, and
    neither can wait cycles.
  - `read_project` adds reads of any thread in the caller's project, including the project's
    worktrees.
  - `Caller::User` reaches everything, and has `approvals`.
- **Never through the API, at any scope:**
  - approving an agent command or an MCP server (`Block::Approve`)
  - discarding a worktree
  - switching the screen

`Command::scope()`, `Command::target()` and `Command::family()` are pure. A table-driven test
checks every command against every grant and every target relation:

| Target relation | Meaning |
|---|---|
| self | the caller's own thread |
| child | a thread the caller started |
| sibling | another thread in the caller's project |
| other project | a thread in another project |

### 4.3 Outgoing text with its origin (`main.rs`)

This is the refactor every write in phase 2 depends on.

```rust
enum Origin { User, Thread { id: u64, title: String }, Po }   // Po = a notification (§4.5)
struct Outgoing { text: String, origin: Origin }
```

1. **`submit(out: Outgoing, later: bool)`** replaces the body of `send`. It covers today's
   paths, and none of them touch the draft:
   - **Blank or unstarted:** `start_thread(out, later)`. Today that path calls `self.send`
     again and reads the draft a second time (`main.rs:2506`). The rewrite passes `out` along.
   - **Starting, loading, or woken from sleep** (`picking`, `restore`, `loading`, `applying`,
     or not `ready`): the text goes to `pending: Option<Outgoing>`, or behind it in the queue
     if one is already pending.
     - This fixes sending to a sleeping thread. Today it falls through to `dispatch` and fails
       with "not ready" (`main.rs:3337`, `3421`).
     - `send_pending` already runs on `Ready` for a resumed thread (`main.rs:3000`).
   - **Busy:**
     - A dex agent gets `enqueue` (steer, or followup with `later`).
     - Any other agent's text goes to `queue: Vec<Queued>`, and `Queued` gains
       `origin: Origin`.
   - **Ready and idle:** `dispatch(out)`.

   `send` reads and clears the draft, then calls `submit` with `Origin::User`.
2. **`send_to_agent(&Outgoing)`** chooses the prefix from the origin:

   | Origin | Prefix |
   |---|---|
   | `User` | today's `[The user has `x` open in the viewer.]`, only for the thread on screen |
   | `Thread` | `[From thread "title" (#id), sent through Po.]` |
   | `Po` | `[Po]` |

   `pending` and `Queued` keep the `Outgoing`, so the label survives every deferral.
3. **`restore_queue`** (on stop, fail and exit, `main.rs:3415`) moves only `Origin::User` items
   into the draft. Items from an agent are dropped and reported to their sender, as a
   notification in phase 2 or as `dropped` in `thread.get`.
4. **Settings stay per thread.**
   - `Applying` gains `overrides: HashMap<String, String>`, and `apply_next` (`main.rs:2373`)
     reads them before `self.choices`.
   - An option set through the API goes into `Thread.api_options: HashSet<String>`. When
     `OptionSet` arrives (`main.rs:2978`), an id in that set is not saved to `choices` or
     `config`.
   - `start_thread` stops setting `chosen_agent` (`main.rs:2488`) unless the origin is `User`.
   - Resumed sessions skip `apply_choices` (`fresh == false`, `main.rs:2998`), so they apply
     `overrides` alone after `Ready`.

### 4.4 Change detection in one place

A thread's observable state changes from many places:

- `on_event`
- `begin_turn`
- `stop` (`3458`)
- `answer` (`3103`)
- the handshake watchdog's `fail` (`2293`)
- `close_thread`
- `sleep` and `enforce_cap`

Rather than hook each one, Po observes the state around every change:

```rust
fn observed<R>(&mut self, id: u64, work: impl FnOnce(&mut Self) -> R) -> R
// snapshot ThreadState (§4.6) of `id` before and after `work`; on a change, call `changed(id, before, after)`
```

- `with_thread` calls go through `observed`. That covers events, which already arrive through
  `with_thread` (`main.rs:2309`), and the API's own writes.
- `close_any_thread`, `close_thread`, `sleep` and `enforce_cap` report `Closed` or `Asleep`
  explicitly.
- `changed` feeds notify-parent (§4.5) and the waiters (§4.7).

### 4.5 Notify parent (the default way an orchestrator hears back)

`thread.create { notify: "done" | "all" | "never" }` has the default `"done"`.

- When a child's state changes to `done` (a turn ended), `asking`, `failed` or `closed`, Po
  sends an `Outgoing { origin: Po }` to the **parent**:

  ```
  [Po] Thread #7 "fix flaky test" finished: <preview, ≤ 200 chars>. Use get_thread or last_reply for more.
  ```

  `"all"` also sends `running` and `asking-answered`.
- **Coalescing:** changes that happen within 2 s go into one message. A parent that is busy gets
  that message through `submit`, which queues it as in §4.3, so it lands after the current turn
  ends. A parent that is asleep is woken by it.
- **Cost guard:** each notification turn costs tokens. Notifications to a parent are limited to
  1 message per 10 s. More frequent ones are merged into the next.
- When a child is asking, it still raises a desktop notification for the user (`main.rs:3014`).
  When a child finishes a turn, it does **not** notify the desktop while its parent gets the
  message.

This removes most of the long-poll machinery from the main path. It needs nothing new from the
socket, and agents handle "a message arrived" naturally.

### 4.6 Read model (`src/state.rs`, new, no GPUI)

```rust
pub enum ThreadState { Starting, Approval, Ready, Running, Asking, Failed, Asleep, Closed }
pub struct ThreadInfo {
    id, project, title, agent, state: ThreadState, unread, busy, status,
    last_error: Option<String>, session, preview, queued: usize, dropped: Vec<String>,
    asking: Option<AskInfo>, parent: Option<u64>, children: Vec<u64>, active_at, on_screen,
}
pub struct AskInfo     { title, options: Vec<{ id, name, kind }> }   // Block::Permission only
pub struct OptionInfo  { id, name, category, current, choices: Vec<{ value, name }> }
pub struct AgentInfo   { id, name, trusted, daemon }
pub struct ProjectInfo { id, root, name, branch, dirty, pr: Option<u64>, worktree_of: Option<u64>,
                         on_screen, threads: usize }
pub struct Entry       { index, role, text, tools: Vec<{ title, status }>, plan: Option<…> }
```

- `ThreadState` is derived once in one function and tested:
  - `Approval` is the `Block::Approve` card, waiting for the user.
  - `Asking` is `Block::Permission`.
  - `Failed` comes from `fail()` and `Stopped`/`Exited`.
  - `Asleep` is `Thread::asleep`.
  - `Starting` is a live client that isn't `ready` yet.

  It lets an orchestrator tell "done" from "crashed" and from "waiting for the user", which
  `Activity` can't do.
- **Transcript cursors:** a `thread.messages` cursor is `"<generation>:<index>"`. `connect`
  clears messages (`2248`), and `return_pending` truncates them (`2516`). A stale generation
  replies `reset: true` and starts from 0.
- **Results:**
  - `thread.last_reply` returns the last agent message's text, which is what an orchestrator
    usually wants.
  - `project.changes` returns the changed files with counts, and `diff` for one file.

### 4.7 Waiting (fallback)

`thread.wait { threads?, until?, timeout? }` exists for agents or scripts that would rather
block.

- **Limits:**
  - The timeout defaults to 25 s and is capped at 50 s, under Codex's 60 s tool limit.
  - A caller can have at most 2 waiters.
  - The socket gets a separate budget for waits (`MAX_WAITS = 8`) beside `MAX_CONNECTIONS`, so
    waits can't push out the viewer tools.
- **The window replies later.** `run_command` returns `Answer::Now(Reply)` or
  `Answer::Later`. For `Later`, the pump (`main.rs:1170`) moves `request.reply` into a
  `Waiter`. `control::answer` and `call_at` take their timeout from `Command::timeout()`.
- **Waking:** `changed` (§4.4) replies to each matching waiter. A timer drops expired ones with
  `timed_out: true`.
- **Cancelling:** a caller's waiters are dropped with `cancelled` when its turn stops, fails or
  exits, or when its grant is lowered. A waiter whose receiving side is gone is dropped on the
  next sweep.
- **`po mcp` becomes concurrent.** Each `tools/call` runs on its own thread, and writes to
  stdout go through a mutex. `ping` and `notifications/cancelled` are answered at once. A
  cancelled call closes its socket connection, and the waiter is dropped.
- **No cycles.** Waits only target self (refused) or children. Children are `Viewer`, so they
  can't wait on anything.

### 4.8 MCP server (`mcp.rs`)

- At `initialize`, `po mcp` asks the window for `control.info`, which returns the grant and the
  thread. It then lists only the tools that grant allows, and adds a paragraph per scope to
  `instructions`.
- The list is fixed for the agent's lifetime, so there is no `tools/list_changed`, and client
  support for it varies anyway. A tool that the grant no longer allows is refused at call time.
- The tool table carries each tool's `Scope`. The existing test (every tool names a command Po
  reads) also checks that the tool's scope matches `Command::scope`.

### 4.9 Many windows and persistence

Each window is its own process with its own socket. The workspace file is shared, and windows
merge it on save.

- **Ids:** a thread's `parent` and `children` are per-process ids.
  - Saved state records `parent_session: Option<String>` instead, as new hand-written fields in
    `workspace.rs` `ThreadState` (it is not serde, `workspace.rs:89`).
  - On restore, a child is relinked to a parent in the same window whose `session` matches.
  - Grants are never saved, so a restored orchestrator is back to `Viewer` until the user
    raises it again.
- **`thread.find { session }`** lets a script or a restarted orchestrator get a thread's
  current id back.
- **`po ctl` discovery:**
  - It lists `*.sock` and connects to each one. A socket that refuses the connection is removed;
    it doesn't rely on the pid, which can be reused.
  - It asks each window for `window.info`.
  - `--window <pid>` picks a window. Otherwise it uses the window that has the cwd as a
    project, and with exactly one window, that one.

## 5. Command catalog

Command names are `noun.verb`, and MCP tool names are snake_case. **P** is the phase that adds
the command. **Reach** uses the target relations from §4.2: self, child, sibling (requires
`read_project`), all.

### Reads

| Command | Args | Reply | Scope | Reach | P |
|---|---|---|---|---|---|
| `control.info` | none | `{grant, thread, window}` | Viewer | self | 1 |
| `agent.list` | none | `[AgentInfo]`, `default` | Viewer | none | 1 |
| `project.list` | none | `[ProjectInfo]` | Viewer | own project; all at Global | 1 |
| `project.changes` | `project?`, `path?` | file list, or one file's diff | Viewer | own project | 1 |
| `thread.list` | `project?`, `state?` | `[ThreadInfo]` | Viewer | self + children (+ siblings) | 1 |
| `thread.get` | `thread?` | `ThreadInfo` | Viewer | self, child, sibling | 1 |
| `thread.options` | `thread?` | `[OptionInfo]` | Viewer | self, child | 1 |
| `thread.messages` | `thread?`, `cursor?`, `limit?`, `full?` | `{entries, next, reset?}` | Viewer | self, child, sibling | 1 |
| `thread.last_reply` | `thread` | `{text, state}` | Viewer | self, child, sibling | 1 |
| `thread.find` | `session` | `{thread}` | Viewer | as `thread.get` | 4 |
| `window.info` | none | `{pid, projects, on_screen}` | User | none | 4 |

### Orchestration

| Command | Args | Reply | Scope | Reach | P |
|---|---|---|---|---|---|
| `thread.create` | `prompt` (required), `agent?`, `options?`, `notify?` | `{thread, state}` | Orchestrate | own project | 2 |
| `thread.send` | `thread`, `text`, `later?` | `{state: sent\|pending\|queued}` | Orchestrate | child | 2 |
| `thread.cancel` | `thread` | `{dropped: [text]}` | Orchestrate | child | 2 |
| `thread.set_option` | `thread?`, `id`, `value` | `{pending: true}` | Viewer (self) / Orchestrate | self, child | 2 |
| `thread.close` | `thread` | `{}` | Orchestrate | child | 2 |
| `thread.wait` | `threads?`, `until?`, `timeout?` | `{thread, state, timed_out}` | Orchestrate | children | 2 |
| `thread.answer` | `thread`, `option` | `{}` | Orchestrate + `approvals` | child | 4 |

`thread.create`:

- **`prompt` is required.** A promptless thread would be `blank`, and `parked.retain(worth_keeping)`
  drops it silently (`main.rs:1575`, `2616`). It is also never saved (`2789`).
- **`agent`:**
  - It must be an id from `agent.list`. Anything else is refused. Today `connect` turns an
    unknown key into a command line (`main.rs:2261`).
  - It defaults to the caller's agent.
  - An untrusted agent or unapproved MCP servers give `state: "approval"`. The user approves in
    the UI, and the parent is notified when the thread starts.
- **`options`:** these become `Applying.overrides`, and are checked against `choices` once
  `Options` arrives. They are never remembered (§4.3). The prompt waits in `pending` until the
  options are applied, as today.
- **Where the thread goes:** it is parked in the caller's project, not shown, and marked with
  its parent (§7). Its grant is `Viewer`.
- **Limits:** a parent can have at most `MAX_CHILDREN` (4) live children.
  - `max_live` defaults to 8 (`main.rs:1145`). With the orchestrator plus 4 children,
    `enforce_cap` has room for the user's own threads.
  - When a child is put to sleep, the parent is told (`Asleep`), and its next `thread.send`
    wakes it (§4.3).
- **Worktrees:** `isolate` is *not* in phase 2 (see `thread.create_isolated`).

Other orchestration commands:

- **`thread.send`:** refused to self. Origin `Thread`.
- **`thread.cancel`:** this is `stop()`. Queued text from the parent comes back in `dropped`,
  and it never goes into the draft (§4.3).
- **`thread.set_option`:** `value` must be one of the option's choices. The result arrives
  through `OptionSet`/`Refused`, and the reply only says it was asked for. On a running thread
  the change applies from the next turn.
- **`thread.answer`:** this only ever matches `Block::Permission`. Asks for the user's approval
  are not reachable.

### Projects and worktrees

| Command | Args | Reply | Scope | P |
|---|---|---|---|---|
| `project.open` | `path` or `pr` | `{project}`, or `{confirm: "asked"}` | Global | 3 |
| `project.close` | `project` | `{}` | Global | 3 |
| `project.offer` | `project`, `thread?` | `{}` | Global | 3 |
| `thread.create_isolated` | as `thread.create` | `{creating: true, request}` | Global | 3 |
| `worktree.merge` | `project` | `{confirm: "asked"}` | Global | 3 |

`project.open`:

- **A known project** (open in this window, or in the saved workspace) opens in the background
  with no agent.
- **A new path** shows a confirmation card in the caller's thread, built like `Block::Approve`,
  and replies `confirm: "asked"`. The parent is notified when the user answers.
- It refuses paths that aren't directories, and `/` and `$HOME` themselves.
- `pr` checks the PR out off the UI thread with `Pr::open`.

`project.close`:

- It only closes projects the API opened that have no user threads left. Anything else is
  refused.

`project.offer`:

- It never switches. It puts a chip in the live list, "#7 suggests project X", and the user
  clicks to go there. This takes the place of the old `thread.show`/`project.show`.

`thread.create_isolated`:

- It needs a background `open_project` variant that doesn't swap the slot. Today it always does
  (`main.rs:1608`). It also needs worktree creation to stop returning silently while a merge is
  `finishing` (`main.rs:1624`, `2485`); it can queue behind it, or refuse with a reason.
- The worktree is made asynchronously, so the reply holds a request id. The parent is notified
  with `{thread, project}` once it exists, or with the error.

### Existing commands

`view.show`, `view.update`, `view.close`, `file.open`, `diff.show` and `viewer.close` keep their
behaviour, at scope `Viewer`, reach self.

## 6. CLI: `po ctl`

- `po ctl windows`
- `po ctl <command> [--window pid] [key=value …] [--json '{…}']` prints the reply as JSON. The
  exit code is non-zero on `error`.
- `po ctl wait …` loops `thread.wait` until the condition holds, so a script isn't bound by the
  50 s cap.
- Authentication is by process (§4.1). It is refused from inside an agent.

## 7. UI

- **Control menu** in the composer's settings row, next to the model:
  - It sets Viewer, Orchestrate or Global, plus "may read other threads here" and "may answer
    its threads' permission asks".
  - It shows "applies when the agent restarts" when the grant is raised.
- **Grant indicator:** any thread above `Viewer` shows a small badge in its header and in the
  project list. Clicking it opens the menu. **Stop agent control** lowers the grant to `Viewer`,
  cancels its waiters, and leaves its children running as ordinary threads.
- **Children** show `↳ <parent title>` in the project list and the live list. Their first
  message shows `from <parent>`, and notifications in the parent show `Po`.
- **The parent's header** shows a status line with its children: "3 threads · 1 asking ·
  1 failed".
- **Confirmation cards** (opening a project, merging a worktree) appear in the orchestrator's
  thread. They are built like the existing approval card, `main.rs:5228`.

## 8. Phases

Each phase ships on its own and leaves Po working.

### Phase 0: groundwork (no new commands)

- **`control.rs`:**
  - read the peer's pid and walk its parent chain
  - `register(id, agent_pid)`
  - `Caller::User`
  - per-command timeouts
  - a separate wait budget
- **`acp.rs`/`agent.rs`:** expose the child's pid.
- **`command.rs`:**
  - `Scope`, `Grant`
  - `Command::scope/target/family/timeout`
  - table tests for the existing commands
- **`main.rs`:**
  - `run_command` split by family, returning `Answer`
  - `authorize`
  - `with_project`
  - `observed` and `changed`
  - the `Outgoing`/`Origin` refactor through `submit`, `start_thread`, `pending`, `Queued`,
    `send_to_agent` and `restore_queue`
  - per-thread `overrides`/`api_options`
  - `chosen_agent` set only for the user
- **`mcp.rs`:** concurrent calls.
- Log every control command with `log::info!(caller, command)`.
- Tests:
  - peer-chain check against a forked child
  - `submit` on each path: blank, starting, asleep, busy (ACP and dex), and ready
  - `restore_queue` keeps only the user's text
  - an override is applied but not remembered

### Phase 1: reads, grants UI

- `state.rs`, with `ThreadState` derivation and the snapshots.
- Every read command marked P1.
- The Control menu, the grant indicator and **Stop agent control**. Cross-thread reads never
  ship without a visible way to revoke them.
- `control.info` at `initialize`, and a tool list filtered by grant.
- Tests:
  - `ThreadState` derivation (approval vs asking, failed, asleep)
  - cursor reset after `connect`
  - reach by target relation
- Manual check: give an Orchestrate grant, and ask the agent to report on a sibling. It should be
  refused without `read_project` and allowed with it.

### Phase 2: orchestration

- Every orchestration command except `thread.answer`, plus notify-parent with coalescing and the
  rate limit, plus `thread.wait`. A user waits less when phase 2 ships notify and wait together,
  and agents never need to poll.
- `MAX_CHILDREN`, and the child and parent UI markers.
- Tests:
  - create with options that override carried choices
  - create with an unknown agent id, which is refused
  - an untrusted agent, which gives `approval`, and the parent is notified when the user
    approves
  - send to an asleep child, which wakes it and sends
  - cancel returns the dropped text
  - notifications coalesce and are rate limited
  - wait wakes on `changed`, times out, and is cancelled when the caller stops
- Manual check: "start three threads on these three bugs with different models, tell me when
  they're done, then summarize their diffs".

### Phase 3: projects and worktrees

- The background `open_project` variant, `Pr::open` off the UI thread, and worktree creation
  that doesn't need the screen or the `finishing` flag.
- `project.open`, `project.close`, `project.offer`, `thread.create_isolated` and
  `worktree.merge`, with their confirmation cards.
- Tests: known vs new path, refused paths, close refused while the user has threads, an isolated
  create reports through notify.

### Phase 4: approvals, CLI, ids across restarts

- `thread.answer` behind `approvals`.
- `po ctl`, `window.info`, `thread.find`, and saving and relinking `parent_session`.
- Tests:
  - CLI argument parsing to a request
  - refused from inside an agent's process tree
  - stale sockets removed by a failed connect
  - a child relinked to its parent after a restart

### Phase 5: later, if needed

- `events.subscribe` streaming and `po ctl watch`.
- An audit view of the logged control commands.
- Cost and token usage per thread, if agents report it over ACP.
- Named-pipe control on Windows (peer pid through `GetNamedPipeClientProcessId`).
- Control for dex threads.

## 9. Risks and open questions

- **Prompt injection.** A child reads hostile content and reports it to its parent, so the
  parent's next turn contains it. The `[From thread …]` / `[Po]` labels, ownership-only writes,
  children capped at `Viewer`, and no API that approves anything all limit the damage. They
  can't prevent it. Should notify-parent messages quote the child's text, or only say "finished"
  and make the parent call `last_reply`, which keeps the text out of a message the user didn't
  send?
- **Token cost of notify-parent.** Every notification is a parent turn. The 10 s rate limit and
  coalescing bound it. Should `"done"` notify only when *all* children are done, as an option?
- **Walking the process chain.** An agent that daemonizes (double-forks) its MCP server breaks
  the chain, and that agent loses Po's tools rather than gaining anything. Check this with each
  agent in `agents.json`.
- **Swapping state in `with_thread`.** Every phase-2 command runs on a background thread through
  the swap. §4.3 lists the fields that belong to the screen and are known today (the draft,
  `project.current`, focus, `chosen_agent`). Each new command needs the same check, and its test
  must run against a thread that is not on screen.
- **`MAX_CHILDREN = 4` against `max_live = 8`.** Should children count against a budget of their
  own instead?
- **Unread state.** A child's finished turn stays unread for the user even after the parent
  read it. Is that right, or noise?

## 10. Changes from revision 1

| Review finding | Change |
|---|---|
| A token file and `/proc` environments let any agent act as the user or as another thread | Peer-pid plus process-chain authentication, no token file, grants in memory only; threat model stated (§1.1, §4.1) |
| `Project` scope reached the user's own threads | Writes only to own children; sibling reads opt-in; children always `Viewer`, depth 1 by construction (§4.2) |
| The origin was lost via `start_thread`, `pending`, `Queued`; queued text leaked into the draft on cancel | `Outgoing { origin }` through every path; `restore_queue` keeps only user text (§4.3) |
| A thread created without a prompt was dropped by `worth_keeping` | `prompt` is required (§5) |
| `isolate` swapped the screen, failed silently while `finishing`, and was asynchronous | Moved to phase 3 as `create_isolated` with a reply that arrives later (§5) |
| Sending to a sleeping child failed | `submit` puts the text in `pending` while the thread wakes (§4.3) |
| The single-threaded `po mcp`, the 16-connection limit, a 60 s wait over Codex's limit, and missed wake-ups | Notify-parent is the default; waits capped at 25/50 s with their own budget; concurrent `po mcp`; `observed` change detection (§4.4, §4.5, §4.7) |
| `Activity` couldn't tell approval, failure or sleep apart | `ThreadState`, `last_error`, `AskInfo` from `Permission` only (§4.6) |
| `agent` could be any command | Configured ids only (§5) |
| API-set options changed the user's defaults | `overrides` and `api_options`, never remembered; `chosen_agent` untouched (§4.3) |
| Phase order: reads before revoke, create before wait | The grant UI ships with phase 1; notify and wait ship with phase 2 (§8) |
| `thread.show` and `project.open` took over the screen | `project.offer` chip; confirmation for new paths (§5) |
| Origin and grants in `ThreadState` | `parent_session` saved, grants never saved; JSON is hand-written, not serde (§4.9) |
| Gaps | `last_reply`, `project.changes`, `thread.find` added; usage deferred (§5, §8) |

## 11. Changes after review of the implementation

A fresh review of the implementation (PR #79) found these, all fixed:

| Finding | Change |
|---|---|
| `relation_to` took any two live threads for siblings | A sibling shares a project (`state::share_project`, tested) |
| A tokenless peer whose process chain could not be read was taken for the user | `classify` refuses a chain that does not reach init; the process table is injectable for tests. An agent that deliberately re-parents itself (double fork) is still outside the threat model of §1.1 |
| A `thread.wait` that ran out answered "Po did not answer" | The server waits 2 s past the command's timeout, so the window answers `timed_out` |
| `Closed` was announced after the thread was removed, so parent and waiters never heard | It is announced first; `po ctl wait until=closed` treats a gone thread as closed |
| `start_isolated` ignored a parent that had gone and the child limit | `start_child` checks both when the worktree is ready |
| A thread could set its own `mode` (permissions) through `thread.set_option` or `options` | The API never sets a `mode` option |
| The audit log could be flushed by reads | Writes and refusals only, repeats counted (§ phase 5) |
| Hung-up clients held connection slots; a request could be dripped in | Hang-ups are detected while waiting or streaming; a request is read within 10 s and a size limit; `po mcp` serves at most 16 calls at once |
| Lowering `read_project` or `approvals` kept waits; Stop agent control left child notices; restored `parent_session` was trusted | Any lowering drops waits; children go quiet; relinking keeps depth 1 and `MAX_CHILDREN`. `thread.find` answers the same for a missing session and one out of reach |

## 12. Found by running it

The first run in a real window (a debug build on a scratch repository, driven by a fake ACP agent
that calls the real `po mcp` tools, and by `po ctl`) found four faults the unit tests had missed,
all fixed:

| Fault | Fix |
|---|---|
| `Control` was `Clone` and its `Drop` removed the socket, so the first dropped clone (when an agent was started) deleted the socket file: nothing could reach the window | The file is removed when the last clone goes (`SocketFile`, tested) |
| Thread reads (`thread.list`, `get`, `messages`, `last_reply`, `options`, `find`) were routed to the thread-write handler and refused as "not a thread command" | `run_thread` hands anything it does not write to `run_read` |
| Notify-parent never sent: for a parent never told before, the rate limit computed a wait of zero, took it for a wait, and rescheduled itself for ever at 95% CPU | `notice_wait` treats no wait as none (tested) |
| A thread was named from Po's own notice when the user had said nothing | Po's messages are shown with a `[Po]` mark and skipped for titles |

Two more faults came out of clicking through the UI (a small Wayland virtual-pointer tool and
`wtype` drove the real window), both fixed:

| Fault | Fix |
|---|---|
| The Control menu's two opt-ins ("may read other threads here", "may answer its permission asks") called `set_grant` with the scope fixed at Viewer and the other flag off, so toggling one would have dropped a Global thread to Viewer | They keep the current scope and the other flag |
| The header badge and the composer button built dropdowns with the same element id, so they shared one menu state and anchor, and clicks on the second after the first were swallowed | Each place has its own id |

Opening a PR, with `gh` signed in, turned up a last one, also fixed: `project.changes` (and
`dirty` in `project.list`) read the cached list of changes, which is only filled for a project
that has been on screen. For a PR opened in the background, and for an agent's isolated worktree,
which is never shown, it said "no changes" however much had been done, which would have defeated
"then summarize their diffs". They are now read from the repository when asked (the project on
screen keeps its watched cache for `dirty`).

Seen working, by agent calls through the real `po mcp` and by clicks: grants and the tool list by
scope; every Control menu item (scope, both opt-ins, each logged and visible to the agent through
`control_info`) and Stop agent control; `thread.create` with notify; approvals (`answer_thread`,
an unknown option refused); `thread.wait` timing out as `timed_out` and waking on a state; send,
self-send refused, close; the `mode` option refused; `/` refused; the sidebar rows, with **Open**
(the project is added in the background, the screen stays, the agent is told), **Not now**,
**Go** (the screen switches only then) and **Merge** (the worktree and its project go, the agent
is told "Merged into master"); an isolated worktree thread started and its parent told; `po ctl`
`windows`, `watch`, `wait`, `audit.list`, `thread.find`, and its refusal from inside an agent
("Po refused: …"); a child relinked to its parent after a restart. A PR checkout (`project.open` with `pr`, answered **Check out**) opened as a background project
with its 11 changed files readable, and `close_project` removed it and its temporary checkout.
Not tried: a resumed (asleep) child being woken by `thread.send`.
