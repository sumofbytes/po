//! Methods of [`Reader`], grouped by what they do.

use crate::*;

/// An agent's view as the pane shows it, with no chrome.
pub(crate) fn agent_view_snapshot(view: &command::AgentView) -> model::agent::Snapshot {
    model::agent::Snapshot { title: view.title.clone(), body: model::agent::Body::Markdown(model::source::Shared(Arc::from(view.body.as_str()))), ..Default::default() }
}

/// A form the agent put on screen as the pane shows it, with no chrome.
pub(crate) fn form_view_snapshot(form: &command::AgentForm) -> model::agent::Snapshot {
    let fields = form
        .fields
        .iter()
        .map(|field| model::agent::Field { name: field.name.clone(), label: field.label.clone(), options: field.options.clone(), value: field.value.clone() })
        .collect();
    model::agent::Snapshot {
        title: form.title.clone(),
        body: model::agent::Body::Form {
            id: form.id.clone(),
            fields,
            submit: form.submit.clone(),
            answered: form.answered.is_some(),
            doc: form.body.as_deref().map(|body| model::source::Shared(Arc::from(body))),
        },
        ..Default::default()
    }
}

/// The threads a project always lists, most recent first, before the rest fold under "more".
const MIN_THREADS: usize = 3;

impl Reader {
    /// The left side: every project with its threads, drawn by NSH from what [`Self::sidebar_snapshot`]
    /// publishes.
        /// What the sidebar shows: the projects and threads that answer its filter, what waits on the
    /// user, and what agents have suggested.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn sidebar_snapshot(&self) -> model::sidebar::Snapshot {
        use model::sidebar as side;
        let query = self.side_query.trim().to_lowercase();
        let searching = !query.is_empty();
        let views = self.views();
        let many = views.len() > 1;
        let (active_project, active_thread) = (self.project_id, self.thread.id);
        let now = now_ms();
        let thread_row = |pid: u64, thread: &Thread| side::Thread {
            project: pid,
            id: thread.id,
            label: self.thread_label(thread),
            name: capitalized(thread.title()),
            mark: match thread.activity() {
                Activity::Idle => side::Mark::Idle,
                Activity::Unread => side::Mark::Unread,
                Activity::Running => side::Mark::Running,
                Activity::Asking => side::Mark::Asking,
                Activity::Failed => side::Mark::Failed,
            },
            selected: (pid, thread.id) == (active_project, active_thread),
            controlled: self.grants.get(&thread.id).is_some_and(|grant| *grant != command::Grant::initial() && *grant != command::Grant::default()),
            ago: ago(now, thread.active_at),
            closing: self.closing == Some((pid, thread.id)) && self.close_loses_work(thread.id),
        };
        // Filtering keeps a project whose name matches, with all its threads, and any project with a
        // thread that does. A thread waiting on the user is in Needs you instead.
        let project_hit = |view: &View<'_>| view.project.name().to_lowercase().contains(&query);
        // Questions come first, the rest in the order they were made, so rows don't reshuffle as turns end.
        let mut waiting: Vec<(&View<'_>, &Thread)> = views
            .iter()
            .flat_map(|view| view.threads.iter().map(move |thread| (view, *thread)))
            .filter(|(view, thread)| self.waits_on_user(view.id, thread))
            .filter(|(view, thread)| !searching || project_hit(view) || self.matches_query(thread, &query))
            .collect();
        waiting.sort_by_key(|(_, thread)| (thread.activity() != Activity::Asking, thread.id));
        let waiting = waiting.into_iter().map(|(view, thread)| side::Waiting { thread: thread_row(view.id, thread), project: view.project.name() }).collect();

        let projects = views
            .iter()
            .filter(|view| !searching || project_hit(view) || view.threads.iter().any(|t| !self.waits_on_user(view.id, t) && self.matches_query(t, &query)))
            .map(|view| {
                let pid = view.id;
                let collapsed = self.projects_collapsed.contains(&pid) && !searching;
                // What keeps its row: the one on screen, running or asking, or something it did
                // within the last day. The rest is searched for, or folds under "more".
                let active = |t: &Thread| listed(now, view.shown, t);
                // Most recently used by the user first, not by the agent, so rows hold still while threads run. Only the active ones show; the rest fold under "more".
                let mut recent: Vec<&Thread> = view.threads.iter().copied().filter(|t| (!t.blank || t.id == view.shown) && !self.waits_on_user(pid, t)).collect();
                recent.sort_by_key(|t| (std::cmp::Reverse(t.used_at), std::cmp::Reverse(t.id)));
                if searching && !project_hit(view) {
                    recent.retain(|t| self.matches_query(t, &query));
                }
                let expanded = self.threads_expanded.contains(&pid) || searching;
                // At least the few most recent threads stay, and so does whatever is active.
                let kept = |i: usize, t: &Thread| i < MIN_THREADS || active(t);
                let shown_count = recent.iter().enumerate().filter(|(i, t)| kept(*i, t)).count();
                let foldable = !searching && recent.len() > shown_count;
                let hidden = if expanded || !foldable {
                    0
                } else {
                    let hidden = recent.len() - shown_count;
                    recent = recent.iter().copied().enumerate().filter(|(i, t)| kept(*i, t)).map(|(_, t)| t).collect();
                    hidden
                };
                if collapsed {
                    recent.clear();
                }
                side::Project {
                    id: pid,
                    name: view.project.name(),
                    active: pid == active_project,
                    collapsed,
                    closable: many,
                    threads: recent
                        .into_iter()
                        .map(|t| thread_row(pid, t))
                        .collect(),
                    fold: (foldable && !collapsed && (hidden > 0 || expanded)).then_some((expanded, hidden)),
                }
            })
            .collect();

        let suggestions = self
            .suggestions
            .iter()
            .map(|suggestion| {
                let (text, yes) = match &suggestion.what {
                    Proposal::Go(project) => {
                        let name = views.iter().find(|view| view.id == *project).map_or_else(|| "a project".to_string(), |view| view.project.root.file_name().map_or_else(|| "a project".into(), |name| name.to_string_lossy().into_owned()));
                        (format!("“{}” suggests {name}", suggestion.by), "Go")
                    }
                    Proposal::Open(root) => (format!("“{}” wants to open {}", suggestion.by, root.display()), "Open"),
                    Proposal::Pr { spec, .. } => (format!("“{}” wants to check out PR {spec}", suggestion.by), "Check out"),
                    Proposal::Merge(project) => {
                        let what = views.iter().find(|view| view.id == *project).and_then(|view| view.project.worktree.as_ref()).map_or_else(|| "a worktree".to_string(), |wt| format!("{} into {}", wt.spec.branch, wt.spec.repo.display()));
                        (format!("“{}” wants to merge {what}", suggestion.by), "Merge")
                    }
                };
                side::Suggestion { id: suggestion.id, text, yes }
            })
            .collect();

        side::Snapshot {
            glass: false,
            waiting,
            projects,
            suggestions,
            status: self.worktree_status.clone(),
            searching,
            issues_open: self.issues_open,
            home_open: self.home_open,
            // What the store worked out, with no disk read: a render only reads the issues.
            issues: issues::shared::overview().borrow().counts(date::today()),
        }
    }

