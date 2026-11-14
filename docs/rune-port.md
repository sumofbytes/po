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
- `src/host/app.rs` — the Rune host: `rune::run(AppSpec{..})`, shortcuts, and the `poll` that takes what
  the `Reader` published and runs the `Cmd`s the screens sent.
- `src/reader/*` and `Reader` in `src/reader/state.rs` — the logic, unchanged, written against gpui's
  `Window`/`Context<Self>`. `src/host/rt.rs` provides those names over `src/host/mod.rs` (a thread per
  spawned task, a scoped hop back onto the window's thread for `update`), so the logic needed no rewrite.
- `src/config/fonts.rs` — the bundled Geist and Geist Mono, handed to Rune's `AppSpec`.

gpui, NSH, `vendor/`, `src/nsh`, `src/ui`, `terminal_view.rs` and `theme.rs` are gone.

## Not ported yet (known gaps)

What earlier gaps remain, now that the terminal (resize, scrollback, paste, selection and its
copy), the chat (pictures, the Control menu, a virtualised transcript with its rail), the window
(saved pane widths, the active flag, dropped files) and the issues page (project chips, a More
popup for priority, label and lateness, an epic's page with its fields, milestones and issues,
labels and parent on an issue's detail) all work:

- **Terminal**: mouse reporting to programs, wide characters.
- **Chat**: the add box on the issues page always targets the repo last shown.
- **Tests**: the NSH `Headless` screen tests went with NSH; the `model` tests remain; there are no
  `rune::Runtime` tests of `rune_ui`. For profiling, `PO_BENCH=stream:<rounds>` fills the thread
  with `rounds` rounds so a long transcript lays out with nobody there.
- **Platforms**: only Linux was run. Wayland IME and clipboard, macOS and Windows are unchecked.
