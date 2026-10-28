# Plan: issues and projects in Po (Linear, lighter)

Status: phases 1–5 are in and phase 6 partly · 2026-10-28 (`src/issues/`, `src/nsh/issues.rs`,
`src/nsh/epics.rs`; the format and API are in `docs/issues-format.md`).

Where the build differs from the plan below:
- The detail view replaces the list (Back / Esc) instead of opening in a right-hand pane, and
  pickers are rows of chips, not popovers. Both are cheaper and test headlessly; NSH has `Popover`
  and `Menu` when a denser layout is wanted.
- The board moves a card by dragging it across columns (pan, tested headlessly) or with the arrows
  on the card. There is no ordering within a column and no drag preview yet.
- Changes made underneath (git, another window) are picked up by a 1.5 s poll of the files, not a
  file watcher.
- `issue.link` is a `link` field on `issue.update`.
- "Work on this" uses Po's usual worktree (branch `po/<id>`), not `po-12-short-title`, and links
  the branch, not the thread (the session id is not known when the worktree opens). Merging the
  worktree moves the issues linked to its branch to Done; PRs opened elsewhere are not watched.
- Phase 6: the `todos.json` import is in. Saved views, a cross-project "My issues" and cycles are
  not.

Tasks today are one global checklist (`src/task.rs`, `src/nsh/tasks.rs`, `todos.json`): nested items,
headers, a due date, done or not. This grows it into a small issue and project tracker that lives with
the repo, that people and agents both work in.

Decided: issues belong to a Po project (a repo) and have keys like `PO-12`; they are stored in the
repo; the scope is issues, projects and milestones, and agent access.

## 1. Model

```
Issue      key (PO-12), title, description (markdown), status, priority, labels[], assignee?,
           parent? (sub-issues), project?, milestone?, due?, estimate?, created, updated,
           links[] (thread, branch, PR)
Status     Backlog · Todo · In Progress · In Review · Done · Canceled   (fixed set; a status has a
           category: unstarted / started / done / canceled, which is all the UI logic looks at)
Priority   None · Urgent · High · Medium · Low
Label      name + color, defined per repo
Project    id, name, description, status (Planned · Active · Paused · Done), lead?, start, target,
           milestones[]                        — a project groups issues; progress is derived
Milestone  id, name, target date                — belongs to one project; progress is derived
Cycle      deferred. A milestone with dates covers it until someone misses it.
```

Deliberately left out to stay light: custom workflows, teams/workspaces, roles and permissions,
SLAs, triage queues, notifications, comments threads (a description plus links covers it for v1),
estimates beyond one optional number.

Naming: the repo-level thing a user calls "project" in the sidebar is a Po project (a checkout).
the Linear-style project is called an **Epic** in the UI to avoid the clash (open question 1). The
sections below say "epic" for it.

## 2. Storage

In the repo, under `.po/`:

```
.po/config.json          key prefix ("PO"), next number, labels, status names
.po/issues/PO-12.json    one file per issue
.po/epics/<id>.json      one file per project/epic, milestones inside it
```

One file per issue instead of the single `.po/issues.json` asked for: two branches that each add or
edit different issues then merge cleanly, and a git diff reads as "PO-12 moved to Done". A single
array file conflicts on every concurrent edit. The key counter is the one shared write; it lives in `config.json`. Two branches that both create
`PO-13` collide as a normal git add/add conflict on that file, resolved by hand; the counter
itself never falls below the highest issue file present, so the next number after a merge is
fresh.

Worktrees: Po makes a checkout per isolated thread. Each would have its own copy of `.po/`, and
edits would diverge. Rule: the store always resolves to the **primary checkout's** `.po/` (a
worktree's `worktree_of` project, `ProjectInfo::worktree_of` in `src/state.rs`), so every thread
sees one live copy. Whether to commit `.po/` is the user's call; it is not gitignored.

Writes keep `task.rs`'s habits: temp file then rename, a file that cannot be parsed is moved to
`.bak` rather than overwritten, and a file watcher reloads when git or another window changes
`.po/` underneath.

Existing `todos.json`: stays as a personal, cross-project checklist for now ("My tasks"), untouched
by this. A one-time "import as issues" action per header can come later.

## 3. Code layout

