# Porting Po's UI to Rune

Branch `feat/rune`. Rune replaces gpui and NSH, which removes `vendor/`. Rune is pinned to a `dev`
rev in `Cargo.toml`; `dev` is never rewritten, so bump the rev deliberately.

Run it: `cargo run --bin po-rune` (the gpui `po` binary still builds beside it).
`PO_RUNE_SCREEN=settings` opens on the settings page; `-- --snap out.png` renders headless.

## Done

- **Foundation**: `po-rune` opens a Rune window in the configured theme.
- **Theme**: `rune_ui::theme` builds a `rune_kit::Kit` from the same schemes the gpui theme reads.
- **Terminal**: `rune_ui::terminal` draws `terminal::Session` frames as one mono text per row, with
  a span per run. Columns and Nerd Font icons line up. Not yet: resize, scrollback, selection,
  copy and paste, mouse, wide characters, a square cursor.
- **Settings**: `rune_ui::settings` is the page, over the unchanged `settings::Form`.
- **Shell**: `rune_ui::shell` lays out sidebar, main pane and viewer with `SplitView`.
- **Model**: `src/model/` holds what the screens and the `Reader` exchange (chat, sidebar, diff,
  explorer, source, agent and finder snapshots; the bus `Cmd`; header `Control`), with no view
  code. `nsh` re-exports it; the 44 NSH screen tests pass unchanged.
- **Host**: `src/host.rs` is a toolkit-free stand-in for gpui's `cx.notify`, `cx.spawn` and
  `cx.background_executor` (`notify`, `update`, `query`, `spawn`, `background`, `timer`), with
  tests. Nothing uses it yet.

## How the screens connect

Screens only draw a snapshot and report a `Cmd` (`src/nsh/bus.rs`); the `Reader` in `main.rs`
runs the command. That seam is what makes the port tractable.

`Reader` is the hard part: `main.rs` (3.3k lines) and `src/reader/*` (4.8k) are written against
gpui's `Window` and `Context<Self>`. Everything else outside `src/nsh/` is already gpui-free
(`acp`, `agent`, `command`, `control`, `dex`, `issues/*`, `mcp`, `state`, `workspace`, `worktree`).

gpui calls the Reader makes, to be replaced:

| gpui | Uses | Rune |
|---|---|---|
| `cx.notify()` | 98 | `poll` returns true |
| `cx.spawn` / `background_executor` | 44 | a thread, with the result sent over a channel and applied in `poll`; wake the window with the `Waker` |
| `cx.listener` | 22 | `El::on_press` and friends, over `&mut State` |
| `window.window_handle()` | 14 | not needed |
| `Entity<AppView>` panes | ~12 | plain screens (`rune_ui::*`) |

## Remaining

1. **Decouple `Reader` from gpui.** Replace its `window`/`cx` parameters with `host::Host`. The
   `Snapshot` and `Cmd` types are already out (`src/model/`), except home, issues and the
   strips, whose state is signal-based. Do this inside the gpui build first, so each step runs.
   Open question: `Reader` is `Context<Self>`-shaped (`this.update(cx, ..)` in 25 spawned
   tasks); either rewrite those as `host.update`/`host.query`, or put the Reader on its own
   thread behind a channel of `Cmd`s and snapshots.
2. **Port the screens** onto `rune_ui` (NSH lines): sidebar 795, chat 2675, issues 1991,
   home 459, source 275, diff 182, explorer 211, tabs 155, strip 285, finder 238, agent 338,
   epics 324, header 130, overlay 140. Chat is the largest: Rune has `virtual_rows`, `memo`,
   `stick_bottom` scrolling and a `Markdown` component (feature `markdown`).
3. **Window chrome**: titlebar, drag regions, resizable panes (`PaneState` exists), full screen,
   keybindings, drag and drop of files, notifications, `po update`.
4. **Fonts**: replace `src/fonts.rs` (gpui) with Rune's `families` and `AppSpec::fonts`.
5. **Tests**: port the NSH `Headless` tests to `rune::Runtime`.
6. **Cutover**: make `po-rune` the `po` binary, delete `src/nsh`, `terminal_view.rs`, `theme.rs`,
   `ui/`, `vendor/` and the gpui and NSH dependencies.

## Open questions

- The mono-text terminal is good enough to type in; check `vim`, `htop` and large output.
- Linux Wayland: IME, clipboard and drag-and-drop in Rune are unchecked.
- The settings page is a main-pane page here; Po's current one is an overlay.
