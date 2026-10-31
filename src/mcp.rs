//! `po mcp`: the commands of [`crate::command`] as an MCP server, so an agent finds them among its
//! tools. Po hands it to each agent it starts, with the
//! socket and token of that agent's thread; every tool call becomes one request to the window.
use serde_json::{Value, json};

use crate::command::{Command, Grant, Relation, Scope};

/// What the server tells the agent it is for.
const BASE: &str = "You are running inside Po, a desktop app that shows the user your \
conversation next to a viewer of the project's files and changes. Use these tools to put in front \
of the user what you are talking about: open the file or diff you refer to, or show a plan or \
summary as a markdown view. show_form goes further: a form of your own fields and choices the \
user fills in, whose answers form_wait brings you — the form stays until they answer or close \
it, so wait again after a timeout. The user decides what they look at: while they are typing or \
scrolling, a move of the viewer waits for a pause (the reply says `deferred`) and a new view is \
offered rather than shown. Don't fight that by asking again.";

/// What every thread may read, added to [`BASE`].
const READS: &str = "You can also ask about yourself and your project: control_info says who \
you are and what the user has granted; get_thread, thread_messages and last_reply read your \
thread's state and transcript; thread_options lists its settings; list_threads, list_agents, \
list_projects and project_changes say what else Po has open. list_issues, get_issue and \
list_epics read the project's issues, which live in its .po folder. Reading the threads of others, \
rather than your own, needs a grant the user gives in Po.";

/// What an orchestrator may do, added when its grant says so. The tools of this scope arrive
/// with phase 2 of the control plan.
const ORCHESTRATE: &str = "You may start and steer other threads: each starts with the agent \
and settings you choose, answers you when it finishes or asks, and is yours to read and close. \
They start with the smallest grant, and nothing they do can reach further than yours does. \
You may also keep the project's issues: create_issue, update_issue, create_epic and \
update_epic. When you work an issue, move it along and report with a note.";

/// What a globally granted thread may do, added when its grant says so.
const GLOBAL: &str = "You may open other projects and offer them to the user: opening a new \
folder asks them first, and nothing switches the screen by itself.";

fn instructions(grant: Option<&Grant>) -> String {
    let mut out = BASE.to_string();
    out.push_str("\n\n");
    out.push_str(READS);
    let Some(grant) = grant else { return out };
    if grant.scope >= Scope::Orchestrate {
        out.push_str("\n\n");
        out.push_str(ORCHESTRATE);
    }
    if grant.scope >= Scope::Global {
        out.push_str("\n\n");
        out.push_str(GLOBAL);
    }
    out
}

/// Protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Each tool, the command it sends, what it is for and its arguments.
fn tools() -> Vec<(&'static str, &'static str, &'static str, Value, Scope)> {
    let string = |description: &str| json!({ "type": "string", "description": description });
    let number = |description: &str| json!({ "type": "integer", "minimum": 1, "description": description });
    vec![
        (
            "show_view",
            "view.show",
            "Show the user a markdown document in Po's viewer, as a tab of its own: a plan, a \
             summary, a review. Giving the `id` of one of your views replaces it. The reply \
               carries the view's `id`, which update_view and close_view take.",
            json!({
                "type": "object",
                "properties": {
                    "title": string("A short title for the tab."),
                    "body": string("The document, in markdown."),
                    "id": string("The view to replace; leave out to add a new one."),
                },
                "required": ["title", "body"],
            }),
            Scope::Viewer,
        ),
        (
            "update_view",
            "view.update",
            "Change the title or body of one of your views without moving the user to it.",
            json!({
                "type": "object",
                "properties": {
                    "id": string("The view, as show_view returned it."),
                    "title": string("A new title."),
                    "body": string("A new body, in markdown."),
                },
                "required": ["id"],
            }),
            Scope::Viewer,
        ),
        (
            "close_view",
            "view.close",
            "Close one of your views.",
            json!({ "type": "object", "properties": { "id": string("The view to close.") }, "required": ["id"] }),
            Scope::Viewer,
        ),
        (
            "open_file",
            "file.open",
            "Open a file of the project in Po's viewer, at a line if given, to show the user the \
             code you are talking about.",
            json!({
                "type": "object",
                "properties": {
                    "path": string("The file, relative to the project root or absolute inside it."),
                    "line": number("The line to put in view, from 1."),
                },
                "required": ["path"],
            }),
            Scope::Viewer,
        ),
        (
            "show_diff",
            "diff.show",
            "Show the project's uncommitted changes in Po's viewer, at one file's diff if given.",
            json!({
                "type": "object",
                "properties": { "path": string("A changed file, relative to the project root.") },
            }),
            Scope::Viewer,
        ),
        (
            "close_viewer",
            "viewer.close",
            "Close Po's viewer, so the conversation has the window to itself.",
            json!({ "type": "object", "properties": {} }),
            Scope::Viewer,
        ),
        (
            "show_form",
            "form.show",
            "Put a form in front of the user in Po's viewer: text fields and choices you \
             define, with a submit button. They answer at their pace; form_wait brings you \
             their answers, keyed by each field's name. Giving the `id` of one of your forms \
               replaces it. The reply carries the form's `id`, which form_wait takes.",
            json!({
                "type": "object",
                "properties": {
                    "title": string("A short title for the tab."),
                    "body": string("What the form is for, in markdown, shown above the fields."),
                    "fields": {
                        "type": "array",
                        "description": "The fields, in order.",
                        "items": {
                            "type": "object",
                            "description": "A text field is `{ \"name\", \"label\", \"kind\": \"text\", \"value\" }`; a choice adds `\"options\": [\"a\", \"b\"]`, and `value` is one of them picked to begin with.",
                            "properties": {
                                "name": string("The field's name, as the answers come back keyed by it."),
                                "label": string("What the user sees; the name, when left out."),
                                "kind": { "type": "string", "description": "`text` by default, or `choice`." },
                                "options": { "type": "array", "items": { "type": "string" }, "description": "A choice's options, in order." },
                                "value": string("The initial text, or the choice picked to begin with."),
                            },
                            "required": ["name"],
                        },
                    },
                    "submit": string("The submit button's label; `Submit` by default."),
                    "id": string("The form to replace; leave out to add a new one."),
                },
                "required": ["title"],
            }),
            Scope::Viewer,
        ),
        (
            "form_wait",
            "form.wait",
            "Wait for the user to submit one of your forms, at most 50 s; the reply carries \
             their answers, keyed by each field's name. On a timeout, wait again — the form \
             stays until they answer or close it.",
            json!({
                "type": "object",
                "properties": {
                    "id": string("The form, as show_form returned it."),
                    "timeout": { "type": "integer", "description": "Give up after this many seconds; at most 50." },
                },
                "required": ["id"],
            }),
            Scope::Viewer,
        ),
        (
            "control_info",
            "control.info",
            "Who you are to Po: your thread, and what the user has granted it. Call it first; it \
             says which of the other tools you may use.",
            json!({ "type": "object", "properties": {} }),
            Scope::Viewer,
        ),
        (
            "list_agents",
            "agent.list",
            "The agents Po can run: their ids for starting threads, and which the user trusts.",
            json!({ "type": "object", "properties": {} }),
            Scope::Viewer,
        ),
        (
            "list_projects",
            "project.list",
            "The projects open in Po's window, with their branches and whether they have \
            changes.",
            json!({ "type": "object", "properties": {} }),
            Scope::Viewer,
        ),
        (
            "project_changes",
            "project.changes",
            "The project's changed files, with their status; give `path` for one file's diff.",
            json!({
                "type": "object",
                "properties": {
                    "path": string("A changed file, relative to the project root, to read its diff."),
                },
            }),
            Scope::Viewer,
        ),
        (
            "list_threads",
            "thread.list",
            "The threads Po has, and where each stands: running, asking, failed, asleep. Yours \
             and your project's; the others only when the user grants it.",
            json!({
                "type": "object",
                "properties": {
                    "state": { "type": "string", "description": "List only threads in this state.", "enum": ["starting", "approval", "ready", "running", "asking", "failed", "asleep", "closed"] },
                },
            }),
            Scope::Viewer,
        ),
        (
            "get_thread",
            "thread.get",
            "One thread in full: its state, what it is doing, what waits in its queue, and what \
             it asks, if anything. Yours unless the user granted reading others.",
            json!({ "type": "object", "properties": { "thread": { "type": "integer", "description": "The thread; yours when left out." } } }),
            Scope::Viewer,
        ),
        (
            "thread_options",
            "thread.options",
            "A thread's settings, like its model, with the choices each takes.",
            json!({ "type": "object", "properties": { "thread": { "type": "integer", "description": "The thread; yours when left out." } } }),
            Scope::Viewer,
        ),
        (
            "thread_messages",
            "thread.messages",
            "A thread's transcript, oldest first. Give the `next` cursor of an earlier reply to \
             read on; a `reset` says the transcript started over.",
            json!({
                "type": "object",
                "properties": {
                    "thread": { "type": "integer", "description": "The thread; yours when left out." },
                    "cursor": string("Where to read from, as the last reply's `next` said."),
                    "limit": { "type": "integer", "description": "At most this many messages (200 when left out)." },
                    "full": { "type": "boolean", "description": "Include tool calls' arguments and results." },
                },
            }),
            Scope::Viewer,
        ),
        (
            "last_reply",
            "thread.last_reply",
            "What a thread's agent last said, in its own words. This is the quick way to hear \
             what a thread of yours came back with.",
            json!({ "type": "object", "properties": { "thread": { "type": "integer", "description": "The thread; yours when left out." } } }),
            Scope::Viewer,
        ),
        (
            "create_thread",
            "thread.create",
            "Start a thread of your own: it runs `agent` (yours by default) in the project, \
             gets `prompt` as its first message, and answers you when it finishes or asks. It \
             starts with the smallest grant, and it is never shown to the user unless you ask \
             Po to offer it. Tell it how to answer you with `notify`: `done` (default), `all`, \
             or `never` — you can also just wait for it.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": string("The thread's first message. A thread without one is refused."),
                    "agent": string("The agent to run, as agent.list says; yours by default."),
                    "notify": { "type": "string", "description": "How the thread reports to you: `done`, `all`, or `never`.", "enum": ["done", "all", "never"] },
                    "options": {
                        "type": "object",
                        "description": "Settings for the session, as thread_options says them: an object of setting id to choice.",
                        "additionalProperties": { "type": "string" },
                    },
                },
                "required": ["prompt"],
            }),
            Scope::Orchestrate,
        ),
        (
            "send_thread",
            "thread.send",
            "Send text to a thread you started: into the turn (steer) if its agent takes \
             messages mid-turn, after the turn with `later`, or queued for when it is free.",
            json!({
                "type": "object",
                "properties": {
                    "thread": { "type": "integer", "description": "The thread, as create_thread returned it." },
                    "text": string("What to send."),
                    "later": { "type": "boolean", "description": "Send after the turn instead of into it." },
                },
                "required": ["thread", "text"],
            }),
            Scope::Orchestrate,
        ),
        (
            "cancel_thread",
            "thread.cancel",
            "Stop a thread you started mid-turn. Text of its own that was still queued comes \
             back in the reply.",
            json!({ "type": "object", "properties": { "thread": { "type": "integer", "description": "The thread." } }, "required": ["thread"] }),
            Scope::Orchestrate,
        ),
        (
            "close_thread",
            "thread.close",
            "End a thread you started, stopping its agent.",
            json!({ "type": "object", "properties": { "thread": { "type": "integer", "description": "The thread." } }, "required": ["thread"] }),
            Scope::Orchestrate,
        ),
        (
            "set_option",
            "thread.set_option",
            "Ask a thread to change one of its settings (as thread_options says them). It \
             applies between turns, and it is never remembered as the user's choice.",
            json!({
                "type": "object",
                "properties": {
                    "thread": { "type": "integer", "description": "The thread; yours when left out." },
                    "id": string("The setting's id, as thread_options says them."),
                    "value": string("One of the setting's choices, as thread_options says on each."),
                },
                "required": ["id", "value"],
            }),
            Scope::Viewer,
        ),
        (
            "wait_thread",
            "thread.wait",
            "Wait for one of your threads (or all of them) to reach a state, at most 50 s. \
             Better is to be told: create_thread answers you by default. Waits at most two at \
             a time.",
            json!({
                "type": "object",
                "properties": {
                    "threads": { "type": "array", "items": { "type": "integer" }, "description": "Which of your threads; all of them when left out." },
                    "until": { "type": "string", "description": "The state to wait for, `done` by default (a turn ended, failed, asked, asleep or closed), or `any` change.", "enum": ["starting", "approval", "ready", "running", "asking", "failed", "asleep", "closed", "done", "any"] },
                    "timeout": { "type": "integer", "description": "Give up after this many seconds; at most 50." },
                },
            }),
            Scope::Orchestrate,
        ),
        (
            "answer_thread",
            "thread.answer",
            "Answer a permission ask of a thread you started (get_thread shows what it asks and \
             the options' ids). The user's own approvals of a command or a tool server are not \
             reachable from here.",
            json!({
                "type": "object",
                "properties": {
                    "thread": number("The thread."),
                    "option": string("The id of one of the ask's options."),
                },
                "required": ["thread", "option"],
            }),
            Scope::Orchestrate,
        ),
        (
            "find_thread",
            "thread.find",
            "The thread id for a saved session id, which outlives a restart of Po where thread \
             ids do not.",
            json!({ "type": "object", "properties": { "session": string("The session id.") }, "required": ["session"] }),
            Scope::Viewer,
        ),
        (
            "open_project",
            "project.open",
            "Open a folder, or a pull request for review, as a project in Po's window, in the \
             background. A project that is already open is returned as it is; a new one is first \
             asked of the user, and the reply says `asked`. It never switches what the user is \
             looking at. Give `path` or `pr`.",
            json!({
                "type": "object",
                "properties": {
                    "path": string("A directory; not the home directory or the root."),
                    "pr": string("A PR number, branch or URL, as `gh pr view` takes it."),
                },
            }),
            Scope::Global,
        ),
        (
            "create_isolated_thread",
            "thread.create_isolated",
            "Like create_thread, in a git worktree of your project: a branch of its own that \
             the user can review as a project. The worktree takes a moment: the reply says it \
             is being made, and you are told with the thread and project ids when it exists, or \
             why it could not be. Merge the work with merge_worktree.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": string("The thread's first message."),
                    "agent": string("The agent to run, as agent.list says; yours by default."),
                    "notify": { "type": "string", "description": "How the thread reports to you: `done`, `all`, or `never`.", "enum": ["done", "all", "never"] },
                    "options": { "type": "object", "description": "Settings for the session: setting id to choice.", "additionalProperties": { "type": "string" } },
                },
                "required": ["prompt"],
            }),
            Scope::Global,
        ),
        (
            "merge_worktree",
            "worktree.merge",
            "Ask the user to merge a worktree project into the checkout it came from. They \
             decide with a click, and you are told how it went. Discarding is theirs alone.",
            json!({ "type": "object", "properties": { "project": number("The worktree's project, as list_projects says.") }, "required": ["project"] }),
            Scope::Global,
        ),
        (
            "close_project",
            "project.close",
            "Close a project you opened, once the user has no thread of their own in it.",
            json!({ "type": "object", "properties": { "project": number("The project, as list_projects says.") }, "required": ["project"] }),
            Scope::Global,
        ),
        (
            "list_issues",
            "issue.list",
            "The project's issues, most pressing first. Open ones unless `scope` says otherwise; \
             `status`, `epic`, `label`, `priority`, `text` and `parent` narrow it.",
            json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "Open ones (default), closed ones or all of them.", "enum": ["open", "closed", "all"] },
                    "status": { "type": "string", "description": "The status to list.", "enum": ["backlog", "todo", "in-progress", "in-review", "done", "canceled"] },
                    "epic": string("An epic's id, as list_epics says."),
                    "label": string("A label's name, as list_labels says."),
                    "priority": { "type": "string", "description": "How pressing.", "enum": ["urgent", "high", "medium", "low"] },
                        "text": string("Words to find in the key, title, description or labels."),
                        "parent": string("An issue's key, such as PO-12: only its sub-issues."),
                    "limit": number("At most this many (default 50, most 200)."),
                },
            }),
            Scope::Viewer,
        ),
        (
            "get_issue",
            "issue.get",
            "One issue in full: description, links, sub-issues and their progress.",
            json!({ "type": "object", "properties": { "issue": string("The issue's key, such as PO-12.") }, "required": ["issue"] }),
            Scope::Viewer,
        ),
        (
            "create_issue",
            "issue.create",
            "Add an issue to the project. It starts as a todo. Labels you name that the project \
             lacks are added.",
            json!({
                "type": "object",
                "properties": {
                    "title": string("What needs doing."),
                    "description": string("Markdown."),
                    "status": { "type": "string", "description": "How it starts; `todo` by default.", "enum": ["backlog", "todo", "in-progress", "in-review", "done", "canceled"] },
                    "priority": { "type": "string", "description": "How pressing.", "enum": ["urgent", "high", "medium", "low"] },
                    "labels": json!({ "type": "array", "items": { "type": "string" }, "description": "Label names." }),
                    "assignee": string("Who it is for."),
                    "parent": string("The key of the issue this is a sub-issue of."),
                    "epic": string("An epic's id, as list_epics says."),
                    "milestone": string("A milestone's id of that epic, as list_epics says."),
                    "due": string("A date: 2026-11-02, or fri, nov 2."),
                    "estimate": number("A whole number."),
                },
                "required": ["title"],
            }),
            Scope::Orchestrate,
        ),
        (
            "update_issue",
            "issue.update",
            "Change an issue. Give only what changes; null clears assignee, parent, epic, \
             milestone, due and estimate. `note` is added to the description under a dated \
             heading, which is how to report progress. `link` ties it to a thread, branch or PR. \
             Move it to in-progress when you start it and to in-review or done when you finish.",
            json!({
                "type": "object",
                "properties": {
                    "issue": string("The issue's key, such as PO-12."),
                    "title": string("A new title."),
                    "description": string("Replaces the description."),
                    "note": string("Appended to the description, dated."),
                    "status": { "type": "string", "description": "A new status.", "enum": ["backlog", "todo", "in-progress", "in-review", "done", "canceled"] },
                    "priority": { "type": "string", "description": "How pressing; `none` clears it.", "enum": ["urgent", "high", "medium", "low", "none"] },
                    "labels": json!({ "type": "array", "items": { "type": "string" }, "description": "Replaces the labels." }),
                    "add_labels": json!({ "type": "array", "items": { "type": "string" } }),
                    "remove_labels": json!({ "type": "array", "items": { "type": "string" } }),
                    "assignee": json!({ "type": ["string", "null"] }),
                    "parent": json!({ "type": ["string", "null"], "description": "An issue's key, such as PO-12; null clears it." }),
                    "epic": json!({ "type": ["string", "null"], "description": "An epic's id, as list_epics says; null clears it." }),
                    "milestone": json!({ "type": ["string", "null"], "description": "A milestone's id, as list_epics says; null clears it." }),
                    "due": json!({ "type": ["string", "null"], "description": "The date, in create_issue's `due` form, or null to clear it." }),
                    "estimate": json!({ "type": ["integer", "null"], "description": "A whole number, or null to clear it." }),
                    "link": json!({ "type": "object", "description": "One of {thread: session}, {branch: name}, {pr: number}." }),
                    "unlink": json!({ "type": "object", "description": "A link to take off, in the same form." }),
                },
                "required": ["issue"],
            }),
            Scope::Orchestrate,
        ),
        (
            "list_epics",
            "epic.list",
            "The project's epics, with each one's `id` — which `epic` on list_issues and \
               update_epic take —, its milestones and how far along it is.",
            json!({ "type": "object", "properties": {} }),
            Scope::Viewer,
        ),
        (
            "create_epic",
            "epic.create",
            "Add an epic: a body of work that issues belong to.",
            json!({
                "type": "object",
                "properties": {
                    "name": string("The epic's name."),
                    "description": string("Markdown."),
                    "status": { "type": "string", "description": "How it starts; `planned` by default.", "enum": ["planned", "active", "paused", "done"] },
                    "lead": string("Who leads it."),
                    "start": string("A date."),
                    "target": string("A date."),
                    "add_milestone": json!({ "description": "A name, or {name, target}.", "type": ["string", "object"] }),
                },
                "required": ["name"],
            }),
            Scope::Orchestrate,
        ),
        (
            "update_epic",
            "epic.update",
            "Change an epic, or add or remove one of its milestones.",
            json!({
                "type": "object",
                "properties": {
                    "epic": string("The epic's id, as list_epics says."),
                    "name": string("A new name."),
                    "description": string("Replaces the description."),
                    "status": { "type": "string", "description": "A new status.", "enum": ["planned", "active", "paused", "done"] },
                    "lead": json!({ "type": ["string", "null"] }),
                    "start": json!({ "type": ["string", "null"] }),
                    "target": json!({ "type": ["string", "null"] }),
                    "add_milestone": json!({ "description": "A name, or {name, target}.", "type": ["string", "object"] }),
                    "remove_milestone": string("A milestone's id."),
                },
                "required": ["epic"],
            }),
            Scope::Orchestrate,
        ),
        (
            "delete_issue",
            "issue.delete",
            "Delete an issue for good: its file goes, and its sub-issues become top-level. To drop \
             an issue and keep the record, set its status to canceled instead.",
            json!({ "type": "object", "properties": { "issue": string("The issue's key, such as PO-12.") }, "required": ["issue"] }),
            Scope::Global,
        ),
        (
            "delete_epic",
            "epic.delete",
            "Delete an epic and its milestones. Its issues stay, out of any epic.",
            json!({ "type": "object", "properties": { "epic": string("The epic's id, as list_epics says.") }, "required": ["epic"] }),
            Scope::Global,
        ),
        (
            "list_labels",
            "label.list",
            "The project's labels, with their colors and how many issues have each.",
            json!({ "type": "object", "properties": {} }),
            Scope::Viewer,
        ),
        (
            "define_label",
            "label.define",
            "Add a label, or change the color of the one by that name.",
            json!({
                "type": "object",
                "properties": { "name": string("The label's name."), "color": string("#rrggbb, or leave out for the default.") },
                "required": ["name"],
            }),
            Scope::Orchestrate,
        ),
        (
            "remove_label",
            "label.remove",
            "Delete a label, and take it off the issues that have it.",
            json!({ "type": "object", "properties": { "name": string("The label's name.") }, "required": ["name"] }),
            Scope::Global,
        ),
        (
            "import_todos",
            "issue.import_todos",
            "Bring the user's todo list in as issues: nesting, due dates and checked-off tasks come \
             along, and a header becomes the label of the tasks under it. The todo list is left as \
             it is. Refused if the project already has issues, unless `force` is set.",
            json!({ "type": "object", "properties": { "force": json!({ "type": "boolean" }) } }),
            Scope::Global,
        ),
        (
            "show_issues",
            "issue.show",
            "Put the issues page in front of the user, for the project on screen: the list, the \
             board or the epics, or one issue or epic, with a filter and grouping. The user \
             decides what they look at, so use it to show what you are talking about.",
            json!({
                "type": "object",
                "properties": {
                    "view": { "type": "string", "description": "The list (default), the board or the epics.", "enum": ["list", "board", "epics"] },
                    "issue": string("An issue's key, such as PO-12: open its page."),
                    "epic": string("An epic's id, as list_epics says: open its page."),
                    "scope": { "type": "string", "description": "Open ones (default), closed ones or all of them.", "enum": ["open", "closed", "all"] },
                    "text": string("Words to filter the list by."),
                    "label": string("A label's name, as list_labels says."),
                    "filter_epic": string("An epic's id to list the issues of."),
                    "priority": { "type": "string", "description": "How pressing.", "enum": ["urgent", "high", "medium", "low"] },
                    "late": json!({ "type": "boolean" }),
                    "group": { "type": "string", "description": "How to group the list.", "enum": ["status", "priority", "epic", "none"] },
                },
            }),
            Scope::Orchestrate,
        ),
        (
            "work_on_issue",
            "issue.work",
            "Start a thread on an issue in a git worktree of its own, as create_isolated_thread \
             does, with the issue as its first message. The issue moves to in-progress and the \
             worktree's branch is linked to it once the worktree is up; merging that worktree \
             moves it to done.",
            json!({
                "type": "object",
                "properties": {
                    "issue": string("The issue's key, such as PO-12."),
                    "agent": string("The agent's id, as list_agents says."),
                    "notify": { "type": "string", "description": "How the thread reports to you: `done`, `all`, or `never`.", "enum": ["done", "all", "never"] },
                },
                "required": ["issue"],
            }),
            Scope::Global,
        ),
        (
            "offer_project",
            "project.offer",
            "Suggest a project to the user: a chip appears for them to click, and the screen \
             stays where it is.",
            json!({ "type": "object", "properties": { "project": number("The project, as list_projects says.") }, "required": ["project"] }),
            Scope::Global,
        ),
    ]
}

