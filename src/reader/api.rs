//! Methods of [`Reader`], grouped by what they do.

use crate::*;

impl Reader {
    /// The one way a request from outside the window changes it. Views are the caller's own; the
    /// viewer is the user's, so only the thread on screen may move it, and not while they are busy.
    pub(crate) fn run_command(&mut self, caller: command::Caller, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Answer {
        let (name, writes) = (command.name(), command.writes());
        let answer = self.run_command_inner(caller, command, window, cx);
        let outcome = match &answer {
            command::Answer::Now(Ok(_)) => "ok".to_string(),
            command::Answer::Now(Err(why)) => format!("refused: {why}"),
            command::Answer::Later(_) => "waiting".to_string(),
        };
        self.record(caller, name, outcome, writes);
        answer
    }

    pub(crate) fn run_command_inner(&mut self, caller: command::Caller, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Answer {
        log::info!("control: {caller:?} {}", command.name());
        if let Err(why) = self.authorize(caller, &command) {
            log::warn!("control: {caller:?} {} refused: {why}", command.name());
            return command::Answer::Now(Err(why));
        }
        // The user's connection has no thread of its own; its commands act on the one on screen.
        let id = match caller {
            command::Caller::Thread(id) => id,
            command::Caller::User => self.thread.id,
        };
        match command.family() {
            command::Family::View => command::Answer::Now(self.run_view(id, command, window, cx)),
            command::Family::Project if command.writes() => command::Answer::Now(self.run_project(id, command, window, cx)),
            command::Family::Control | command::Family::Agent | command::Family::Project | command::Family::Window => command::Answer::Now(self.run_read(caller, id, command)),
            command::Family::Thread => self.run_thread(caller, id, command, window, cx),
        }
    }

    /// Whether `caller` may run `command` at all: what it has been granted, and how the thread
    /// the command names stands to the caller's own.
    pub(crate) fn authorize(&self, caller: command::Caller, command: &command::Command) -> Result<(), String> {
        let (grant, id) = match caller {
            command::Caller::User => (command::Grant::user(), None),
            command::Caller::Thread(id) => (self.grants.get(&id).copied().unwrap_or_else(command::Grant::initial), Some(id)),
        };
        if command.user_only() && id.is_some() {
            return Err("only the user, from `po ctl`, may ask this".into());
        }
        if command.needs_parent() && id.is_none() {
            return Err("starting, steering and waiting on threads is an agent's; `po ctl` can read, answer, stop and close".into());
        }
        // The user has no thread of their own, so nothing stands between them and a target.
        let relation = match command.target() {
            Some(target) => match id {
                Some(id) => self.relation_to(id, target),
                None => command::Relation::Own,
            },
            None => command::Relation::Own,
        };
        grant.allows(command, relation).then_some(()).ok_or_else(|| "the user has not granted this".to_string())
    }

    /// How `target` stands to thread `caller`: itself, its child, a
    /// sibling thread of its project, or a thread elsewhere.
    pub(crate) fn relation_to(&self, caller: u64, target: u64) -> command::Relation {
        use command::Relation;
        if target == caller {
            return Relation::Own;
        }
        if self.threads_all().any(|thread| thread.id == target && thread.parent == Some(caller)) {
            return Relation::Child;
        }
        // A sibling stands in the same project; a view holding both says so.
        let projects: Vec<Vec<u64>> = self.views().iter().map(|view| view.threads.iter().map(|thread| thread.id).collect()).collect();
        if state::share_project(&projects, caller, target) {
            Relation::Sibling
        } else {
            Relation::Elsewhere
        }
    }

    /// Every thread of every project this window has.
    pub(crate) fn threads_all(&self) -> impl Iterator<Item = &Thread> {
        std::iter::once(&self.thread).chain(self.parked.iter()).chain(self.slots.iter().flat_map(|slot| std::iter::once(&slot.thread).chain(slot.parked.iter())))
    }

    /// The view family: what the caller's thread puts on screen, and the viewer it may move
    /// while its thread is the one being looked at.
    pub(crate) fn run_view(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        use command::Command;
        let reply = if command.moves_viewer() {
            self.with_thread(id, |reader, visible| {
                if !visible {
                    return Err("only the thread on screen can move the viewer".to_string());
                }
                let command = reader.check_move(command)?;
                if reader.last_input.elapsed() < QUIET {
                    reader.defer_move(id, command, window, cx);
                    return Ok(serde_json::json!({ "deferred": true }));
                }
                reader.pending_move = None;
                reader.as_agent(cx, |reader, cx| reader.apply_move(command, window, cx));
                Ok(serde_json::json!({}))
            })
            .unwrap_or_else(|| Err("that thread is gone".into()))
        } else {
            let shows = matches!(command, Command::ViewShow { .. } | Command::FormShow { .. });
            self.with_thread(id, |reader, visible| {
                let reply = match &command {
                    Command::FormShow { id, title, body, fields, submit } => {
                        // A replaced form is filled in again from scratch; the screen seeds its
                        // answers anew wherever the fields (which carry the starting values)
                        // changed.
                        reader.thread.forms.show(id.clone(), title.clone(), body.clone(), fields.clone(), submit.clone())?
                    }
                    _ => reader.thread.views.apply(command.clone())?,
                };
                reader.sync_view_docs(cx);
                if let (true, Some(view)) = (shows, reply["id"].as_str()) {
                    let view = view.to_string();
                    if visible && reader.last_input.elapsed() >= QUIET {
                        reader.as_agent(cx, |reader, cx| {
                            reader.show_agent_view(&view, cx);
                            reader.follow_agent();
                        });
                    } else if reader.thread.shown.as_deref() != Some(&view) {
                        // The user is typing, or this is not the thread they are in: ask first.
                        reader.thread.offered = Some(view);
                    }
                }
                Ok(reply)
            })
            .unwrap_or_else(|| Err("that thread is gone".into()))
        };
        cx.notify();
        reply
    }

    /// The read families: snapshots of this window's threads, agents and projects. They look
    /// and never swap a thread in to change it — every write is its own command, and the
    /// reads answer from the state as it is.
    pub(crate) fn run_read(&self, caller: command::Caller, id: u64, command: command::Command) -> command::Reply {
        use command::{Command, Scope};
        use serde_json::json;
        let grant = self.grant_of(caller);
        // Everything, for the user's connection; otherwise what it was granted.
        let everywhere = matches!(caller, command::Caller::User) || grant.scope == Scope::Global;
        match command {
            Command::ControlInfo => self.read_control_info(id, grant),
            Command::AuditList { limit } => self.read_audit(limit),
            Command::WindowInfo => {
                let projects: Vec<Value> = self.views().iter().map(|view| self.project_info(view).json()).collect();
                Ok(json!({ "pid": std::process::id(), "projects": projects, "on_screen": self.project_id }))
            }
            Command::ThreadFind { session } => self.read_thread_find(id, grant, session),
            Command::AgentList => {
                let agents: Vec<Value> = self
                    .agents
                    .iter()
                    .map(|agent| state::AgentInfo { id: agent.id.clone(), name: agent.name.clone(), trusted: config::trusted(agent), daemon: agent.daemon.is_some() }.json())
                    .collect();
                Ok(json!({ "agents": agents, "default": self.chosen_agent }))
            }
            Command::Issues { op, args } => self.run_issues(id, op, &args),
            Command::ProjectList => {
                let own = self.own_project(id);
                let projects: Vec<Value> = self
                    .views()
                    .iter()
                    .filter(|view| everywhere || own == Some(view.id))
                    .map(|view| self.project_info(view).json())
                    .collect();
                Ok(json!({ "projects": projects }))
            }
            Command::ProjectChanges { project, path } => self.read_project_changes(id, everywhere, project, path),
            Command::ThreadList { project, state } => self.read_thread_list(id, grant, everywhere, project, state),
            Command::ThreadGet { thread } => {
                let want = thread.unwrap_or(id);
                let views = self.views();
                let (view, thread) = find_thread(&views, want).ok_or("no such thread")?;
                Ok(self.thread_info(view, thread).json())
            }
            Command::ThreadOptions { thread } => {
                let want = thread.unwrap_or(id);
                let views = self.views();
                let (_, thread) = find_thread(&views, want).ok_or("no such thread")?;
                let options: Vec<Value> = thread
                    .options
                    .iter()
                    .map(|option| state::OptionInfo { id: option.id.clone(), name: option.name.clone(), category: option.category.clone(), current: option.current.clone(), choices: option.choices.clone() }.json())
                    .collect();
                Ok(json!({ "options": options }))
            }
            Command::ThreadMessages { thread, cursor, limit, full } => self.read_messages(id, thread, cursor, limit, full),
            Command::ThreadLastReply { thread } => self.read_last_reply(id, thread),
            _ => Err("not a read".into()),
        }
    }

    fn read_control_info(&self, id: u64, grant: command::Grant) -> command::Reply {
        use serde_json::json;
        use command::Scope;
        let scope = match grant.scope {
            Scope::Viewer => "viewer",
            Scope::Orchestrate => "orchestrate",
            Scope::Global => "global",
        };
        Ok(json!({
            "grant": { "scope": scope, "read_project": grant.read_project, "approvals": grant.approvals },
            "thread": id,
            "window": std::process::id(),
        }))
    }

    fn read_audit(&self, limit: Option<usize>) -> command::Reply {
        use serde_json::json;
        let skip = self.audit.len().saturating_sub(limit.unwrap_or(50).clamp(1, MAX_AUDIT));
        let entries: Vec<Value> = self
            .audit
            .iter()
            .skip(skip)
            .map(|entry| {
                let who = match entry.caller {
                    command::Caller::User => json!("user"),
                    command::Caller::Thread(id) => json!({ "thread": id }),
                };
                json!({ "at": entry.at, "caller": who, "command": entry.command, "outcome": entry.outcome, "count": entry.count })
            })
            .collect();
        Ok(json!({ "entries": entries }))
    }

    fn read_thread_find(&self, id: u64, grant: command::Grant, session: String) -> command::Reply {
        use serde_json::json;
        use command::Command;
        let views = self.views();
        let found = views.iter().flat_map(|view| view.threads.iter().map(move |thread| (view, *thread))).find(|(_, thread)| thread.session.as_deref() == Some(session.as_str()));
        let (_, thread) = found.ok_or("no thread has that session")?;
        // The same reach as reading it by id, and the same answer when it is out of
        // reach as when there is none, so that this doesn't say which sessions exist.
        if !grant.allows(&Command::ThreadGet { thread: Some(thread.id) }, self.relation_to(id, thread.id)) {
            return Err("no thread has that session".into());
        }
        Ok(json!({ "thread": thread.id }))
    }

    fn read_project_changes(&self, id: u64, everywhere: bool, project: Option<u64>, path: Option<String>) -> command::Reply {
        use serde_json::json;
        let own = self.own_project(id);
        let at = project.or(own).ok_or("that thread is gone")?;
        let views = self.views();
        let view = views.iter().find(|view| view.id == at).ok_or("no such project")?;
        if own != Some(at) && !everywhere {
            return Err("another project's changes are read at Global".into());
        }
        // Read from the repository now: a project that was never on screen has not had
        // its changes listed, and an agent's isolated worktree is just that.
        let changes = diff::changes(&view.project.root, &view.project.base);
        match path {
            Some(path) => {
                let change = changes.iter().find(|change| change.path == path).ok_or(format!("`{path}` has no changes"))?;
                let rows = diff::file_diff(&view.project.root, &view.project.base, change);
                let mut text = String::new();
                for row in rows {
                    if text.len() > state::MAX_DIFF {
                        text.truncate(state::MAX_DIFF);
                        text.push_str("\n…");
                        break;
                    }
                    text.push_str(&match row.kind {
                        diff::Kind::Hunk => format!("{}\n", row.text),
                        diff::Kind::Added => format!("+{}\n", row.text),
                        diff::Kind::Removed => format!("-{}\n", row.text),
                        diff::Kind::Context => format!(" {}\n", row.text),
                    });
                }
                Ok(json!({ "path": path, "diff": text }))
            }
            None => Ok(json!({
                "files": changes.iter().map(|change| json!({ "path": change.path, "status": change.status })).collect::<Vec<_>>(),
            })),
        }
    }

    fn read_thread_list(&self, id: u64, grant: command::Grant, everywhere: bool, project: Option<u64>, state: Option<String>) -> command::Reply {
        use serde_json::json;
        use command::Command;
        let own = self.own_project(id);
        let want = state.as_deref().and_then(crate::state::ThreadState::from_name);
        let mut threads = Vec::new();
        for view in self.views() {
            if !(everywhere || own == Some(view.id)) || project.is_some_and(|want| want != view.id) {
                continue;
            }
            for thread in &view.threads {
                // The same reach table as every other read: itself, its children, and
                // its project's other threads only with `read_project`.
                if !grant.allows(&Command::ThreadGet { thread: Some(thread.id) }, self.relation_to(id, thread.id)) {
                    continue;
                }
                let info = self.thread_info(&view, thread);
                if want.is_some_and(|want| want != info.state) {
                    continue;
                }
                threads.push(info.json());
            }
        }
        Ok(json!({ "threads": threads }))
    }

    fn read_messages(&self, id: u64, thread: Option<u64>, cursor: Option<String>, limit: Option<usize>, full: Option<bool>) -> command::Reply {
        use serde_json::json;
        let want = thread.unwrap_or(id);
        let views = self.views();
        let (_, thread) = find_thread(&views, want).ok_or("no such thread")?;
        let all = transcript(thread, full.is_some_and(|full| full));
        let (reset, start) = match cursor.as_deref().and_then(|cursor| cursor.split_once(':')) {
            Some((generation, index)) if generation.parse::<u64>() == Ok(thread.generation) => (false, index.parse::<usize>().unwrap_or(0)),
            Some(_) => (true, 0), // the transcript was cleared or replaced since
            None => (false, 0),
        };
        let limit = limit.unwrap_or(state::DEFAULT_MESSAGES).max(1);
        let entries: Vec<Value> = all.iter().skip(start).take(limit).map(|entry| entry.json()).collect();
        let more = start + entries.len() < all.len();
        Ok(json!({
            "entries": entries,
            "next": more.then(|| format!("{}:{}", thread.generation, start + entries.len())),
            "reset": reset,
        }))
    }

    fn read_last_reply(&self, id: u64, thread: Option<u64>) -> command::Reply {
        use serde_json::json;
        let want = thread.unwrap_or(id);
        let views = self.views();
        let (_, thread) = find_thread(&views, want).ok_or("no such thread")?;
        let text = thread
            .messages
            .iter()
            .rev()
            .find_map(|msg| match msg {
                Msg::Agent(blocks) => {
                    let said: Vec<&str> = blocks.iter().filter_map(|block| match block {
                        Block::Text(text) => Some(text.as_str()),
                        _ => None,
                    }).collect();
                    (!said.is_empty()).then(|| said.join("\n\n"))
                }
                _ => None,
            })
            .unwrap_or_default();
        let mut text = text;
        if text.len() > command::MAX_BODY {
            text.truncate(command::MAX_BODY);
            text.push_str("\n…");
        }
        Ok(json!({ "text": text, "state": state::state(&thread.facts()).name() }))
    }

    pub(crate) fn grant_of(&self, caller: command::Caller) -> command::Grant {
        match caller {
            command::Caller::User => command::Grant::user(),
            command::Caller::Thread(id) => self.grants.get(&id).copied().unwrap_or_else(command::Grant::initial),
        }
    }

    /// The project writes: opening, closing and offering projects. None of it switches the
    /// screen; a folder that is not a project yet waits for the user's click.
    pub(crate) fn run_project(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        use command::Command;
        use serde_json::json;
        match command {
            Command::Issues { op: issues::api::Op::IssueWork, args } => self.work_issue(id, &args, window, cx),
            Command::Issues { op: issues::api::Op::IssueShow, args } => self.show_issues(id, &args, window, cx),
            Command::Issues { op, args } => self.run_issues(id, op, &args),
            Command::ProjectOpen { path: None, pr: Some(spec) } => {
                // Reviewed from the checkout the caller is in, as the PR strip does.
                let from = self.project_of(id).and_then(|project| self.project_by_id(project).map(|project| project.root.clone())).unwrap_or_else(|| self.home.clone());
                self.suggest(id, Proposal::Pr { spec, from }, cx)?;
                Ok(json!({ "confirm": "asked" }))
            }
            Command::WorktreeMerge { project } => {
                if self.project_by_id(project).and_then(|project| project.worktree.as_ref()).is_none() {
                    return Err("that project is not a worktree".into());
                }
                self.suggest(id, Proposal::Merge(project), cx)?;
                Ok(json!({ "confirm": "asked" }))
            }
            Command::ProjectOpen { path: Some(path), .. } => {
                let root = std::path::Path::new(&path).canonicalize().map_err(|_| format!("`{path}` is not a folder"))?;
                if !root.is_dir() {
                    return Err(format!("`{path}` is not a folder"));
                }
                if root.parent().is_none() || config::home().is_some_and(|home| home.canonicalize().is_ok_and(|home| home == root)) {
                    return Err("not the root or the home directory".into());
                }
                if let Some(view) = self.views().iter().find(|view| view.project.pr.is_none() && view.project.root == root) {
                    return Ok(json!({ "project": view.id, "opened": false }));
                }
                self.suggest(id, Proposal::Open(root), cx)?;
                Ok(json!({ "confirm": "asked" }))
            }
            Command::ProjectOffer { project } => {
                if !self.views().iter().any(|view| view.id == project) {
                    return Err("no such project".into());
                }
                self.suggest(id, Proposal::Go(project), cx)?;
                Ok(json!({}))
            }
            Command::ProjectClose { project } => {
                if !self.api_projects.contains(&project) {
                    return Err("only a project the API opened can be closed by it".into());
                }
                // The user's own threads (any that nobody started and that were used) keep it.
                let views = self.views();
                let view = views.iter().find(|view| view.id == project).ok_or("no such project")?;
                if view.threads.iter().any(|thread| thread.parent.is_none() && !thread.blank) {
                    return Err("the user has threads in it".into());
                }
                self.api_projects.remove(&project);
                self.suggestions.retain(|suggestion| !matches!(suggestion.what, Proposal::Go(p) if p == project));
                self.close_project(project, window, cx);
                Ok(json!({}))
            }
            _ => Err("not a project command".into()),
        }
    }

    /// The issue and epic commands, on the issues of the repo the caller works in (the project on
    /// screen for the user's own `po ctl`).
    pub(crate) fn run_issues(&self, id: u64, op: issues::api::Op, args: &serde_json::Value) -> command::Reply {
        let root = self.issues_root_of(id);
        let today = date::today();
        issues::shared::change_repo(&root, |tracker, now| issues::api::run(op, args, tracker, now, today))
    }

    /// The project the caller works in, whose issues its commands act on; the one on screen for
    /// the user's own `po ctl`.
    pub(crate) fn issues_root_of(&self, id: u64) -> PathBuf {
        self.own_project(id).and_then(|project| self.views().iter().find(|view| view.id == project).map(|view| view.project.root.clone())).unwrap_or_else(|| self.project.root.clone())
    }

    /// `thread.create_isolated`: the worktree is made off the UI thread, then the thread starts
    /// in a project of its own, and the caller is told how it went.
    pub(crate) fn create_isolated(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let command::Command::ThreadCreateIsolated { prompt, agent, options, notify } = command else {
            return Err("not a thread.create_isolated".into());
        };
        self.create_isolated_for(id, prompt, agent, options, notify, None, window, cx)
    }

    /// `issue.work`: a thread in a worktree of its own on an issue, whose branch is linked to the
    /// issue and which is moved to In Progress once the worktree is up.
    pub(crate) fn work_issue(&mut self, id: u64, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let root = self.issues_root_of(id);
        let (number, prompt, key) = issues::shared::change_repo(&root, |tracker, _| {
            let number = issues::api::work_issue(tracker, args)?;
            Ok::<_, String>((number, issues::api::work_prompt(tracker, number).ok_or("that issue is gone")?, tracker.key(number)))
        })?;
        let options = args["options"].as_object().map(|options| options.iter().filter_map(|(id, value)| value.as_str().map(|value| (id.clone(), value.to_string()))).collect()).unwrap_or_default();
        let agent = args["agent"].as_str().map(String::from);
        let notify = args["notify"].as_str().map(String::from);
        let mut reply = self.create_isolated_for(id, prompt, agent, options, notify, Some((root, number)), window, cx)?;
        reply["issue"] = serde_json::json!(key);
        Ok(reply)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_isolated_for(&mut self, id: u64, prompt: String, agent: Option<String>, options: Vec<(String, String)>, notify: Option<String>, issue: Option<(PathBuf, u64)>, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let key = self.child_agent(id, agent)?;
        let project = self.project_of(id).ok_or("that thread is gone")?;
        if self.finishing || self.isolating {
            return Err("a worktree is being made or merged; try again in a moment".into());
        }
        // A worktree is made from the repository, not from another worktree.
        let from = {
            let project = self.project_by_id(project).ok_or("that thread is gone")?;
            match &project.worktree {
                Some(wt) => wt.spec.repo.clone(),
                None => project.root.clone(),
            }
        };
        self.isolating = true;
        let request = self.next_request;
        self.next_request += 1;
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let made = cx.background_executor().spawn(async move { worktree::Worktree::create(&from) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.isolating = false;
                        let told = match made {
                            Ok(wt) => {
                                let branch = wt.spec.branch.clone();
                                let started = reader.start_isolated(id, wt, key, prompt, options, notify, window, cx);
                                if let (Ok(_), Some((root, number))) = (&started, &issue) {
                                    Self::issue_started(root, *number, Some(branch));
                                }
                                started
                            }
                            Err(error) => Err(error),
                        };
                        let text = match told {
                            Ok((thread, project)) => format!("Isolated thread #{thread} is up in project {project} (request {request})."),
                            Err(error) => format!("Isolated thread (request {request}) could not start: {error}"),
                        };
                        reader.tell(id, text, window, cx);
                    })
                })
                .ok();
        })
        .detach();
        Ok(serde_json::json!({ "creating": true, "request": request }))
    }

    /// Puts a made worktree up as a project and starts the child in it; returns the thread and
    /// project. If the parent went away meanwhile the project goes too, keeping its branch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_isolated(&mut self, parent: u64, wt: worktree::Worktree, key: String, prompt: String, options: Vec<(String, String)>, notify: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Result<(u64, u64), String> {
        let mut project = Self::build_project(wt.spec.path.clone(), None, vec![]);
        project.base = wt.spec.base.clone();
        project.worktree = Some(wt);
        let pid = self.add_slot(project, Mode::Diff, cx);
        match self.start_child(parent, pid, key, prompt, options, notify, window, cx) {
            Ok(reply) => Ok((reply["thread"].as_u64().unwrap_or_default(), pid)),
            Err(error) => {
                self.api_projects.remove(&pid);
                self.close_project(pid, window, cx);
                Err(error)
            }
        }
    }

    /// Says `text` to thread `id` as Po, wherever that thread is.
    pub(crate) fn tell(&mut self, id: u64, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let out = Outgoing { text, origin: Origin::Po, images: vec![] };
        self.with_thread(id, |reader, _| reader.submit(out, false, window, cx));
    }

    /// Puts `what` to the user from thread `from`, once: asking again for the same thing only
    /// makes it the newest.
    pub(crate) fn suggest(&mut self, from: u64, what: Proposal, cx: &mut Context<Self>) -> Result<(), String> {
        let same = |a: &Proposal, b: &Proposal| match (a, b) {
            (Proposal::Go(a), Proposal::Go(b)) => a == b,
            (Proposal::Open(a), Proposal::Open(b)) => a == b,
            (Proposal::Pr { spec: a, .. }, Proposal::Pr { spec: b, .. }) => a == b,
            (Proposal::Merge(a), Proposal::Merge(b)) => a == b,
            _ => false,
        };
        self.suggestions.retain(|suggestion| !(suggestion.from == from && same(&suggestion.what, &what)));
        if self.suggestions.len() >= MAX_SUGGESTIONS {
            return Err("the user has enough waiting; try again after they answer".into());
        }
        let by = self.title_of(from).unwrap_or_else(|| "A thread".into());
        let id = self.next_suggestion;
        self.next_suggestion += 1;
        self.suggestions.push(Suggestion { id, from, by, what });
        cx.notify();
        Ok(())
    }

    /// The user's answer to a suggestion. Accepting a folder opens it in the background and the
    /// thread that asked is told either way; accepting a project goes to it.
    pub(crate) fn answer_suggestion(&mut self, id: u64, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.suggestions.iter().position(|suggestion| suggestion.id == id) else { return };
        let Suggestion { from, what, .. } = self.suggestions.remove(at);
        let from_thread = from;
        let told = match (what, yes) {
            (Proposal::Go(project), true) => {
                self.show_project(project, window, cx);
                None
            }
            (Proposal::Go(_), false) => None,
            (Proposal::Open(root), true) => {
                let project = self.open_in_background(root.clone(), cx);
                Some(format!("The user opened {}: project {project}.", root.display()))
            }
            (Proposal::Open(root), false) => Some(format!("The user declined to open {}.", root.display())),
            (Proposal::Pr { spec, from }, true) => {
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    let result = cx.background_executor().spawn({
                        let spec = spec.clone();
                        async move { pr::Pr::open(&from, &spec) }
                    }).await;
                    handle
                        .update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                let text = match result {
                                    Ok(pr) => {
                                        let root = pr.worktree.clone();
                                        let project = Self::build_project(root, Some(pr), vec![]);
                                        let id = reader.add_slot(project, Mode::Diff, cx);
                                        format!("The user opened PR {spec}: project {id}.")
                                    }
                                    Err(error) => format!("PR {spec} could not be checked out: {error}"),
                                };
                                reader.tell(from_thread, text, window, cx);
                            })
                        })
                        .ok();
                })
                .detach();
                None
            }
            (Proposal::Pr { spec, .. }, false) => Some(format!("The user declined to open PR {spec}.")),
            (Proposal::Merge(project), true) => {
                self.finish_worktree_of(project, true, Some(from), window, cx);
                None
            }
            (Proposal::Merge(project), false) => Some(format!("The user declined to merge project {project}.")),
        };
        if let Some(text) = told {
            self.tell(from, text, window, cx);
        }
        cx.notify();
    }

    /// Adds a project over `root` as a slot, with a blank thread, leaving the screen where it is.
    /// If it is already open, that one is returned.
    pub(crate) fn open_in_background(&mut self, root: PathBuf, cx: &mut Context<Self>) -> u64 {
        if let Some(view) = self.views().iter().find(|view| view.project.pr.is_none() && view.project.root == root) {
            return view.id;
        }
        let project = Self::build_project(root, None, vec![]);
        self.add_slot(project, Mode::Source, cx)
    }

    /// Adds `project` as a slot with a blank thread, leaving the screen where it is, and marks
    /// it as the API's.
    pub(crate) fn add_slot(&mut self, project: Project, mode: Mode, cx: &mut Context<Self>) -> u64 {
        let thread = self.blank_thread();
        let id = self.next_project;
        self.next_project += 1;
        self.slots.push(Slot { id, mode, project, thread, parked: vec![] });
        self.api_projects.insert(id);
        self.request_save(cx);
        cx.notify();
        id
    }

    /// The thread family: what an orchestrator does with the threads it started. Every write
    /// runs on the UI thread through the swap, and nothing here switches the screen.
    pub(crate) fn run_thread(&mut self, caller: command::Caller, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Answer {
        use command::{Answer, Command};
        // A wait's deadline is capped with the command's own timeout, judged before the
        // command is taken apart.
        let deadline = std::time::Instant::now() + command.timeout();
        match command {
            Command::ThreadCreate { .. } => Answer::Now(self.create_thread(id, command, window, cx)),
            Command::ThreadCreateIsolated { .. } => Answer::Now(self.create_isolated(id, command, window, cx)),
            Command::ThreadSend { thread: target, text, later } => Answer::Now(self.send_to_thread(id, target, text, later, window, cx)),
            Command::ThreadAnswer { thread: target, option } => {
                Answer::Now(self.not_self(caller, id, target, "answer").and_then(|()| self.answer_thread(target, option, window, cx)))
            }
            Command::ThreadCancel { thread: target } => Answer::Now(self.not_self(caller, id, target, "cancel").and_then(|()| self.cancel_thread(target, cx))),
            Command::ThreadSetOption { thread, id: setting, value } => Answer::Now(self.set_thread_option(id, thread, setting, value, window, cx)),
            Command::ThreadClose { thread: target } => {
                Answer::Now(self.not_self(caller, id, target, "close").and_then(|()| self.close_thread_by_api(target, window, cx)))
            }
            Command::ThreadWait { threads, until, timeout: _ } => match self.wait_on_threads(caller, id, threads, until, deadline) {
                Ok(wait) => Answer::Later(wait),
                Err(why) => Answer::Now(Err(why)),
            },
            Command::FormWait { id: form, timeout: _ } => self.wait_on_form(caller, id, form, deadline),
            // The reads of threads (list, get, messages…) are answered from the state as it is.
            read => Answer::Now(self.run_read(caller, id, read)),
        }
    }

    fn send_to_thread(&mut self, id: u64, target: u64, text: String, later: Option<bool>, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        if target == id {
            return Err("send to a thread you started, not to yourself".into());
        }
        let Some(title) = self.title_of(id) else { return Err("that thread is gone".into()) };
        let out = Outgoing { text, origin: Origin::Thread { id, title }, images: vec![] };
        let sent = self.with_thread(target, |reader, _| {
            // The waiters and the parent hear when this starts or ends the child's turn.
            Some(reader.observed(window, cx, |reader, window, cx| reader.submit(out, later.unwrap_or(false), window, cx)))
        });
        match sent.flatten() {
            Some(Where::Dispatch) => Ok(serde_json::json!({ "state": "sent" })),
            Some(Where::Wait | Where::Start) => Ok(serde_json::json!({ "state": "pending" })),
            Some(_) => Ok(serde_json::json!({ "state": "queued" })),
            None => Err("no such thread".into()),
        }
    }

    fn answer_thread(&mut self, target: u64, option: String, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let answered = self.with_thread(target, |reader, _| {
            reader.observed(window, cx, |reader, _, cx| {
                // Only a permission ask: the user's approval of a command or an MCP
                // server is a different block, and not reachable from here.
                let found = reader.thread.messages.iter().enumerate().rev().find_map(|(msg, entry)| {
                    let Msg::Agent(blocks) = entry else { return None };
                    blocks.iter().enumerate().find_map(|(block, b)| match b {
                        Block::Permission(ask) if ask.answered.is_none() => Some((msg, block, ask.options.iter().position(|o| o.id == option))),
                        _ => None,
                    })
                });
                match found {
                    None => Err("that thread is not asking".to_string()),
                    Some((_, _, None)) => Err(format!("`{option}` is not one of its options; thread.get lists them")),
                    Some((msg, block, Some(at))) => {
                        reader.answer(msg, block, at, cx);
                        Ok(serde_json::json!({}))
                    }
                }
            })
        });
        answered.unwrap_or_else(|| Err("no such thread".into()))
    }

    fn cancel_thread(&mut self, target: u64, cx: &mut Context<Self>) -> command::Reply {
        let stopped = self.with_thread(target, |reader, _| {
            reader.stop(cx);
            let dropped: Vec<Value> = reader.thread.dropped.drain(..).map(serde_json::Value::String).collect();
            Ok(serde_json::json!({ "dropped": dropped }))
        });
        stopped.unwrap_or_else(|| Err("no such thread".into()))
    }

    fn set_thread_option(&mut self, id: u64, thread: Option<u64>, setting: String, value: String, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let target = thread.unwrap_or(id);
        let stored = self.with_thread(target, |reader, _| {
            let thread = &mut reader.thread;
            let Some(option) = thread.options.iter().find(|option| option.id == setting) else {
                return Err(format!("no option `{setting}` here; thread.options lists them"));
            };
            if !option.choices.iter().any(|(choice, _)| *choice == value) {
                return Err(format!("`{value}` is not one of `{setting}`'s choices"));
            }
            // A mode may be the agent's permissions (one that skips asking, say): what
            // an agent may do without the user's click is the user's to change.
            if option.category == "mode" {
                return Err("a thread's mode is the user's to change".into());
            }
            thread.overrides.insert(setting.clone(), value.clone());
            thread.api_options.insert(setting.clone());
            Ok(())
        });
        match stored {
            Some(Ok(())) => {
                // A thread that is ready and idle takes it now; one that is busy
                // applies what it owes between turns.
                self.with_thread(target, |reader, _| {
                    if reader.thread.ready && !reader.thread.busy && reader.thread.client.is_some() {
                        reader.apply_overrides(window, cx);
                    }
                    Ok(serde_json::json!({ "pending": true }))
                })
                .unwrap_or_else(|| Err("no such thread".into()))
            }
            Some(Err(why)) => Err(why),
            None => Err("no such thread".into()),
        }
    }

    fn close_thread_by_api(&mut self, target: u64, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        match self.project_of(target) {
            Some(project) => {
                self.close_any_thread(project, target, window, cx);
                Ok(serde_json::json!({}))
            }
            None => Err("no such thread".into()),
        }
    }


    /// Refuses an orchestrator's command that names itself: these are for the threads it started.
    fn not_self(&self, caller: command::Caller, id: u64, target: u64, verb: &str) -> Result<(), String> {
        if matches!(caller, command::Caller::Thread(_)) && target == id {
            return Err(format!("{verb} a thread you started, not yourself"));
        }
        Ok(())
    }

    /// Whether the caller may hold one more wait: two each, [`MAX_WAITS`] in all.
    fn wait_room(&self, caller: command::Caller) -> Result<(), String> {
        if self.waiters.iter().filter(|waiter| waiter.caller == caller).count() >= 2 {
            return Err("at most 2 waits at a time".into());
        }
        if self.waiters.len() >= MAX_WAITS {
            return Err("Po is already holding its share of waits".into());
        }
        Ok(())
    }

    fn wait_on_threads(&self, caller: command::Caller, id: u64, threads: Option<Vec<u64>>, until: Option<String>, deadline: std::time::Instant) -> Result<command::Wait, String> {
        self.wait_room(caller)?;
        // Waits target children only; a wait on the caller itself never ends well.
        for target in threads.iter().flatten() {
            if *target == id {
                return Err("waiting on yourself would never end".into());
            }
            if !self.threads_all().any(|thread| thread.id == *target && thread.parent == Some(id)) {
                return Err(format!("thread {target} is not one you started"));
            }
        }
        // Until a state, or `any` change; nothing said means "left running or starting".
        let until = match until.as_deref() {
            Some("any") => vec![],
            Some("done") | None => vec!["ready", "asking", "approval", "failed", "asleep", "closed"],
            Some(state) => vec![crate::state::ThreadState::from_name(state).expect("checked").name()],
        };
        Ok(command::Wait { threads: threads.unwrap_or_default(), until, deadline, form: None })
    }

    fn wait_on_form(&mut self, caller: command::Caller, id: u64, form: String, deadline: std::time::Instant) -> command::Answer {
        if let Err(why) = self.wait_room(caller) {
            return command::Answer::Now(Err(why));
        }
        // A form the user has answered already answers now; one still open is waited on.
        let state = self.with_thread(id, |reader, _| reader.thread.forms.get(&form).and_then(|form| form.answered.clone()));
        match state {
            Some(Some(values)) => command::Answer::Now(Ok(values)),
            Some(None) => command::Answer::Later(command::Wait { threads: vec![], until: vec![], deadline, form: Some(form) }),
            None => command::Answer::Now(Err(format!("no form `{form}`; show_form returns its id"))),
        }
    }

    /// Holds a wait the window answered later, and starts its timer.
    pub(crate) fn add_waiter(&mut self, caller: command::Caller, wait: command::Wait, reply: std::sync::mpsc::Sender<command::Reply>, _window: &mut Window, cx: &mut Context<Self>) {
        let (token, handle) = (self.next_waiter, self.window);
        self.next_waiter += 1;
        let left = wait.deadline.saturating_duration_since(std::time::Instant::now());
        self.waiters.push(Waiter { token, caller, threads: wait.threads, until: wait.until, form: wait.form, reply });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(left).await;
            let _ = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.time_out(token, window, cx)));
        })
        .detach();
    }

    /// Keeps a control command in the audit log: what changes something, and anything refused;
    /// plain reads that were answered are not what the log is for, and would let a caller
    /// push the rest out. The same thing twice in a row is counted, not repeated.
    pub(crate) fn record(&mut self, caller: command::Caller, command: &'static str, outcome: String, writes: bool) {
        if !writes && outcome == "ok" {
            return;
        }
        if let Some(last) = self.audit.back_mut()
            && last.caller == caller
            && last.command == command
            && last.outcome == outcome
        {
            last.count += 1;
            last.at = now_ms();
            return;
        }
        if self.audit.len() >= MAX_AUDIT {
            self.audit.pop_front();
        }
        self.audit.push_back(Audit { at: now_ms(), caller, command, outcome, count: 1 });
    }

    /// Replies to a wait that ran out, unless it was already answered.
    pub(crate) fn time_out(&mut self, token: u64, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(at) = self.waiters.iter().position(|waiter| waiter.token == token) {
            let waiter = self.waiters.remove(at);
            waiter.reply.send(Ok(serde_json::json!({ "timed_out": true }))).ok();
        }
    }

    /// Ends the waits of a caller whose turn stopped, failed or exited, or whose grant was
    /// lowered: they would never be answered in time, and the caller is told why.
    pub(crate) fn drop_waiters(&mut self, caller: command::Caller, why: &str) {
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            if waiter.caller == caller {
                waiter.reply.send(Err(format!("cancelled: {why}"))).ok();
            } else {
                kept.push(waiter);
            }
        }
        self.waiters = kept;
    }

    /// Answers the waits `id`'s change satisfies. A waiter whose receiving side is gone is
    /// dropped here, rather than held for a sweep.
    pub(crate) fn wake_waiters(&mut self, id: u64, after: crate::state::ThreadState) {
        // The parent of the thread that changed, computed before the waits are taken out, so
        // the two looks at the threads don't meet.
        let parent = self.threads_all().find(|thread| thread.id == id).and_then(|thread| thread.parent);
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            // A form wait is answered by its user's submit, not by a thread's change of state.
            if waiter.form.is_some() {
                kept.push(waiter);
                continue;
            }
            let watched = matches!(waiter.caller, command::Caller::User)
                || matches!(waiter.caller, command::Caller::Thread(caller) if parent == Some(caller));
            let hit = (waiter.threads.contains(&id) || (waiter.threads.is_empty() && watched))
                && (waiter.until.is_empty() || waiter.until.contains(&after.name()));
            if hit && waiter.reply.send(Ok(serde_json::json!({ "thread": id, "state": after.name(), "timed_out": false }))).is_ok() {
                continue;
            }
            kept.push(waiter);
        }
        self.waiters = kept;
    }

    /// Answers the waits on form `id` of thread `owner`, which the user just submitted.
    pub(crate) fn wake_form(&mut self, owner: u64, id: &str, reply: Value) {
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            let hit = waiter.form.as_deref() == Some(id) && waiter.caller == command::Caller::Thread(owner);
            if hit && waiter.reply.send(Ok(reply.clone())).is_ok() {
                continue;
            }
            kept.push(waiter);
        }
        self.waiters = kept;
    }

    /// Ends the waits on form `id` of thread `owner`, which was closed without an answer.
    pub(crate) fn drop_form(&mut self, owner: u64, id: &str) {
        let mut kept = Vec::new();
        for waiter in self.waiters.drain(..) {
            let hit = waiter.form.as_deref() == Some(id) && waiter.caller == command::Caller::Thread(owner);
            if hit {
                waiter.reply.send(Err("cancelled: the form was closed".into())).ok();
                continue;
            }
            kept.push(waiter);
        }
        self.waiters = kept;
    }

    /// Runs `work` on the thread swapped in, watching its state: if it changed, the waiters
    /// and the parent hear about it.
    pub(crate) fn observed<R>(&mut self, window: &mut Window, cx: &mut Context<Self>, work: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) -> R) -> R {
        let before = crate::state::state(&self.thread.facts());
        let out = work(self, window, cx);
        let after = crate::state::state(&self.thread.facts());
        if before != after {
            self.changed(self.thread.id, after, window, cx);
        }
        out
    }

    /// Tells whoever listens that `id` now stands in `after`: its waiters, then its parent.
    pub(crate) fn changed(&mut self, id: u64, after: crate::state::ThreadState, window: &mut Window, cx: &mut Context<Self>) {
        self.wake_waiters(id, after);
        self.publish(id, after);
        self.notify_parent(id, after, window, cx);
    }

    /// Tells every `po ctl watch` that thread `id` is now in `after`; one that has hung up is
    /// dropped.
    pub(crate) fn publish(&mut self, id: u64, after: crate::state::ThreadState) {
        if self.subscribers.is_empty() {
            return;
        }
        let event = serde_json::json!({
            "event": "thread.state",
            "thread": id,
            "state": after.name(),
            "title": self.title_of(id),
            "project": self.project_of(id),
            "at": now_ms(),
        });
        self.subscribers.retain(|subscriber| subscriber.send(Ok(event.clone())).is_ok());
    }

    /// Puts a line about `id`'s state on the notice of the thread that started it, unless it
    /// asked not to hear or the change isn't one it asked about.
    pub(crate) fn notify_parent(&mut self, id: u64, after: crate::state::ThreadState, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((Some(parent), level)) = self.with_thread(id, |reader, _| Some((reader.thread.parent, reader.thread.notify))).flatten() else { return };
        if level == Notify::Never {
            return;
        }
        // `done` hears when the turn is over, the child asks, or it is failed, asleep or
        // closed; `all` also hears that a turn started. A parent's own messages never quote
        // the child: what it said is read with last_reply, on purpose.
        if level == Notify::Done && matches!(after, crate::state::ThreadState::Running | crate::state::ThreadState::Starting) {
            return;
        }
        let word = match after {
            crate::state::ThreadState::Running => "started a turn",
            crate::state::ThreadState::Asking | crate::state::ThreadState::Approval => "asks for an answer",
            crate::state::ThreadState::Failed => "failed",
            crate::state::ThreadState::Asleep => "went to sleep",
            crate::state::ThreadState::Closed => "is closed",
            crate::state::ThreadState::Starting => "is starting",
            crate::state::ThreadState::Ready => "finished its turn",
        };
        let line = format!("Thread #{id} {word}. Use last_reply or get_thread to read what it said.");
        let notice = self.notices.entry(parent).or_insert_with(|| Notice { text: String::new() });
        if !notice.text.is_empty() {
            notice.text.push('\n');
        }
        notice.text.push_str(&line);
        // Changes that happen within the coalesce window go out together.
        if self.notice_scheduled.insert(parent) {
            let (handle, parent) = (self.window, parent);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTICE_COALESCE).await;
                let _ = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.flush_notice(parent, window, cx)));
            })
            .detach();
        }
    }

    /// Sends a parent the notice it has been waiting for, once the rate limit lets it speak.
    pub(crate) fn flush_notice(&mut self, parent: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.notices.contains_key(&parent) {
            self.notice_scheduled.remove(&parent);
            return;
        }
        // Every notice is a turn of the parent's: more frequent ones merge into the next.
        if let Some(left) = notice_wait(self.notice_sent_at.get(&parent).map(Instant::elapsed)) {
            let (handle, parent) = (self.window, parent);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(left).await;
                let _ = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.flush_notice(parent, window, cx)));
            })
            .detach();
            return;
        }
        let Some(notice) = self.notices.remove(&parent) else { return };
        self.notice_scheduled.remove(&parent);
        self.notice_sent_at.insert(parent, Instant::now());
        let out = Outgoing { text: notice.text, origin: Origin::Po, images: vec![] };
        self.with_thread(parent, |reader, _| {
            reader.submit(out, false, window, cx);
            Some(())
        });
        cx.notify();
    }

    /// What a thread is called, for labelling text it sends to a child.
    pub(crate) fn title_of(&self, id: u64) -> Option<String> {
        self.threads_all().find(|thread| thread.id == id).map(|thread| thread.title())
    }

    /// The project `id`'s thread belongs to, if it is still here.
    pub(crate) fn project_of(&self, id: u64) -> Option<u64> {
        self.views().into_iter().find(|view| view.threads.iter().any(|thread| thread.id == id)).map(|view| view.id)
    }

    /// The project the caller's thread belongs to, if it is still here.
    pub(crate) fn own_project(&self, id: u64) -> Option<u64> {
        self.views().into_iter().find(|view| view.threads.iter().any(|thread| thread.id == id)).map(|view| view.id)
    }

    /// One thread as the read model sees it: a snapshot of the fields an outsider gets.
    pub(crate) fn thread_info(&self, view: &View<'_>, thread: &Thread) -> state::ThreadInfo {
        let asking = thread.messages.iter().rev().find_map(|msg| {
            let Msg::Agent(blocks) = msg else { return None };
            blocks.iter().find_map(|block| match block {
                Block::Permission(Ask { request: _, title, options, answered: None }) => {
                    Some(state::AskInfo { title: title.clone(), options: options.iter().map(|o| state::AskOption { id: o.id.clone(), name: o.name.clone(), kind: o.kind.clone() }).collect() })
                }
                _ => None,
            })
        });
        let children = self.threads_all().filter(|child| child.parent == Some(thread.id)).map(|child| child.id).collect();
        state::ThreadInfo {
            id: thread.id,
            project: view.id,
            title: thread.title(),
            agent: thread.agent.clone().unwrap_or_default(),
            state: state::state(&thread.facts()),
            unread: thread.unread,
            busy: thread.busy,
            status: thread.status.clone(),
            last_error: thread.last_error.clone(),
            session: thread.session.clone(),
            preview: thread.preview(),
            queued: thread.queue.len(),
            dropped: thread.dropped.clone(),
            asking,
            parent: thread.parent,
            children,
            active_at: thread.active_at,
            on_screen: thread.id == view.shown,
            usage: thread.usage,
        }
    }

    /// One project as the read model sees it.
    pub(crate) fn project_info(&self, view: &View<'_>) -> state::ProjectInfo {
        state::ProjectInfo {
            id: view.id,
            root: view.project.root.to_string_lossy().into_owned(),
            name: view.project.name(),
            branch: match &view.project.worktree {
                Some(wt) => Some(wt.spec.branch.clone()),
                None => self.heads.get(&view.project.root).map(|head| head.name.clone()).or(view.project.pr.as_ref().map(|_| String::new())),
            },
            // The project on screen is watched, so its list is current; another has not had its
            // changes listed (an agent's worktree never has), so it is asked of the repository.
            dirty: if view.id == self.project_id { !view.project.changes.is_empty() } else { !diff::changes(&view.project.root, &view.project.base).is_empty() },
            pr: view.project.pr.as_ref().map(|pr| pr.number),
            worktree_of: view.project.worktree.as_ref().map(|wt| {
                self.views().iter().find(|other| other.project.root == wt.spec.repo).map(|other| other.id).unwrap_or(0)
            }),
            on_screen: view.id == self.project_id,
            threads: view.threads.len(),
        }
    }

    /// Sets what thread `id`'s agent may do through Po's API. Held in memory only, so it is
    /// lost on restart and is not shared between windows, and set only here, from the UI —
    /// never from anything an agent could write to. A raise applies when the agent next starts
    /// (its tool list is fixed then); a lowering, on its next request.
    pub(crate) fn set_grant(&mut self, id: u64, scope: command::Scope, read_project: bool, approvals: bool, cx: &mut Context<Self>) {
        let grant = command::Grant { scope, read_project, approvals };
        // Any reach taken away, not only the scope: the waits asked under it would answer what
        // the thread can no longer ask.
        let before = self.grants.get(&id).copied().unwrap_or_else(command::Grant::initial);
        let lowered = before.scope > grant.scope || (before.read_project && !grant.read_project) || (before.approvals && !grant.approvals);
        if self.grants.get(&id) == Some(&grant) {
            return;
        }
        self.grants.insert(id, grant);
        // A lowered grant ends the thread's waits; they would answer what it can no longer ask.
        if lowered {
            self.drop_waiters(command::Caller::Thread(id), "the grant was lowered");
        }
        log::info!("control: thread {id} granted {:?} read_project={read_project} approvals={approvals}", grant.scope);
        cx.notify();
    }

    /// Ends what thread `id`'s agent may do: back to Viewer, its waiters gone, and its children left running as ordinary threads.
    pub(crate) fn stop_agent_control(&mut self, id: u64, cx: &mut Context<Self>) {
        self.set_grant(id, command::Scope::Viewer, false, false, cx);
        // Its children go on as ordinary threads, and no longer start turns of its.
        let quiet = |thread: &mut Thread| {
            if thread.parent == Some(id) {
                thread.notify = Notify::Never;
            }
        };
        quiet(&mut self.thread);
        self.background_threads().for_each(quiet);
    }
}
