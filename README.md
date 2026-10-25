# po

A native desktop GUI (built on [GPUI](https://crates.io/crates/gpui-pre)) for
coding agents that speak the [Agent Client Protocol](https://agentclientprotocol.com).
Chat with an agent, browse the project's files with a syntax-highlighted viewer,
and review the agent's plans and edit diffs side by side.

## Install

Prebuilt binaries are published for Linux (x86_64, aarch64) and macOS (Apple Silicon, Intel), and Windows x86_64:

```sh
curl -fsSL https://raw.githubusercontent.com/sumofbytes/po/HEAD/scripts/install.sh | sh
```

- Pin a version: `curl -fsSL .../install.sh | sh -s -- 0.1.0`
- Custom directory: `PO_INSTALL_DIR=/usr/local/bin curl -fsSL .../install.sh | sh`
  (default is `~/.local/bin`)

On Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/sumofbytes/po/HEAD/scripts/install.ps1 | iex
```

`PO_VERSION` and `PO_INSTALL_DIR` work the same way (default is `%LOCALAPPDATA%\Programs\po`).

Or download a tarball (a `.zip` on Windows) from the [releases page](https://github.com/sumofbytes/po/releases).
On Linux the runtime needs Wayland or X11, Vulkan, and fontconfig libraries.
Other platforms: build from source below.

## Build

Requires a stable Rust toolchain (edition 2024). On Linux, install a C
toolchain plus Wayland/X11, xkbcommon, fontconfig and Vulkan development
packages. `.cargo/config.toml` links with `clang` + `mold`; delete it if you
don't have them.

```sh
cargo run --release
```

## Usage

```sh
po [path]                 # open a project (default: current directory)
po --agent "dex acp" .    # pick the agent command for this launch
po --pr 123               # check out a GitHub PR (number or URL) and review it
po -f [path]              # stay in the foreground instead of detaching
```

By default `po` detaches from the terminal, like `code .`; output goes to
`$XDG_CACHE_HOME/po/po.log` (default `~/.cache/po/po.log`). Environment
variables: `PO_AGENT` (default agent command, same as `--agent`), `PO_THEME`
(theme name, overrides the config), `PO_TRACE=1` (print startup timings).
Detaching is Unix-only for now.

## Configuration

Everything lives in `$XDG_CONFIG_HOME/po/` (default `~/.config/po/`). All of it
is optional. `config.json` looks like
[`config.example.json`](config.example.json):

```json
{
  "theme": "Rose Pine",
  "font": { "family": "Adwaita Sans", "size": 14, "mono_family": "Noto Sans Mono", "mono_size": 13 },
  "agents": [{ "id": "mine", "name": "My agent", "command": "my-agent --acp" }]
}
```

### Coding agents

Any program that speaks ACP over stdio works. `po` ships with entries for
the popular ones: Claude Code, Codex, Gemini CLI, OpenCode, Cursor, GitHub
Copilot and Goose ([`agents.json`](agents.json)); install the one you want and
it must be on your `PATH`. Add your own under `agents` in `config.json`, which
is merged over the bundled list: `id` and `command` are
required, `name` defaults to the `id`. An entry whose `id` matches a bundled
one replaces it, so you can change flags or the binary path. `command` is split
on whitespace into program and arguments (no shell quoting). Choose the agent
in the app, or pass `--agent` / `PO_AGENT`.

The first time `po` is about to run a command that isn't one of the bundled ones as
shipped, because it is new in `config.json` or changes a bundled entry, it shows the command
and waits for you to choose **Run it**. Your answer is kept for that exact command in
`approved-agents.json` in `po`'s data directory (next to the worktrees), and changing the
command asks again. A command you pass with `--agent` / `PO_AGENT` is yours, so it is not asked
about; a value that is an agent's `id` picks that agent, and its command is checked as above.

#### MCP servers

`mcp_servers` in `config.json` lists [MCP](https://modelcontextprotocol.io) servers that
`po` hands to every ACP agent session it starts or resumes, so each agent gets the same
extra tools:

```json
{ "mcp_servers": [
  { "name": "fs", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."], "env": { "KEY": "value" } }
] }
```

Stdio servers only. `name` and `command` are required; `args` and `env` (an object) are
optional, and `command` is not split further. An entry missing `name` or `command`, a repeated
`name`, or an `args` or `env` value of the wrong type is logged and skipped. The list is read
when a session starts, so a change applies to new and resumed threads, not to ones already
running. This does not apply to a `daemon` agent.

The agent starts the server, but it runs as you, so a server is treated like an agent command:
the first time one is about to be handed to an agent, `po` shows its command line and waits for
**Run it**. The answer is kept for that command, arguments and `env` (not the `name`), and
changing any of them asks again. A server you decline is left out until `po` is restarted.

#### Letting the agent show you things

Every ACP agent `po` starts is also handed `po`'s own MCP server, named `po` (it is `po`
itself, so it is not asked about). Its tools let the agent put what it is talking about in
front of you:

| Tool | What it does |
|---|---|
| `show_view` | Shows a markdown document as a tab in the viewer |
| `update_view` | Changes one of its views |
| `close_view` | Closes one of its views |
| `open_file` | Opens a file of the project, at a line |
| `show_diff` | Shows the changes, at one file's diff |
| `close_viewer` | Closes the viewer |

The server reaches the window through a socket, with a token for its thread that is passed in
the server's environment, not the agent's, so programs the agent runs don't inherit it. Only the
thread on screen can move the viewer, and only files inside the project can be opened. While you are
typing or scrolling, a move waits until you have been still for a few seconds and is dropped if
you change the viewer yourself first; a new view is offered as a button rather than shown.

#### Steering and follow-ups (dex daemon)

ACP has no way to add to a turn that is already running, so with an ACP agent a
message typed while it works is queued and sent when the turn ends. An entry with a
`daemon` URL, such as
`{ "id": "dex-daemon", "name": "dex (daemon)", "command": "dex serve 127.0.0.1:8420", "daemon": "http://127.0.0.1:8420" }`
in your `config.json`, talks to a [`dex serve` daemon over HTTP
and SSE instead, which can take messages mid-turn:

- **Enter** steers: the agent reads the message before its next model call.
- **Alt+Enter** queues a follow-up that runs after the current turn.
- Queued messages are listed above the input until the agent takes them; the
  **×** takes one back into the input to edit.

The model and mode menus under the input list the daemon's models and the modes
it allows; po sends the choice with each message.

An entry with a `daemon` URL uses this path, and its `command` starts the
daemon if nothing answers at the URL (po stops a daemon it started). Plain
`http` only; set `DEX_DAEMON_TOKEN` if the daemon needs a bearer token.

### Layout

One layout, in three columns:

- **Left:** every project with its threads and their status marks, and below
  them the active project's files (or changes). The line between the two can be
  dragged, and the chevron beside **Explorer** folds it down to its header at
  the bottom, which gives the projects the rest of the height; unfolding puts it
  back where it was.
- **Middle:** the conversation of the thread you are in.
- **Right:** the viewer, for a file or a diff. Open it from the explorer, the
  Ctrl+P finder, or **Open** on a tool call.

Shortcuts are written with Ctrl; on macOS the same keys use Cmd (Cmd+P, Cmd+W to close the window).

The window's own controls sit in the pane headers, at the edges: the sidebar
toggle (Ctrl+B) at the left of the leftmost pane, and the viewer toggle
(Ctrl+Shift+B) at the right of the rightmost one, with **⇄** beside it (Ctrl+Shift+S)
while the viewer is open. **⇄** swaps the viewer with the conversation, putting
the viewer in the middle and the chat on the right, for when you are reading
code more than talking to an agent.

Three shortcuts set how the layout is used:

- **Browse** (Ctrl+1): the viewer open on files. It opens wide, since reading
  code wants room.
- **Review** (Ctrl+2): the viewer open on changes and diffs. This is where a PR
  or a worktree thread's work is read.
- **Chat** (Ctrl+3): the viewer closed, so the conversation has the middle to
  itself.

In a conversation, thinking starts folded behind a **Thinking** row; click it to
read it. Two shortcuts act on the whole thread: Ctrl+O opens every tool
call's details (or folds them all when they are already open), and Ctrl+T
does the same for thinking.

Widths, the split, whether the explorer is folded, whether the viewer is open and
which way round they are come back with your workspace.

### Projects and threads

Several projects can be open at once. **Add project…** (Ctrl+Shift+O, or the **+** in the Projects header)
opens a folder; each project keeps its own files, changes, open file and agent
threads. Opening a PR adds it as a project too, and closing it removes the
checkout.

A project can run several agent threads. **New Thread** (Ctrl+N) opens a blank
one without stopping the others, which keep working in the background; pick
between them in the list on the left. Under the input box, a blank thread shows
the agent it will start with (a dropdown; the last one you used is remembered)
and, in a git repository, a **Worktree** toggle. The agent starts when you send
the first message, which waits for it if it is slow to start. **History** in the
thread's header (Ctrl+Shift+H) lists the earlier sessions of the thread's agent
to resume, newest first; one already open in a thread is shown instead of opened
twice, and a listing you leave without picking is not kept.

A mark on each thread shows what it is doing (`●` running, `!` waiting
for a permission answer, green `●` finished while you were elsewhere), and a
project shows the busiest of its threads. Only the thread on screen moves the
viewer to the files it edits.

#### Worktrees

Threads in one project share its checkout, so two agents editing at once can step
on each other. In a git repository, turning on **Worktree** under a blank thread's input gives
the thread a checkout of its own: a new branch `po/<id>` under
`~/.local/share/po/worktrees/`, shown as a project of its own whose Changes view
is everything the thread did since it branched. Two buttons finish it:

- **Merge** commits what is in the worktree and merges the branch into the
  checkout it came from, then removes the worktree. If the merge conflicts it is
  undone and the branch is kept.
- **Discard** (click twice) deletes the branch and everything in it.

Closing a worktree project with × never loses work: what is left is committed
and the branch is kept if it has anything on it. The worktree starts from the
last commit, so uncommitted changes and untracked files in the original checkout
aren't in it.

#### Coming back, and background agents

`po` remembers your projects and threads (in `~/.local/share/po/workspace.json`),
wherever it is started. Starting `po` again brings them back: the project you
started in is shown, the others wait in the background (windows open at the same
time keep each other's projects and threads when they save; pane sizes are shared,
the last window to save wins), and every thread resumes its agent session when you first show it.
A resumed agent has to support listing and loading sessions. PR reviews aren't
remembered, and worktrees stay on disk until you merge, discard or close them.

Agents nobody is talking to don't have to keep running. A thread in the
background with nothing going on has its agent stopped after `idle_minutes`
(default 15, `0` to never) and is resumed when shown; beyond `max_live_agents`
(default 8) the longest idle are stopped first. A thread mid-turn, waiting for
a permission answer or with messages queued is never stopped. When a thread
finishes or asks for permission out of view, `po` sends a desktop notification
(`notify-send` on Linux, `osascript` on macOS); set `"notifications": false` to
turn that off.

### Themes

Set `"theme"` to a colour scheme name. Over 450 are built in (Catppuccin,
Dracula, Gruvbox, Rose Pine, Tokyo Night and so on); the names are the lines
starting `@@` in [`themes/schemes.txt`](themes/schemes.txt), and case doesn't
matter. The default is `Rose Pine`; an unknown name falls back to it. `po`
derives its UI colours from each scheme's palette. The set is the
[iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes)
collection (MIT).

To make your own, write a theme as JSON (see any scheme converted in
`src/schemes.rs` for the keys) and save it as `<config>/themes/<name>.json`, or
set `"theme"` to the absolute path of the file; a file there wins over a scheme
of the same name. The `font` block overrides the theme's UI and monospace fonts
and sizes. `family` and `mono_family` take a name or a list tried in order
(`["JetBrains Mono", "Fira Code"]`); one that isn't installed is logged and
skipped. Without one, the monospace font is the system's `monospace` (fontconfig
on Linux), then a well-known one for the platform, then the bundled Fira Mono.
`size` also scales the UI's spacing, since layout is measured in units of it.
Edits to `config.json` and to theme files apply without a restart.

Ctrl+, (Cmd+, on macOS) opens a settings page for the theme, fonts, glass,
notifications, idle and live-agent limits, and `agents`. Save writes them to
`config.json`, keeping any other keys in it (`mcp_servers` is still edited there).

Set `"glass": true` for a frosted look like macOS's Liquid Glass: the window
shows the desktop through it, blurred, the panes become translucent tints of the
theme's colours and float apart from each other with a bright rim. A number from
`0.2` to `1` sets how opaque the panes are instead (`true` is `0.72`), and
`PO_GLASS=1` turns it on for one run. The blur is the platform's: macOS, Windows,
and Wayland compositors with KDE's blur protocol. Other Wayland compositors, and
X11 with a compositor running, show the window see-through without blur.

Markdown, in the agent's replies and in `.md` files (which open rendered; the
**Source** button or Ctrl+Shift+M switches to the text), is coloured from the
theme: headings from the `title` syntax colour, inline code from `string`,
bullets from `punctuation`, links from `colors.link`. An optional top-level
`"markdown"` object overrides any of these with `#rrggbb` colours: `heading`,
`code`, `code_background`, `inline_code_background`, `quote`, `quote_border`,
`rule`, `marker`, `strong`, `emphasis`.

## License

Dual-licensed under [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE), at your
option. Bundled third-party material keeps its own license: `vendor/gpui-pre-wgpu`,
`vendor/gpui-pre-apple`, and `vendor/gpui-pre-windows` (Apache-2.0), `src/viewer/languages`
queries (Apache-2.0), and the Lucide icons in `assets/icons` (ISC).

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
