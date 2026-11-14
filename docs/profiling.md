# Profiling Po with hotpath

Po is instrumented with [hotpath](https://hotpath.rs). A build with one of the `hotpath`
features times the functions on Po's busy paths, follows its channels and locks, and writes a
report when Po quits. A normal build compiles all of it to nothing, so release binaries are
unchanged.

`PO_TRACE=1` (see `src/dev/trace.rs`) says how long each launch step took. hotpath says which
functions that time went to, how often they ran, and what they allocated, for the whole run.

## Running

```sh
# Timing, channels, locks and threads.
cargo run --release --features hotpath -- -f .

# The same, plus the bytes each function allocates (slower: every allocation is counted).
cargo run --release --features hotpath-alloc -- -f .
```

Use `-f`. A detached Po writes the report to `~/.cache/po/po.log` with the rest of its output,
not to your terminal.

Po writes the report when it ends: when you close the window or quit, with `PO_TRACE=exit`, or
on Ctrl+C or `kill` (it writes the report and exits with status 130, for both). Use a release build for the
numbers: a debug build is several times slower and spends its time differently.

### Useful variables

| Variable | Effect |
|---|---|
| `HOTPATH_OUTPUT_PATH=report.txt` | Write the report to a file instead of stdout. |
| `HOTPATH_OUTPUT_FORMAT=json` | `table` (default), `json`, `json-pretty` or `none`. |
| `HOTPATH_SHUTDOWN_MS=20000` | A timed run: write the report and quit after 20 s. Closing earlier still writes it; 0 is no timer. |
| `HOTPATH_FOCUS=snapshot` | Only report functions whose name contains `snapshot` (`/regex/` also works). |
| `HOTPATH_REPORT=functions-timing,channels` | Only these sections. |
| `HOTPATH_LIMIT=0` | Show every row, not the top 15. |
| `HOTPATH_ALLOC_METRIC=count` | With `hotpath-alloc`, sort by number of allocations instead of bytes. |
| `HOTPATH_METRICS_PORT=6771` | Port of the live metrics server (default 6770; a second profiled window needs its own). |
| `HOTPATH_METRICS_SERVER_OFF=1` | No metrics server, for static reports only. |

## Live view

hotpath's TUI shows the numbers while you use Po. That is the best way to see what scrolling a
long transcript, a streaming reply or a big diff costs:

```sh
cargo install hotpath --features tui
cargo run --release --features hotpath -- -f .   # in one terminal
hotpath console                                    # in another
```

## Recipes

**Launch.** `PO_TRACE=exit` quits after the first frame and writes the report on the way out:

```sh
PO_TRACE=exit HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=/tmp/launch.json \
  cargo run --release --features hotpath-alloc -- -f ~/some/big/repo
```

**Before and after a change.** Run the same scenario on both commits with a fixed
`HOTPATH_SHUTDOWN_MS` and JSON output, then compare the two reports. hotpath warns that the JSON
format may change between its releases, so compare reports from the same hotpath version.

**A streaming reply.** Start a profiled Po, send a prompt that makes the agent write a lot, and
quit when the turn ends. Then compare:
- `Reader::render`, `Reader::chat_snapshot` and `chat::publish` call counts against
  `acp events`: how many frames each event costs.
- `chat::transcript` and `chat::agent_row` call counts: how often NSH rebuilds the transcript
  rather than updating it in place.
- `acp events` *Max queue*: whether the UI thread kept up with the agent.

**File churn.** Run `cargo build` (or `git checkout` a large branch) in a profiled project. Look
at `file events` (the watcher's backlog) and `watch batch`, `po::walk`, `diff::changes` and
`Reader::apply_changes`.

## A scripted run, with nobody at the keyboard

`PO_BENCH=stream:<history>:<chunks>:<ms>` (only in a `hotpath` build) fills the thread on screen
with `<history>` messages of markdown, streams a reply of `<chunks>` chunks, one every `<ms>`
milliseconds, through the code an agent's updates take, writes the report and quits. The window,
the text engine and the snapshots are the real ones; only the agent is fake.

`scripts/bench-stream.sh` runs it for several history sizes, in a scratch config so nothing of
yours is read or saved, and prints one line each:

```sh
cargo build --release --features hotpath-alloc
scripts/bench-stream.sh            # histories 2, 20 and 80
scripts/bench-stream.sh 5 50       # your own
```

It runs on one core and waits for the machine to cool between runs (`CORES`, `COOL_C`); the NSH
engine keeps a core busy for the whole run, and a minute of it on two cores took a laptop to 90°C.
The window is on screen for the half minute each run takes.

Read the columns this way: `frames` is how many times the window was drawn, `transcripts` how
often the whole transcript was built, `agent_rows` how many message rows were built, and
`engine alloc` the allocations of the NSH engine thread, which is where the cost of a streamed
reply is. While a reply streams, the transcript should be built once and `agent_rows` should stay
near the number of chunks: one row, the one being written, per chunk. If it grows with the
history, messages that did not change are being built again.

## What is instrumented

| Area | Measured |
|---|---|
| Launch | `Reader::new`, `po::walk`, `po::start_watcher`, `theme::apply`, `config::load`, `Workspace::load`, `Reader::restore`, `Reader::connect` |
| Each frame | `Reader::render`, `Shell::render`, `TerminalView::render`, `terminal_view::paint`; snapshot builders `Reader::{sidebar,chat,diff,source}_snapshot`; the NSH screens' `publish` (each compares the new snapshot with the one shown) |
| Transcript | `chat::transcript`, `chat::agent_row`, `chat::user_row`, `chat::tool_row`, `po::preview_of`, `po::transcript` (control API) |
| Agents | `Shared::handle` (each ACP message), `acp parse` (its JSON), `Mapper::map` and `Shared::stream_turn` (dex), `Reader::on_event` |
| Files | `watch batch`, `Reader::{apply_changes,refresh_changes,rescan,set_files,open,refresh_tree}`, `diff::{changes,against,status,file_diff,versions,parse}`, `FileTree::{set_paths,rows}` |
| Window state | `Reader::{settle_agent_changes,pane,explorer,views,thread_info,project_info,children_status,chat_cmd,run,submit,attach,refind,open_selected,build_project}`, `sidebar::project_group` |
| Issues and state | `store::{load,fingerprint}`, `Store::flush`, `shared::{poll,publish,summary}`, `Reader::{workspace,request_save,run_command,publish}`, `Workspace::save` |
| Terminal | `terminal::snapshot` (on the emulator thread) |
| Channels | `acp events`, `dex events`, `file events`, `control requests`, `nsh commands`, `terminal input` |
| Locks | `acp stdin`, `acp pending`, `terminal frame` |
| Threads | Po's own threads are named (`po-walk`, `po-watch`, `acp-read`, `dex-stream`, `control-accept`, `control-serve`, `issues-poll`, `terminal-read`, `terminal-write`), so the threads section can tell them apart. |

`po mcp`, `po ctl` and `po update` are separate processes and aren't profiled.

## Adding more

- Put `#[hotpath::measure]` on a function. On a method, add `impl_type = "Reader"` so the
  report shows `Reader::method`, not just `method`.
- Wrap a block with `hotpath::measure_block!("label", { ... })`.
- Create a channel with `hotpath::channel!(unbounded(), label = "...")`. In a profiled build
  the macro returns hotpath's wrapper types, so name the ends through the aliases that already
  exist: `profile::{UnboundedSender, UnboundedReceiver}` for `futures` channels,
  `hotpath::wrap::std::sync::mpsc` for std, and `nsh::bus::Commands` for the tokio bus.
- Create a lock with `hotpath::mutex!(Mutex::new(..), label = "...")` and give it the type
  `hotpath::wrap::std::sync::Mutex`.
- A channel or lock made in a constructor (a watcher, an agent, a terminal) is a new instance
  each time: the report shows one row per instance (`acp stdin`, `acp stdin-2`, …).
- Labels must be unique within the crate. A repeated one only fails at `cargo build`, not
  `cargo check`.
- Measuring has a cost on each call. Don't measure functions that run per file, per row of the
  file tree or sidebar, or per character; measure what calls them. The transcript's rows
  (`chat::{user,tool,agent}_row`) are measured because a transcript has few of them and a long
  reply makes `agent_row` expensive.
- Check both builds: `cargo clippy --all-targets` and
  `cargo clippy --all-targets --features hotpath-alloc`.
