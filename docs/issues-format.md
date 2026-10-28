# The `.po/` issue format

Po keeps a repo's issues and epics as JSON files in the repo, so they travel with git and an agent
can read them without Po's API. Code: `src/issues/` (model in `mod.rs`, files in `store.rs`).

```text
.po/config.json         key prefix, next number, labels
.po/issues/PO-12.json   one issue
.po/epics/launch.json   one epic, with its milestones
```

Every file has `"version": 1`. Keys are written in a fixed order and empty fields are left out.
Only the fields marked required have to be there; anything missing or not understood takes its
default, so a hand-written file works.

## config.json

| Field | |
| --- | --- |
| `prefix` | Letters and digits in a key (`PO` in `PO-12`). Defaults to the initials of the repo's folder. |
| `next` | The number the next issue gets. Never goes down; on load it is raised past the highest issue file. |
| `labels` | `[{ "name": "bug", "color": "#e5484d" }]`. `color` is optional. |

## issues/PO-12.json

The file name is the key and the only place the number is kept. A file not named
`<prefix>-<number>.json` is ignored.

| Field | |
| --- | --- |
| `title` | **Required.** |
| `status` | `backlog` (default), `todo`, `in-progress`, `in-review`, `done`, `canceled`. |
| `priority` | `urgent`, `high`, `medium`, `low`; left out for none. |
| `description` | Markdown. |
| `labels` | Names, which should be in `config.json`'s labels. |
| `assignee` | Free text. |
| `parent` | Number of the issue this is a sub-issue of. A parent that is missing, or that loops, is ignored on load. |
| `epic`, `milestone` | An epic's id, and the id of one of its milestones. |
| `due` | `2026-11-02`. |
| `estimate` | A whole number; what it counts is up to the repo. |
| `created`, `updated` | Milliseconds since the epoch. |
| `links` | `{ "thread": "<session id>" }`, `{ "branch": "po-12-fix" }` or `{ "pr": 40 }`. |

Statuses fall in four categories, which are all that progress looks at: `backlog` and `todo` are
unstarted, `in-progress` and `in-review` started, then `done` and `canceled`. Canceled issues are
left out of every count.

## epics/launch.json

The file name is the id: lowercase letters, digits and dashes.

| Field | |
| --- | --- |
| `name` | **Required.** |
| `status` | `planned` (default), `active`, `paused`, `done`. |
| `description`, `lead` | As for an issue. |
| `start`, `target` | Dates. |
| `milestones` | `[{ "id": "beta", "name": "Beta", "target": "2026-12-01" }]`; ids are unique within the epic. |

Progress is never stored. An epic's is worked out from the issues that name it, a milestone's from
those that name both.

## The API

Agents reach the issues through Po's control plane (`src/issues/api.rs`; the tools are in
`src/mcp.rs`, the commands in `src/command.rs`), `po ctl` speaks the same commands as the user.
They work on the repo the caller's project belongs to; a worktree thread works on the primary
checkout's issues.

| Command | MCP tool | Reach |
| --- | --- | --- |
| `issue.list` `{scope, status, epic, label, priority, text, parent, limit}` | `list_issues` | Viewer |
| `issue.get` `{issue}` | `get_issue` | Viewer |
| `epic.list` | `list_epics` | Viewer |
| `issue.create` `{title, description, status, priority, labels, assignee, parent, epic, milestone, due, estimate}` | `create_issue` | Orchestrate |
| `issue.update` `{issue, …the same, add_labels, remove_labels, note, link}` | `update_issue` | Orchestrate |
| `epic.create` `{name, description, status, lead, start, target, add_milestone}` | `create_epic` | Orchestrate |
| `epic.update` `{epic, …the same, remove_milestone}` | `update_epic` | Orchestrate |
| `issue.delete` `{issue}` | `delete_issue` | Orchestrate |
| `epic.delete` `{epic}` | `delete_epic` | Orchestrate |
| `label.list` | `list_labels` | Viewer |
| `label.define` `{name, color}`, `label.remove` `{name}` | `define_label`, `remove_label` | Orchestrate |
| `issue.import_todos` `{force}` | `import_todos` | Orchestrate |
| `issue.show` `{view, issue, epic, scope, text, label, filter_epic, priority, late, group}` | `show_issues` | Orchestrate |
| `issue.work` `{issue, agent, notify}` | `work_on_issue` | Global, from a thread |

`issue.update` also takes `unlink`, the form of `link`, to take a link off. `issue.show` puts the
issues page on screen for the project that is showing, at a view, an issue or an epic, with a
filter and grouping; it is the one command that moves the user's screen. `issue.work` is the
"Work on this" button for an agent: a thread in a worktree of its own (as `thread.create_isolated`
makes), the issue moved to In Progress and its branch linked once the worktree is up. Merging that
worktree, which the user confirms, moves the issue to Done. Everything the UI does to issues can be
done through these commands; a drag on the board is `issue.update` with a new `status`.

An issue is named by its key (`PO-12`, or just `12`). In `issue.update` a field left out is left
alone, and `null` clears an assignee, parent, epic, milestone, due date or estimate. `note` is
appended to the description under a dated heading, which is how an agent reports progress; `link`
adds `{"thread": …}`, `{"branch": …}` or `{"pr": …}` once. Dates are `2026-11-02` or the words the
UI takes (`fri`, `nov 2`). Labels an agent names that the repo lacks are defined. A request that
can't be applied (an unknown key, a parent that would make a loop) changes nothing.

## In the app

- **Issues** in the sidebar opens the page for the project on screen: list (grouped by status,
  priority or epic, filtered by words, label, lateness and scope), board (drag a card across, or use
  its arrows), an issue's page (status, priority, labels, epic, milestone, due date, parent,
  sub-issues, description), and epics with milestones and progress. The add field takes
  `Fix login !high #bug @epic`. **Todos** is the old checklist; an empty tracker offers to
  import it.
- **Work on this** (on an issue's page) starts a thread on the issue in a worktree of its own,
  moves the issue to In Progress and links the branch. Merging that worktree moves the issues
  linked to its branch to Done.

## Behaviour worth knowing

- **Worktrees.** A thread's worktree shares the primary checkout's `.po/`; nothing is written in the
  worktree itself.
- **Merges.** Two branches that each create `PO-13` collide as an ordinary git add/add conflict on
  that file; resolve it by renumbering one (and its `parent` references). Numbers made on other
  branches never lower the counter, so the next issue after a merge gets a fresh number.
- **Files that can't be read.** Invalid JSON is moved aside as `<name>.json.bak` and reported, and
  the rest still load. Po never writes over it.
- **Writes.** A file is written to a temp file and renamed, and only if its contents changed, so
  saving with no change leaves git clean.