/// A parseable request for `command`, without arguments: for looking a command's scope and
/// grant up, which its shape decides, not its arguments.
fn parse_command(command: &str) -> Option<Command> {
    let mut request = json!({ "command": command });
    match command {
        "view.show" => {
            request["title"] = json!("");
            request["body"] = json!("");
        }
        "view.update" => {
            request["id"] = json!("v");
            request["body"] = json!("");
        }
        "view.close" => {
            request["id"] = json!("v");
        }
        "form.show" => {
            request["title"] = json!("");
        }
        "form.wait" => {
            request["id"] = json!("f");
        }
        "file.open" => {
            request["path"] = json!("a");
        }
        "thread.create" | "thread.create_isolated" => {
            request["prompt"] = json!("");
        }
        "thread.send" => {
            request["thread"] = json!(1);
            request["text"] = json!("");
        }
        "thread.cancel" | "thread.close" => {
            request["thread"] = json!(1);
        }
        "project.open" => {
            request["path"] = json!("/");
        }
        "thread.find" => {
            request["session"] = json!("s");
        }
        "thread.answer" => {
            request["thread"] = json!(1);
            request["option"] = json!("o");
        }
        "project.close" | "project.offer" | "worktree.merge" => {
            request["project"] = json!(1);
        }
        "thread.set_option" => {
            request["id"] = json!("s");
            request["value"] = json!("v");
        }
        _ => {}
    }
    Command::parse(&request).ok()
}

