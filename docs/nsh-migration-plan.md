# NSH migration

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
3. ~~**Strips.**~~ Done. `src/nsh/strip.rs` publishes the two strips under the explorer header:
   what a PR-reviewed project shows (`Pr::Opened`, or `Pr::Asking` with the field's last
   attempt's outcome) and what a worktree's shows (its branch, whether Discard waits for a
   second click). A project with neither strip leaves the screen empty. The PR spec the user
   goes back as `Cmd::OpenPr` (trimmed; empty is not sent), and the field empties itself when
   the snapshot's `pr_reset` changes, which is what an opened PR does. `Cmd::ClosePr`,
   `MergeWorktree` and `DiscardWorktree` go where they did before. Gone with this step:
   `pr_strip`/`worktree_strip`, the `pr_input` field with its enter subscription, and
   `ui::button`'s outline variant with the `outline` style it took from `Buttons`.
4. ~~**Thread panel.**~~ Done (`src/nsh/chat.rs`): transcript, cards, composer, session picker and menus. A queued
   message shows with a take-back button, a running turn offers Steer/Queue next to Stop (Alt+Enter sends after the
   turn), and `Snapshot::focus` lets the app put the keyboard in the message field.
5. ~~**Markdown.**~~ Done: `ui/markdown.rs` and the `markdown` crate are gone; NSH's `text_view` draws it.
6. ~~**Remaining `ui/` widgets.**~~ Done. The explorer tree and mode switch are `src/nsh/explorer.rs` over the plain
   data model in `src/explorer.rs` (open folders, selection, arrow keys); the viewer's tab row is `src/nsh/tabs.rs`.
   `src/ui/` is down to `icon.rs` (the icon names NSH draws) and `resizable.rs`: the columns that hold the NSH
   panes. Every pane is its own NSH view, so the columns that arrange them stay gpui.
7. ~~**Final.**~~ Done: the tree-sitter grammars and `src/syntax/` are gone (NSH highlights code itself), as are the
   unused `unicode-*`, `aho-corasick`, `markdown` and `streaming-iterator` dependencies. `cargo build` and
   `cargo test` have no warnings.

## Known differences from before the migration
- Code is colored by NSH's own fixed palette; the theme's `highlight.syntax` colors no longer apply.
- Right-clicking an explorer row no longer marks it.