    /// The conversation, drawn by NSH from what [`Self::chat_snapshot`] publishes; published here in place
    /// of the pane, as the viewer's panes are.
        /// What the conversation shows: the header, the transcript, and the composer.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn chat_snapshot(&self) -> model::chat::Snapshot {
        use model::chat as chat;
        let thread = &self.thread;
        // Bump the transcript's follow (the pane scrolls to its end) when the thread the pane
        // shows got louder: a message came or went, or the turn started or stopped.
        let busy = thread.busy;
        let blocks = match thread.messages.last() {
            Some(Msg::Agent(blocks)) => blocks.len(),
            _ => 0,
        };
        // A thread other than the one last shown starts from its own draft and its own end.
        let shown = (self.project_id, thread.id);
        if self.chat_shown.replace(shown) != shown {
            self.chat_reset.set(self.chat_reset.get() + 1);
        }
        let growth = match thread.messages.last() {
            Some(Msg::Agent(blocks)) => match blocks.last() {
                Some(Block::Text(text)) | Some(Block::Thought(text, _)) => text.len(),
                _ => 0,
            },
            _ => 0,
        };
        let louder = (shown, thread.messages.len(), blocks, busy, growth);
        let before = self.chat_marks.get();
        let (follow, follow_soft) = if louder == before {
            (self.chat_follow.get(), self.chat_follow_soft.get())
        } else if (louder.0, louder.1) != (before.0, before.1) {
            // Another thread, or a message came or went: the end is where to be.
            self.chat_follow.set(self.chat_follow.get() + 1);
            (self.chat_follow.get(), self.chat_follow_soft.get())
        } else {
            // Only the turn going on: whoever scrolled up to read stays there.
            self.chat_follow_soft.set(self.chat_follow_soft.get() + 1);
            (self.chat_follow.get(), self.chat_follow_soft.get())
        };
        self.chat_marks.set(louder);
        let mut statuses = Vec::new();
        if let Some(line) = self.children_status(thread.id) {
            statuses.push(line);
        }
        let picking = thread.picking.then(|| match &thread.sessions {
            Some(sessions) if !thread.loading && !sessions.is_empty() => {
                // What was closed here lately, in the order it was closed, if this agent still lists it.
                let closed: Vec<&SessionInfo> = self
                    .project
                    .closed
                    .iter()
                    .filter(|closed| thread.agent.as_deref() == Some(closed.agent.as_str()))
                    .filter_map(|closed| sessions.iter().find(|session| session.id == closed.session))
                    .collect();
                let session = |info: &SessionInfo, closed: bool| chat::Session {
                    id: info.id.clone(),
                    title: info.title.clone(),
                    when: info.updated.get(..16).unwrap_or_default().replace('T', " "),
                    closed,
                };
                // One that was picked again since is open, not closed.
                let closed: Vec<&SessionInfo> = closed.into_iter().filter(|info| !self.threads_all().any(|t| t.session.as_deref() == Some(info.id.as_str()))).collect();
                let rest = sessions.iter().filter(|info| !closed.iter().any(|c| c.id == info.id));
                chat::Picking::Sessions(closed.iter().map(|info| session(info, true)).chain(rest.map(|info| session(info, false))).collect())
            }
            Some(_) if !thread.loading => chat::Picking::Note(format!("No sessions to resume in {}", self.project.root.display())),
            _ => chat::Picking::Loading,
        });
        let grant = self.grants.get(&thread.id).copied().unwrap_or_else(command::Grant::initial);
        chat::Snapshot {
            glass: false,
            title: capitalized(thread.title()),
            thread: (self.project_id, thread.id),
            lead: vec![],
            trail: vec![],
            statuses,
            place: self.place(),
            blank: thread.blank,
            busy,
            steer: thread.client.as_ref().is_some_and(|client| client.queues()),
            agents: self.agents.iter().map(|agent| (agent.name.clone(), agent.id.clone())).collect(),
            chosen: thread.agent.clone().or_else(|| self.chosen_agent.clone()),
            switchable: thread.can_switch_agent(),
            settings: Thread::ordered(thread.options.iter().collect()).into_iter().map(|option| chat::Setting {
                id: option.id.clone(),
                name: option.name.clone(),
                current: option.current.clone(),
                choices: option.choices.clone(),
            }).collect(),
            grant_pending: thread.started_grant.is_some_and(|started| grant.scope > started.scope || (grant.read_project && !started.read_project) || (grant.approvals && !started.approvals)),
            isolate: worktree::in_repo(&self.project.root).then_some(thread.isolate),
            grant: Some(grant).map(|g| chat::Grant {
                scope: match g.scope {
                    command::Scope::Viewer => chat::Scope::Viewer,
                    command::Scope::Orchestrate => chat::Scope::Orchestrate,
                    command::Scope::Global => chat::Scope::Global,
                },
                read: g.read_project,
                approvals: g.approvals,
            }),
            queue: thread.queue.iter().enumerate().map(|(at, queued)| chat::Queued {
                at,
                kind: match queued.kind {
                    Some(Queue::Steer) => "Steer".into(),
                    Some(Queue::Followup) => "Follow-up".into(),
                    None => String::new(),
                },
                text: queued.out.text.clone(),
                pictures: queued.out.images.len(),
            }).collect(),
            pictures: thread.attachments.iter().map(|a| chat::Picture { uri: a.data_uri(), size: a.size }).collect(),
            note: thread.attach_note.clone(),
            // A resumed session's history is published once, when it is done (`Event::Ready`):
            // replayed as hundreds of updates, it would otherwise be rebuilt from the start for
            // every one of them, each frame walking the whole transcript it has so far.
            items: if thread.loading { Vec::new() } else { chat_items(&thread.messages, thread.busy) },
            picking,
            draft: thread.draft.clone(),
            draft_reset: self.chat_reset.get(),
            focus: self.chat_focus.get(),
            follow,
            follow_soft,
        }
    }

    /// Does what the user asked of on the conversation the NSH pane draws.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn chat_cmd(&mut self, cmd: model::chat::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use model::chat::{Cmd as Chat, GrantChange, Scope as ChatScope};
        match cmd {
            Chat::Draft(text) => self.thread.draft = text,
            Chat::Send { later } => self.send(later, window, cx),
            Chat::Stop => {
                self.stop(cx);
                // Pressing stop is a move in the composer, so the keyboard goes back to its field
                // (a cancel from elsewhere, through `stop`, leaves it where it is).
                self.focus_draft(window, cx);
            }
            Chat::Drop(uris) => self.attach(uris.iter().filter_map(|uri| file_of(uri)).map(|path| Source::Path(PathBuf::from(path))).collect(), cx),
            Chat::Paste(pictures) => self.attach(pictures.into_iter().map(Source::Bytes).collect(), cx),
            Chat::Detach(at) => {
                if at < self.thread.attachments.len() {
                    self.thread.attachments.remove(at);
                }
                cx.notify();
            }
            Chat::Recall(at) => self.recall(at, cx),
            Chat::Answer { message, block, at } => self.answer(message, block, at, cx),
            Chat::Approve { message, block, yes } => self.answer_approval(message, block, yes, window, cx),
            Chat::ToggleTool { message, block } => self.toggle_tool(message, block, cx),
            Chat::ToggleThought { message, block } => self.toggle_thought(message, block, cx),
            Chat::Fold { message, block, folded } => self.fold_run(message, block, folded, cx),
            Chat::Open(path) => self.reveal(&path, window, cx),
            Chat::Resume(id) => self.resume_session(&id, window, cx),
            Chat::PickAgent(id) => self.pick_agent(id, window, cx),
            Chat::Setting { id, value } => self.choose_option(&id, &value, cx),
            Chat::ClosePicker => {
                self.thread.drop_agent();
                cx.notify();
            }
            Chat::DismissNote => {
                self.thread.attach_note = None;
                cx.notify();
            }
            Chat::Isolate(isolate) => {
                self.thread.isolate = isolate;
                cx.notify();
            }
            Chat::Grant(change) => {
                let id = self.thread.id;
                let grant = self.grants.get(&id).copied().unwrap_or_else(command::Grant::initial);
                match change {
                    GrantChange::Set(set) => {
                        let scope = match set.scope {
                            ChatScope::Viewer => command::Scope::Viewer,
                            ChatScope::Orchestrate => command::Scope::Orchestrate,
                            ChatScope::Global => command::Scope::Global,
                        };
                        self.set_grant(id, scope, set.read, set.approvals, cx);
                    }
                    GrantChange::Read => self.set_grant(id, grant.scope, !grant.read_project, grant.approvals, cx),
                    GrantChange::Approvals => self.set_grant(id, grant.scope, grant.read_project, !grant.approvals, cx),
                    GrantChange::Stop => self.stop_agent_control(id, cx),
                }
                // A blank thread's agent is already running, and its tool list was fixed when it
                // started: it starts again so that the first message goes out under this grant.
                if self.thread.blank
                    && self.thread.client.is_some()
                    && self.grants.get(&id).copied().unwrap_or_else(command::Grant::initial) != grant
                    && let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone()))
                {
                    log::info!("agent: blank thread restarts {key} under its new grant");
                    self.connect(&key, false, window, cx);
                }
            }
        }
    }

    /// Does what the user asked of a screen drawn by NSH.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn run(&mut self, cmd: model::bus::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use model::bus::Cmd;
        if !matches!(cmd, Cmd::CloseThread(..)) && self.closing.take().is_some() {
            cx.notify();
        }
        // The settings page is drawn over whatever is open, so going anywhere else puts it away. Home and
        // Issues may already be open beneath it: pressing theirs then only uncovers them.
        if self.settings_open {
            let uncovers = (matches!(cmd, Cmd::ToggleHome) && self.home_open) || (matches!(cmd, Cmd::ToggleIssues) && self.issues_open);
            let leaves = matches!(
                cmd,
                Cmd::ToggleProject(_) | Cmd::StartThread(_) | Cmd::OpenThread(..) | Cmd::AddProject | Cmd::NewThread | Cmd::ToggleHome | Cmd::ToggleIssues | Cmd::OpenIssue(..) | Cmd::WorkOnIssue(..) | Cmd::OpenPr(_) | Cmd::FinderPick(_)
            );
            if leaves {
                self.settings_open = false;
                cx.notify();
            }
            if uncovers {
                return;
            }
        }
        match cmd {
            Cmd::SideQuery(query) => {
                self.side_query = query;
                cx.notify();
            }
            Cmd::ToggleProject(pid) => {
                let expanding = !self.projects_collapsed.insert(pid);
                if expanding {
                    self.projects_collapsed.remove(&pid);
                }
                // Folding is only a redraw; the project is switched to only when it opens another one.
                if expanding && pid != self.project_id {
                    self.show_project(pid, window, cx);
                } else {
                    self.request_save(cx);
                    cx.notify();
                }
            }
            Cmd::StartThread(pid) => self.start_thread_in(pid, window, cx),
            Cmd::ToggleMore(pid) => {
                if !self.threads_expanded.insert(pid) {
                    self.threads_expanded.remove(&pid);
                }
                cx.notify();
            }
            Cmd::OpenThread(pid, tid) => self.open_thread(pid, tid, window, cx),
            Cmd::CloseThread(pid, tid) => self.close_asked(pid, tid, window, cx),
            Cmd::AddProject => self.pick_project(window, cx),
            Cmd::NewThread => self.new_blank_thread(window, cx),
            Cmd::OpenSettings => self.toggle_settings(window, cx),
            Cmd::ToggleIssues => self.toggle_issues(window, cx),
            Cmd::ToggleHome => self.toggle_home(window, cx),
            Cmd::OpenIssue(repo, number) => self.open_issue_of(&repo, number, window, cx),
            Cmd::CloseIssues => self.hide_issues(window, cx),
            Cmd::WorkOnIssue(repo, number) => self.work_on_issue(&repo, number, window, cx),
            Cmd::DismissStatus => {
                self.worktree_status = None;
                cx.notify();
            }
            Cmd::ToggleLeft => self.toggle_left(cx),
            Cmd::ToggleRight => self.toggle_right(cx),
            Cmd::ToggleFocus => self.toggle_focus(cx),
            Cmd::Show(changes) => self.show_viewer(if changes { Mode::Diff } else { Mode::Source }, cx),
            Cmd::Terminal => self.toggle_terminal_on(window, cx),
            Cmd::ToggleRendered => self.toggle_rendered(cx),
            Cmd::SetSplit(split) => self.set_split(split, cx),
            Cmd::ShowView(id) => self.show_agent_view(&id, cx),
            Cmd::CloseView(id) => self.close_agent_view(&id, cx),
            Cmd::TreeClick(id) => {
                let tree = if self.mode == Mode::Diff { &mut self.project.changes_tree } else { &mut self.project.files_tree };
                tree.click(&id);
                self.open_selected(true, window, cx);
                cx.notify();
            }
            Cmd::TreeStep(step) => {
                let tree = if self.mode == Mode::Diff { &mut self.project.changes_tree } else { &mut self.project.files_tree };
                tree.step(step);
                // Moving the selection with the keyboard opens the file too, like a click.
                self.open_selected(true, window, cx);
                cx.notify();
            }
            Cmd::FinderQuery(query) => {
                if query != self.finder_query {
                    self.finder_query = query;
                    self.refind();
                }
            }
            Cmd::FinderPick(path) => self.accept_path(&path, window, cx),
            Cmd::CloseFinder => self.hide_finder(window, cx),
            Cmd::CloseSettings => self.hide_settings(window, cx),
            Cmd::SettingsSaved => self.apply_settings(cx),
            Cmd::Answer(id, yes) => self.answer_suggestion(id, yes, window, cx),
            Cmd::SubmitForm(id, values) => self.form_submitted(&id, values, cx),
            Cmd::OpenPr(spec) => self.open_pr(&spec, window, cx),
            Cmd::ClosePr => self.close_pr(window, cx),
            Cmd::MergeWorktree => self.finish_worktree(true, window, cx),
            Cmd::DiscardWorktree => self.finish_worktree(false, window, cx),
            Cmd::Chat(chat) => self.chat_cmd(chat, window, cx),
        }
    }

    /// Whether thread `thread` of project `pid` waits on the user, which takes it out of its
    /// project and into NEEDS YOU, so a folded project can't hide it. The one on screen is
    /// already in view, so it stays where it is.
    pub(crate) fn waits_on_user(&self, pid: u64, thread: &Thread) -> bool {
        let parent_hears = thread.parent.is_some() && thread.notify != Notify::Never;
        (pid, thread.id) != (self.project_id, self.thread.id) && thread.activity().needs_you(parent_hears)
    }

    /// The viewer or the diff viewer, as the project's mode says, with the file tree beside it
    /// unless that is hidden or the agent's own view is shown, which has no files to pick.
    #[hotpath::measure(impl_type = "Reader")]
        /// A view the agent made, as a markdown document; the pane NSH draws from what is published.
        pub(crate) fn close_agent_view(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.thread.forms.remove(id) {
            let owner = self.thread.id;
            self.drop_form(owner, id);
        } else {
            self.thread.views.apply(command::Command::ViewClose { id: id.to_string() }).ok();
        }
        self.sync_view_docs(cx);
        cx.notify();
    }

    /// The user answered form `id`: keep the answers for waits that start later, and wake
    /// whoever waits on it.
    pub(crate) fn form_submitted(&mut self, id: &str, values: Value, cx: &mut Context<Self>) {
        let reply = serde_json::json!({ "form": id, "values": values });
        let owner = self.thread.id;
        self.thread.forms.answered(id, reply.clone());
        self.wake_form(owner, id, reply);
        cx.notify();
    }

    /// The picked file's changes, drawn by NSH from what [`Self::diff_snapshot`] publishes.
        #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn diff_snapshot(&self) -> model::diff::Snapshot {
        use model::diff::{Body, Snapshot};
        let project = &self.project;
        let (title, body) = match (&project.picked, &project.versions) {
            (None, _) => ("Changes".to_string(), Body::Hint(self.pick_hint("a changed file"))),
            (Some(path), _) if project.rows.is_empty() && project.diffing.is_some() => (path.clone(), Body::Loading),
            (Some(path), None) => (path.clone(), Body::NoChanges),
            (Some(path), Some((old, new))) => (path.clone(), Body::Versions { path: path.clone(), old: old.clone(), new: new.clone() }),
        };
        Snapshot { glass: false, title, lead: vec![], trail: vec![], split: self.split, counts: project.counts, body }
    }

    /// The open file in the viewer, drawn by NSH from what [`Self::source_snapshot`] publishes.
        #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn source_snapshot(&self) -> model::source::Snapshot {
        use model::source::{Body, Snapshot};
        let current = self.project.current.as_deref();
        let markdown = self.project.is_markdown;
        let rendered = markdown && self.rendered;
        let body = match (&self.project.code, &self.project.notice) {
            (_, Some(notice)) => Body::Notice(notice.clone()),
            (Some(code), None) if rendered => Body::Markdown(code.clone()),
            (Some(code), None) => Body::Code { path: current.unwrap_or_default().to_string(), text: code.clone() },
            (None, None) => Body::Hint(self.pick_hint("a file")),
        };
        Snapshot {
            glass: false,
            title: current.map_or_else(|| "No file open".to_string(), str::to_string),
            lead: vec![],
            trail: vec![],
            language: current.map(language).unwrap_or("").to_string(),
            preview: markdown.then_some(self.rendered),
            body,
            reveal: self.reveal,
        }
    }

    pub(crate) fn toggle_rendered(&mut self, cx: &mut Context<Self>) {
        self.rendered = !self.rendered;
        cx.notify();
    }

    /// What a thread is called in the lists: a child shows under the thread that started it.
    pub(crate) fn thread_label(&self, thread: &Thread) -> String {
        match thread.parent.and_then(|parent| self.title_of(parent)).filter(|_| thread.renamed.is_none()) {
            Some(parent) => format!("↳ {parent}"),
            None => capitalized(thread.title()),
        }
    }

    /// What the parent of children says in its header: `3 threads · 1 asking · 1 failed`.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn children_status(&self, id: u64) -> Option<String> {
        let children: Vec<&Thread> = self.threads_all().filter(|thread| thread.parent == Some(id)).collect();
        if children.is_empty() {
            return None;
        }
        let in_state = |state: crate::state::ThreadState| children.iter().filter(|t| crate::state::state(&t.facts()) == state).count();
        let mut line = format!("{} thread{}", children.len(), if children.len() == 1 { "" } else { "s" });
        let asking = in_state(crate::state::ThreadState::Asking);
        if asking > 0 {
            line.push_str(&format!(" · {asking} asking"));
        }
        let failed = in_state(crate::state::ThreadState::Failed);
        if failed > 0 {
            line.push_str(&format!(" · {failed} failed"));
        }
        Some(line)
    }
}