/// The tools the grant allows, as MCP lists them.
fn listed(grant: Option<&Grant>) -> Vec<Value> {
    tools()
        .into_iter()
        .filter(|(_, command, ..)| parse_command(command).is_some_and(|parsed| grant.is_none_or(|grant| grant.allows(&parsed, Relation::Own))))
        .map(|(name, _, description, schema, _)| json!({ "name": name, "description": description, "inputSchema": schema }))
        .collect()
}

/// The answer to one JSON-RPC message, or none for a notification. `send` carries a command to
/// the window and returns its reply; `grant` is this server's, and `initialize` refreshes it.
fn handle(message: &Value, send: &dyn Fn(Value) -> anyhow::Result<Value>, grant: &mut Option<Grant>) -> Option<Value> {
    let id = message.get("id")?.clone();
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => {
            // Who this thread is and what it may do: the tool list is fixed for the agent's
            // lifetime from this answer, so it is asked now, once.
            *grant = crate::control::send(json!({ "command": "control.info" })).ok().and_then(|reply| grant_of(&reply));
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS.iter().find(|version| **version == asked).unwrap_or(&VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "po", "version": env!("CARGO_PKG_VERSION") },
                "instructions": instructions(grant.as_ref()),
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": listed(grant.as_ref()) })),
        "tools/call" => call(params, send, grant.as_ref()),
        method => Err((-32601, format!("no method `{method}`"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

/// A grant, as `control.info` said it.
fn grant_of(reply: &Value) -> Option<Grant> {
    let grant = reply["grant"].as_object()?;
    Some(Grant {
        scope: match grant["scope"].as_str()? {
            "viewer" => Scope::Viewer,
            "orchestrate" => Scope::Orchestrate,
            "global" => Scope::Global,
            _ => return None,
        },
        read_project: grant["read_project"].as_bool()?,
        approvals: grant["approvals"].as_bool()?,
    })
}

/// Runs a tool. What the window refuses is the tool's error for the agent to read, not the
/// protocol's; a tool the grant no longer allows is refused before the window hears of it.
fn call(params: &Value, send: &dyn Fn(Value) -> anyhow::Result<Value>, grant: Option<&Grant>) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let command = tools().into_iter().find(|tool| tool.0 == name).map(|tool| tool.1).ok_or((-32602, format!("no tool `{name}`")))?;
    let mut request = match &params["arguments"] {
        Value::Object(arguments) => Value::Object(arguments.clone()),
        Value::Null => json!({}),
        _ => return Err((-32602, "arguments must be an object".into())),
    };
    request["command"] = command.into();
    if let Some(grant) = grant {
        let parsed = Command::parse(&request).map_err(|error| (-32602, error))?;
        if !grant.allows(&parsed, Relation::Own) {
            return Ok(json!({ "content": [{ "type": "text", "text": "the user has not granted this" }], "isError": true }));
        }
    }
    let (text, error) = match send(request) {
        Ok(reply) => (reply.to_string(), false),
        Err(error) => (format!("{error:#}"), true),
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": error }))
}

/// Serves MCP on standard input and output until the agent closes it. Each request is answered
/// on a thread of its own, so a slow one (a wait, say) doesn't hold the agent's next tool call,
/// and replies go through a writer that owns standard output, so they never tear. Replies may
/// arrive out of order, which JSON-RPC allows: every reply carries the id of its own request.
/// Calls served at once.
const MAX_CALLS: usize = 16;

pub fn serve() -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};
    let (send_line, receive_line) = std::sync::mpsc::channel::<String>();
    let writer = std::thread::spawn(move || {
        let mut out = std::io::stdout().lock();
        for reply in receive_line {
            if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
                break;
            }
        }
    });
    let grant: Arc<Mutex<Option<Grant>>> = Arc::default();
    let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // Calls that take a while (a wait) run beside the rest, but not without end.
        if running.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_CALLS {
            running.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let id = serde_json::from_str::<Value>(&line).ok().and_then(|message| message.get("id").cloned());
            if let Some(id) = id {
                send_line.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "too many calls at once" } }).to_string()).ok();
            }
            continue;
        }
        let (send_line, grant, running) = (send_line.clone(), grant.clone(), running.clone());
        std::thread::spawn(move || {
            let mut mine = *grant.lock().expect("grant lock");
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(message) => handle(&message, &crate::control::send, &mut mine),
                Err(error) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": error.to_string() } })),
            };
            *grant.lock().expect("grant lock") = mine;
            if let Some(reply) = reply {
                send_line.send(reply.to_string()).ok();
            }
            running.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
    drop(send_line);
    writer.join().map_err(|_| anyhow::anyhow!("the writer stopped"))?;
    Ok(())
}

