# NSH migration: remaining work

Goal: draw Po's UI with [NSH](https://github.com/longbridge/nsh) instead of the hand-built gpui widgets in `src/ui/`
and `src/main.rs`.

## Done (branch `nsh`)
Todos window, sidebar, settings page, file finder, viewer source pane, viewer diff pane (`678fecb`).
Dead-code sweep: the old viewer is gone; `CodeColors` and the tree-sitter highlighter live in
`src/syntax/`. `viewer::init` went with it — its key bindings were bound to the dead `CodeView`
key context (the nsh editor ships its own keymap).

## The pattern every step follows
Look at `src/nsh/diff.rs` or `src/nsh/source.rs` as the template.

1. A `Snapshot` struct (`Clone + PartialEq`) is published through a `watch::Sender` from `Reader` in `main.rs`
   (`nsh::<view>::publish(...)`); `publish` skips identical snapshots.
2. A `#[component]` in `src/nsh/<view>.rs` subscribes to it and draws.
3. User actions go back through `nsh::bus::Cmd` (`send(Cmd::...)`) and are handled in `Reader`'s `Cmd` match
   (`main.rs`, near `Cmd::SetSplit`).
4. `Reader` holds the mounted `Entity<AppView>` from `nsh::mount(Root::of::<View>(), window, cx)`, plus its theme
   subscription.
5. A headless test in the same file (`nsh_testing::Headless`) that publishes a snapshot, checks the text on screen
   and presses buttons by `label:`. Add it to every view.
6. Delete the legacy code the step replaces in the same commit. One commit per view, message `Draw the X with NSH`.

## Steps, in suggested order
1. ~~**Dead-code sweep (small, do first).**~~ Done.
2. ~~**Agent / form panes.**~~ Done. `src/nsh/agent.rs` publishes the pane: a `Body::Markdown`
   (the agent's document rendered with NSH's `text_view`) or a `Body::Form` (its fields and
   whether the agent has the answers). What a field holds is the screen's own (`drafts`); the
   seeds reset when the form's id or fields change, which is what a replaced form means.
   Answers go back as `Cmd::SubmitForm(id, values)`, trimmed and with the empty ones left out,
   and `form_submitted` handles them where it did before. Gone with this step: `ui::form`
   wholly, `Thread.docs`/`form_uis`/`form_subs`/`sync_forms` and `markdown_view`; `Project.doc`
   is now just `is_markdown` — the markdown file itself is shown by the source pane's
   `Body::Markdown`. The legacy tab row (`view_tabs`, which keeps the agent's views as tabs)
   still switches among these panes. The old viewer is no longer built: `CodeView`, `CodeViewState`,
   `ViewScroll`, and most of `viewer/{cursor,element,search,wrap,text}.rs`. Still live: `viewer::CodeColors`
   (used by `ui/markdown.rs`), `viewer::prewarm`, `viewer::init`, `viewer::highlights` (a test). Cut it down to
   those, then move them somewhere small. Also unused: `ui::button::Button::text`, `ui::input::Input::{field,suffix}`,
   `Palette::editor_*`, `diff::Row.old/new`. Follow the build warnings to find the rest.
2. **Agent / form panes.** `agent_view` and `form_view` (`main.rs` ~5740-5895) are small and use `pane_header`.
   Move them next to the diff and source panes so `pane_header`, `status_text`, `centered` and `header_row` in
   `main.rs` lose callers.
3. **Strips.** `pr_strip`, `worktree_strip` (~5215-5300): small, self-contained.
4. **Thread panel (the big one).** `agent_panel` (~6198-6630) plus `tool_run`, `thought`, `session_picker`,
   `plan`, `permission`, `approval` (~6629-6950), and the composer built on `ui::input`. Split it:
   a. read-only transcript (messages, tool runs, thoughts, plan),
   b. permission/approval cards,
   c. composer and attachments,
   d. session picker and config dropdowns (`ui::menu`).
   Needs check against NSH: text selection across messages (today `ui::markdown::Selection`), virtualised long
   lists (today a gpui `ListState`), and streaming updates without re-publishing the whole transcript. Decide the
   snapshot shape (per-block ids, so only changed blocks diff) before starting.
5. **Markdown.** Replace `ui/markdown.rs` (978 lines) with NSH's markdown if it has one with code highlighting and
   link handling; the source pane already shows rendered markdown, so check how `nsh/source.rs` does it first.
6. **Remaining `ui/` widgets.** Once nothing calls them: `menu`, `tree`/`file_tree`, `list_item`, `resizable`
   (`sidebars`), `scrollbar`, `input`, `icon`, `button`. The window-level layout (`sidebars`, `body`) in
   `Reader::render` goes last; it may stay gpui if NSH can't own window chrome.
7. **Final.** Remove unused deps and `vendor/` bits, update `README`/`docs`, run `cargo build` with zero warnings.

## Checks for every step
- `cargo build` (no new warnings) and `cargo test` (192 tests plus the update e2e at the time of writing).
- Run the app and look at the view in light and dark themes and with the glass palette (`Palette.glass`); the
  headless tests don't cover appearance.
- Keyboard shortcuts that used to belong to the legacy widget still work (e.g. Ctrl+\ for inline/split).

## Open questions
- Does NSH provide text selection across multiple blocks, and virtual lists, for the thread?
- Can the window layout and resize handles live in NSH, or does gpui keep them?