/// What the window shows and where: the Rune host lays the panes out from it.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Layout {
    pub left_open: bool,
    pub side_open: bool,
    pub swapped: bool,
    pub agent_open: bool,
    pub home_open: bool,
    pub issues_open: bool,
    pub finder_open: bool,
    pub settings_open: bool,
    /// The project's terminal stands in the viewer.
    pub terminal_shown: bool,
    /// The agent's own view or form is in the viewer, which has no files to pick.
    pub showing_agent: bool,
    pub diff: bool,
}

impl Reader {
    pub(crate) fn layout(&self) -> Layout {
        let showing_agent = self.project.terminal_shown
            || self.thread.shown.as_deref().is_some_and(|id| self.thread.forms.get(id).is_some() || self.thread.views.list().iter().any(|view| view.id == id));
        Layout {
            left_open: self.left_open,
            side_open: self.side_open,
            swapped: self.swapped,
            agent_open: self.agent_open,
            home_open: self.home_open,
            issues_open: self.issues_open,
            finder_open: self.finder_open,
            settings_open: self.settings_open,
            terminal_shown: self.project.terminal_shown,
            showing_agent,
            diff: self.mode == Mode::Diff,
        }
    }

    /// Publishes what each screen draws, with the window's controls the panes at its edges carry.
    /// Each button is one pane and never changes what it means: the sidebar toggle at the left of
    /// the leftmost pane, the viewer toggle in the viewer's header (or, while it is closed, in the
    /// conversation's, on the side the viewer opens), and, with the viewer open, a button that
    /// gives it the room by hiding the conversation.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn publish_screens(&mut self) {
        use model::header::{Chrome, Control};
        self.settle_agent_changes();
        let (left_open, side_open, swapped, agent_open) = (self.left_open, self.side_open, self.swapped, self.agent_open);
        let focus = side_open && !agent_open;
        let viewer = |on: bool| if swapped { Control::ViewerLeft(on) } else { Control::Viewer(on) };
        let trail_all = vec![Control::Focus(focus), viewer(side_open)];
        let trail_focus = {
            use model::chat::Place;
            let place = self.place();
            [vec![Control::Files(place == Place::Files), Control::Changes(place == Place::Changes), Control::Terminal(place == Place::Terminal)], trail_all.clone()].concat()
        };
        let none = || (vec![], vec![]);
        // (the conversation's controls, the viewer's)
        let (chat, pane) = if self.home_open || self.issues_open {
            (none(), none())
        } else if !side_open {
            if swapped { ((vec![Control::Sidebar(left_open), viewer(side_open)], vec![]), none()) } else { ((vec![Control::Sidebar(left_open)], vec![Control::Files(false), Control::Changes(false), Control::Terminal(false), viewer(side_open)]), none()) }
        } else if focus {
            (none(), (vec![Control::Sidebar(left_open)], trail_focus))
        } else if swapped {
            (none(), (vec![Control::Sidebar(left_open)], trail_all))
        } else {
            ((vec![Control::Sidebar(left_open)], vec![]), (vec![], trail_all))
        };
        model::sidebar::publish(self.sidebar_snapshot());
        model::chat::publish(self.chat_snapshot().with_chrome(Chrome { glass: false, lead: chat.0, trail: chat.1 }));
        if self.home_open || self.issues_open || !side_open {
            return;
        }
        let chrome = |(lead, trail): (Vec<Control>, Vec<Control>)| Chrome { glass: false, lead, trail };
        let has_tabs = self.publish_tabs(pane.clone());
        // The window's own controls go on the tabs while there are any, and on the pane otherwise.
        let controls = if has_tabs { none() } else { pane.clone() };
        // With the tree beside the file the tree's header takes the lead controls and the file's the trail.
        let with_tree = !self.layout().showing_agent;
        let (tree_controls, controls) = if with_tree { ((controls.0.clone(), vec![]), (vec![], controls.1.clone())) } else { (none(), controls) };
        self.publish_explorer(tree_controls);
        let shown_form = self.thread.shown.as_deref().and_then(|id| self.thread.forms.get(id));
        let shown = self.thread.shown.as_deref().and_then(|id| self.thread.views.list().iter().find(|view| view.id == id));
        match (shown, shown_form, self.mode) {
            _ if self.project.terminal_shown => model::agent::publish(model::agent::Snapshot { title: "Terminal".into(), body: model::agent::Body::Bare, ..Default::default() }.with_chrome(chrome(controls))),
            (Some(view), _, _) => model::agent::publish(agent_view_snapshot(view).with_chrome(chrome(controls))),
            (_, Some(form), _) => model::agent::publish(form_view_snapshot(form).with_chrome(chrome(controls))),
            (None, None, Mode::Source) => model::source::publish(self.source_snapshot().with_chrome(chrome(controls))),
            (None, None, Mode::Diff) => model::diff::publish(self.diff_snapshot().with_chrome(chrome(controls))),
        }
    }

    /// The agent's views as tabs, with a prompt for one it added while the user was busy, and the
    /// window's controls at the ends. False while there are none.
    pub(crate) fn publish_tabs(&self, controls: (Vec<model::header::Control>, Vec<model::header::Control>)) -> bool {
        if self.thread.views.is_empty() && self.thread.forms.is_empty() {
            return false;
        }
        use model::tabs::{Snapshot, Tab};
        let titled = |id: &String| {
            self.thread
                .views
                .list()
                .iter()
                .find(|view| view.id == *id)
                .map(|view| Tab { id: view.id.clone(), title: view.title.clone() })
                .or_else(|| self.thread.forms.list().iter().find(|form| form.id == *id).map(|form| Tab { id: form.id.clone(), title: form.title.clone() }))
        };
        model::tabs::publish(
            Snapshot {
                shown: if self.project.terminal_shown { None } else { self.thread.shown.clone() },
                tabs: self.thread.views.list().iter().map(|view| Tab { id: view.id.clone(), title: view.title.clone() }).chain(self.thread.forms.list().iter().map(|form| Tab { id: form.id.clone(), title: form.title.clone() })).collect(),
                offer: self.thread.offered.as_ref().and_then(titled),
                controls: Default::default(),
            }
            .with_controls(controls),
        );
        true
    }
}
