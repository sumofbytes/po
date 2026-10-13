# po

A native desktop GUI (built on [GPUI](https://crates.io/crates/gpui-pre)) for
coding agents that speak the [Agent Client Protocol](https://agentclientprotocol.com).
Chat with an agent, browse the project's files with a syntax-highlighted viewer,
and review the agent's plans and edit diffs side by side.

## Install

Prebuilt binaries are published for Linux x86_64 and macOS Apple Silicon:

```sh
curl -fsSL https://raw.githubusercontent.com/sumofbytes/po/HEAD/scripts/install.sh | sh
```

- Pin a version: `curl -fsSL .../install.sh | sh -s -- 0.1.0`
- Custom directory: `PO_INSTALL_DIR=/usr/local/bin curl -fsSL .../install.sh | sh`
  (default is `~/.local/bin`)

Or download a tarball from the [releases page](https://github.com/sumofbytes/po/releases).
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
  "theme": "main",
  "font": { "family": "Adwaita Sans", "size": 14, "mono_family": "Noto Sans Mono", "mono_size": 13 },
  "agents": [{ "id": "mine", "name": "My agent", "command": "my-agent --acp" }]
}
```

### Coding agents

Any program that speaks ACP over stdio works. `po` ships with entries for
`opencode acp`, `dex acp`, `exsa-acp` and `claude-agent-acp`
([`agents.json`](agents.json)); install the one you want and it must be on your
`PATH`. Add your own under `agents` in `config.json`: `id` and `command` are
required, `name` defaults to the `id`. An entry whose `id` matches a bundled
one replaces it, so you can change flags or the binary path. `command` is split
on whitespace into program and arguments (no shell quoting). Choose the agent
in the app, or pass `--agent` / `PO_AGENT`.

#### Steering and follow-ups (dex daemon)

ACP has no way to add to a turn that is already running, so with an ACP agent a
message typed while it works is queued and sent when the turn ends. The `dex
(daemon)` entry talks to a [`dex serve` daemon over HTTP
and SSE instead, which can take messages mid-turn:

- **Enter** steers: the agent reads the message before its next model call.
- **Alt+Enter** queues a follow-up that runs after the current turn.
- Queued messages are listed above the input until the agent takes them; the
  **×** takes one back into the input to edit.

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

Widths, the split, whether the explorer is folded, whether the viewer is open and
which way round they are come back with your workspace.

### Projects and threads

Several projects can be open at once. **Add project…** (Ctrl+O, or the **+** in the Projects header)
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

`po` remembers the projects and threads of each window, by the directory it was
started in (in `~/.local/share/po/workspaces/`). Starting `po` there again brings
them back: the project you started on is shown, the others wait in the
background, and every thread resumes its agent session when you first show it.
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

Set `"theme"` to a theme name. On first start `po` copies the bundled themes
(`main`, `moon`, `dawn`, from [`themes/`](themes)) into `<config>/themes/`;
existing files are never overwritten, so edit them freely. A name is looked up
as `<config>/themes/<name>.json`, then `<config>/<name>.json`, then the bundled
theme; an absolute path to a JSON file also works. To make your own, copy one
of the bundled files, rename it, and set `"theme"` to its name. An unknown
theme falls back to `main`. The `font` block overrides the theme's UI and
monospace fonts and sizes.

Markdown, in the agent's replies and in `.md` files (which open rendered; the
**Source** button or Ctrl+Shift+M switches to the text), is coloured from the
theme: headings from the `title` syntax colour, inline code from `string`,
bullets from `punctuation`, links from `colors.link`. An optional top-level
`"markdown"` object overrides any of these with `#rrggbb` colours: `heading`,
`code`, `code_background`, `inline_code_background`, `quote`, `quote_border`,
`rule`, `marker`, `strong`, `emphasis`.

## License

Dual-licensed under [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE), at your
option. Bundled third-party material keeps its own license: `vendor/gpui-pre-wgpu`
(Apache-2.0), `src/viewer/languages` queries (Apache-2.0), and the Lucide icons
in `assets/icons` (ISC).

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