| New | Role |
| --- | --- |
| `src/issues/mod.rs` | `Issue`, `Status`, `Priority`, `Project`, `Milestone`; pure model plus queries (filter, group, progress). No UI, fully unit tested like `task.rs`. |
| `src/issues/store.rs` | Reads/writes `.po/`, repair pass, `watch` channel per repo root, file watcher. |
| `src/nsh/issues.rs` | The page: list grouped by status/priority/epic, board view, filter bar, quick-add. |
| `src/nsh/issue.rs` | Detail pane: title, description editor, property rows (status, priority, labels, due, epic, parent), sub-issues, links. |
| `src/nsh/epics.rs` | Epic list and an epic page with progress bar, milestones, its issues. |

Reused as is: `Date`, `parse_due` and `due_label` (move them to `src/date.rs`, no behaviour
change), the overlay page frame (`nsh::overlay::Overlay::page`), `Cmd` bus, icons (add a few from
Lucide: `circle-dashed`, `circle-dot`, `circle-check`, `signal`, `tag`, `kanban`, `milestone`).

Sidebar: the "Tasks" row becomes **Issues** with the open count and late count for the active
project; a small "My tasks" entry keeps the old checklist reachable.

## 4. UX (keyboard first, like Linear)

- `c` new issue anywhere on the page; the quick-add line keeps `parse_due` and gains inline
  tokens: `!urgent`, `#label`, `@epic`.
- List grouped by status (default), priority or epic; each row: priority glyph, key, title,
  labels, due, status circle. Click the circle to cycle status; `s`, `p`, `l`, `d` open pickers on
  the focused row.
- Board view: columns by status, drag between them (reorders write `sort` within a status).
- Filter bar: status, priority, label, epic, due (late/this week), free text. Saved views are v2.
- `Enter` opens the detail pane on the right; sub-issues render inline, parent shows progress.
- Epic page: progress bar from issue categories, milestone list with their own bars, burn-up is
  v2.

## 5. Agent integration

The control plane already exists (`docs/agent-control-plan.md`: `Scope`/`Grant`, `command.rs`,
`mcp.rs`). Add commands, with reach:

| Command / MCP tool | Reach |
| --- | --- |
| `issue.list`, `issue.get`, `epic.list` | Read (Viewer) |
| `issue.create`, `issue.update` (status, priority, labels, description…); progress notes are appended to the description under a dated heading, as there are no comments | Orchestrate |
| `epic.create`, `epic.update`, `milestone.*` | Orchestrate |
| `issue.link` (attach thread / branch / PR) | Orchestrate |

Plus UI: "Work on this" on an issue starts a thread (isolated worktree, branch named
`po-12-short-title`) with the issue as the prompt, and links the three. When that thread's PR
merges, the issue moves to Done (one rule, shown in the UI, not hidden automation). A thread's
header shows its linked issue key.

Because the files are in the repo, an agent without Po's API can still read `.po/issues/*.json`;
the schema is documented in `docs/issues-format.md` (written in phase 1).

## 6. Phases

Each phase ships and is usable on its own.

1. **Core model + store.** `src/issues`, `.po/` format, repair pass, tests (model, round trip,
   key allocation, collision repair, worktree resolution). No UI.
2. **Issues page.** Replace the Tasks page with the issue list, quick-add, status/priority/labels,
   filters, detail pane, sub-issues. Sidebar row. Headless tests in the style of
   `nsh/tasks.rs`.
3. **Board view** and drag to move.
4. **Epics + milestones.** Model already there from phase 1; epic list/page, progress, assign
   issues.
5. **Agent API.** ctl/MCP commands, reach table entries and test, "Work on this", thread↔issue
   links, PR-merged → Done.
6. **Polish.** Saved views, "My issues" across open projects, import from `todos.json`, cycles if
   milestones prove too coarse.

## 7. Risks

- **NSH is young.** Pickers, drag and drop and a multiline markdown editor may not exist in
  `nsh_base`; phase 2 starts with a spike on those three. Fallback: popover lists built from
  `view()` and a plain multiline `input`, as the composer in `tasks.rs` already does.
- **Git noise.** Every status change dirties the tree. Mitigation: stable key ordering and
  pretty-printed JSON so diffs are one line; `updated` timestamps are not written when nothing
  else changed.
- **Two windows, one repo.** Handled by the watcher plus last-writer-wins per issue file (field
  merge is v2).

## 8. Open questions

1. Settled: **Epic**, and one file per issue.
2. Should `.po/` be committed by default, or should Po offer to add it to `.gitignore` on first
   use?
