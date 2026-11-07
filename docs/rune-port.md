# Po's UI on Rune

Branch `feat/rune`. Rune replaces gpui and NSH, which removes `vendor/`. Rune is pinned to a `dev`
rev in `Cargo.toml`; `dev` is never rewritten, so bump the rev deliberately.

Run it: `cargo run --bin po`. Headless: `po --foreground --script events.json --snap out.png --at 2 <project>`
(events: `tap` with `x`,`y`, `type`, `key`). Use a scratch `HOME`/`XDG_*` so the real workspace is
not touched.

## How it is put together

- `src/model/` — what the screens and the `Reader` exchange: snapshots (chat, sidebar, diff,
  explorer, source, agent, finder, tabs, strips, home), the bus `Cmd`, header `Control`. No view code.
- `src/rune_ui/` — the screens, each drawing a snapshot and sending `Cmd`s: shell (layout), header,
  home, issues (list, board, epics, issue page), sidebar, explorer, tabs, finder, source, diff,
  agent, chat, terminal, settings, theme.
- `src/app.rs` — the Rune host: `rune::run(AppSpec{..})`, shortcuts, and the `poll` that takes what
  the `Reader` published and runs the `Cmd`s the screens sent.
- `src/reader/*` and `Reader` in `main.rs` — the logic, unchanged, written against gpui's
  `Window`/`Context<Self>`. `src/rt.rs` provides those names over `src/host.rs` (a thread per
  spawned task, a scoped hop back onto the window's thread for `update`), so the logic needed no rewrite.
- `src/fonts.rs` — the bundled Geist and Geist Mono, handed to Rune's `AppSpec`.

gpui, NSH, `vendor/`, `src/nsh`, `src/ui`, `terminal_view.rs` and `theme.rs` are gone.

## Not ported yet (known gaps)

- **Issues**: no labels or parent editing, milestones, epic pages (an epic click filters the list),
  or project filter chips; the add box always targets the repo last shown.
- **Chat**: pictures work (dropped or pasted into the draft, shown in it and in the transcript, detachable); no Control (grant) menu, transcript rail, or virtualised transcript.
- **Terminal**: resize, scrollback (wheel), paste and drag-selection (Rune's text selection) work; Ctrl+Shift+C/V and the platform key are left to the window. Not checked: copy to the clipboard, mouse reporting to programs, wide characters.
- **Window**: pane widths save and restore, the window reports whether it is active, files dropped
  on it attach to the message being written, and Ctrl+P works with a field focused. Unchecked:
  desktop notifications and the `po update` flow.
- **Tests**: the NSH `Headless` screen tests went with NSH; the `model` tests remain. There are
  no `rune::Runtime` tests of `rune_ui` yet.
- **Platforms**: only Linux was run. Wayland IME and clipboard, macOS and Windows are unchecked.