/// Po's binary. After `po update` replaces it under a running window, Linux names the old one
/// with a ` (deleted)` suffix; the new one is at the path without it.
fn own_binary() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    match exe.to_str().and_then(|path| path.strip_suffix(" (deleted)")) {
        Some(path) => Some(path.into()),
        None => Some(exe),
    }
}

/// The entry for Po's own server in ACP's `mcpServers`, carrying `env` (the thread's socket and
/// token). None if Po can't name its own binary.
pub fn server(env: &[(String, String)]) -> Option<Value> {
    let exe = own_binary()?;
    let env: Vec<Value> = env.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect();
    Some(json!({ "name": "po", "command": exe.to_string_lossy(), "args": ["mcp"], "env": env }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn refuse(_: Value) -> anyhow::Result<Value> {
        panic!("must not reach the window")
    }

    fn handle_(message: &Value) -> Option<Value> {
        handle(message, &refuse, &mut None)
    }

    #[test]
    fn it_introduces_itself_in_the_version_asked_for() {
        let reply = handle_(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } })).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "po");
        let reply = handle_(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } })).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], VERSIONS[0]);
    }

    #[test]
    fn notifications_get_no_answer() {
        assert!(handle_(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
    }

    #[test]
    fn every_tool_is_listed_with_a_schema_and_names_a_command_po_reads() {
        let reply = handle_(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
        let listed = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(listed.len(), tools().len());
        assert!(listed.iter().all(|tool| tool["inputSchema"]["type"] == "object"));
        for (_, command, ..) in tools() {
            // Refused for missing arguments, perhaps, but never as an unknown command.
            let parsed = crate::command::Command::parse(&json!({ "command": command }));
            assert!(parsed.as_ref().err().is_none_or(|error| !error.starts_with("no command")), "{command}: {parsed:?}");
        }
    }

    // The tool table carries each tool's scope, and it is the scope of the command it sends: a
    // tool must not promise what its command doesn't ask for.
    #[test]
    fn each_tool_s_scope_is_its_command_s() {
        for (_, command, .., scope) in tools() {
            assert_eq!(scope, parse_command(command).unwrap().scope(), "{command}");
        }
    }

    // A grant lists only the tools it allows, and a call of one it no longer allows is refused
    // before the window hears of it: a viewer lists its reads and views, an orchestrator also
    // the tools that start and steer threads.
    #[test]
    fn a_grant_lists_and_allows_only_its_tools() {
        let viewer = Grant::default();
        let viewer_tools = listed(Some(&viewer));
        assert!(!viewer_tools.iter().any(|tool| tool["name"] == "create_thread"));
        assert!(viewer_tools.iter().any(|tool| tool["name"] == "get_thread"));
        let orchestrate = Grant { scope: Scope::Orchestrate, ..Grant::default() };
        // The project tools are Global's: an orchestrator lists all but those.
        let orchestrate_tools = listed(Some(&orchestrate));
        assert!(orchestrate_tools.iter().any(|tool| tool["name"] == "create_thread"));
        assert!(!orchestrate_tools.iter().any(|tool| tool["name"] == "open_project"));
        // Answering a permission ask takes `approvals` too, whatever the scope.
        let global = Grant { scope: Scope::Global, ..orchestrate };
        assert!(!listed(Some(&global)).iter().any(|tool| tool["name"] == "answer_thread"));
        assert_eq!(listed(Some(&Grant { approvals: true, ..global })).len(), tools().len());
        let sent = RefCell::new(false);
        let send = |_: Value| {
            *sent.borrow_mut() = true;
            Ok(json!({}))
        };
        let mut grant = Some(viewer);
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "create_thread", "arguments": { "prompt": "x" } } }), &send, &mut grant).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "the user has not granted this");
        assert!(!*sent.borrow());
        let mut grant = Some(orchestrate);
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "create_thread", "arguments": { "prompt": "x" } } }), &send, &mut grant).unwrap();
        assert_eq!(reply["result"]["isError"], false);
        assert!(*sent.borrow());
    }

    #[test]
    fn a_call_becomes_one_request_and_a_refusal_is_the_tools_error() {
        let sent = RefCell::new(vec![]);
        let send = |request: Value| {
            sent.borrow_mut().push(request.clone());
            if request["path"] == "nope.rs" { anyhow::bail!("no file `nope.rs` in the project") } else { Ok(json!({ "deferred": true })) }
        };
        let call = |arguments| json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "open_file", "arguments": arguments } });
        let reply = handle(&call(json!({ "path": "src/main.rs", "line": 4, "command": "viewer.close" })), &send, &mut None).unwrap();
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["content"][0]["text"], r#"{"deferred":true}"#);
        // The tool decides the command, whatever the arguments say.
        assert_eq!(sent.borrow()[0], json!({ "command": "file.open", "path": "src/main.rs", "line": 4 }));
        let reply = handle(&call(json!({ "path": "nope.rs" })), &send, &mut None).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "no file `nope.rs` in the project");
    }

    #[test]
    fn unknown_methods_and_tools_are_protocol_errors() {
        let reply = handle_(&json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" })).unwrap();
        assert_eq!(reply["error"]["code"], -32601);
        let mut none = None;
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "rm" } }), &refuse, &mut none).unwrap();
        assert_eq!(reply["error"]["code"], -32602);
    }

    #[test]
    fn the_instructions_say_what_each_scope_may_do() {
        let base = instructions(None);
        assert!(base.contains("control_info") && base.contains("last_reply"));
        assert_eq!(instructions(Some(&Grant::default())), base, "a viewer's instructions are the base");
        let all = instructions(Some(&Grant::user()));
        assert!(all.contains(ORCHESTRATE) && all.contains(GLOBAL));
    }

    #[test]
    fn a_grant_is_read_out_of_control_info() {
        let reply = json!({ "grant": { "scope": "orchestrate", "read_project": false, "approvals": false } });
        assert_eq!(grant_of(&reply), Some(Grant { scope: Scope::Orchestrate, ..Grant::default() }));
        assert_eq!(grant_of(&json!({ "grant": { "scope": "surprise" } })), None);
    }
}
