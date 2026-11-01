//! Methods of [`Reader`], grouped by what they do.

use crate::*;

impl Reader {
    /// The left side: every project with its threads, drawn by NSH from what [`Self::sidebar_snapshot`]
    /// publishes.
    pub(crate) fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        nsh::sidebar::publish(self.sidebar_snapshot(cx));
        floating(div().size_full().flex().flex_col().child(self.sidebar.clone()), cx)
    }

    /// What the sidebar shows: the projects and threads that answer its filter, what waits on the
    /// user, and what agents have suggested.
    pub(crate) fn sidebar_snapshot(&self, cx: &App) -> nsh::sidebar::Snapshot {
        use nsh::sidebar as side;
        let query = self.side_query.trim().to_lowercase();
        let searching = !query.is_empty();
        let views = self.views();
        let many = views.len() > 1;
        let (active_project, active_thread) = (self.project_id, self.thread.id);
        let thread_row = |pid: u64, thread: &Thread| side::Thread {
            project: pid,
            id: thread.id,
            label: self.thread_label(thread),
            mark: match thread.activity() {
                Activity::Idle => side::Mark::Idle,
                Activity::Unread => side::Mark::Unread,
                Activity::Running => side::Mark::Running,
                Activity::Asking => side::Mark::Asking,
            },
            selected: (pid, thread.id) == (active_project, active_thread),
            controlled: self.grants.get(&thread.id).is_some_and(|grant| *grant != command::Grant::initial() && *grant != command::Grant::default()),
            day: None,
            ago: ago(now_ms(), thread.active_at),
            closing: self.closing == Some((pid, thread.id)) && self.close_loses_work(thread.id),
        };
        let today = date::today();
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
                // Most recently active first. Only the active ones show; the rest fold under "more".
                let mut recent: Vec<&Thread> = view.threads.iter().copied().filter(|t| (!t.blank || t.id == view.shown) && !self.waits_on_user(pid, t)).collect();
                recent.sort_by_key(|t| (std::cmp::Reverse(t.active_at), std::cmp::Reverse(t.id)));
                if searching && !project_hit(view) {
                    recent.retain(|t| self.matches_query(t, &query));
                }
                let expanded = self.threads_expanded.contains(&pid) || searching;
                // Active: running, finished unseen with its parent told, or an agent still alive; the one
                // on screen too, whatever it is doing.
                let active = |t: &Thread| t.id == view.shown || t.activity() != Activity::Idle || t.client.is_some();
                let shown_count = recent.iter().filter(|t| active(t)).count();
                let foldable = !searching && recent.len() > shown_count.max(1);
                let hidden = if expanded || !foldable {
                    0
                } else {
                    let keep: Vec<&Thread> = recent.iter().copied().filter(|t| active(t)).collect();
                    let hidden = recent.len() - keep.len().max(1);
                    recent = if keep.is_empty() { recent[..1].to_vec() } else { keep };
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
                    threads: {
                        let mut last = None;
                        recent
                            .into_iter()
                            .map(|t| {
                                let day = match today.days_since(date::Date::of_ms(t.active_at)) {
                                    ..=0 => "",
                                    1 => "Yesterday",
                                    2..=6 => "Previous 7 days",
                                    _ => "Older",
                                };
                                let mut row = thread_row(pid, t);
                                row.day = (last != Some(day) && !day.is_empty()).then_some(day);
                                last = Some(day);
                                row
                            })
                            .collect()
                    },
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
            glass: cx.global::<Palette>().glass.is_some(),
            waiting,
            projects,
            suggestions,
            status: self.worktree_status.clone(),
            searching,
            issues_open: self.issues_open,
            issues: {
                self.follow_issues();
                issues::shared::summary()
            },
        }
    }

    /// The conversation, drawn by NSH from what [`Self::chat_snapshot`] publishes; published here in place
    /// of the pane, as the viewer's panes are.
    pub(crate) fn chat_pane(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> gpui::Div {
        nsh::chat::publish(self.chat_snapshot(lead, trail, cx));
        let tint = cx.global::<Palette>().primary.opacity(theme::tokens::DROP_TINT);
        // Files dropped anywhere on the conversation join the draft. NSH's views can sit over this
        // element and keep it from being the hovered one, so while a file is being dragged a layer
        // above them takes the drop; it is only there for the drag, so clicks are never blocked.
        let dragging = cx.has_active_drag();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.chat.clone())
            .children(dragging.then(|| {
                div()
                    .absolute()
                    .size_full()
                    .occlude()
                    .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(tint))
                    .on_drop(cx.listener(|reader, dropped: &gpui::ExternalPaths, _, cx| {
                        reader.attach(dropped.paths().iter().cloned().map(Source::Path).collect(), cx);
                    }))
            }))
    }

    /// What the conversation shows: the header, the transcript, and the composer.
    pub(crate) fn chat_snapshot(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &App) -> nsh::chat::Snapshot {
        use nsh::chat as chat;
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
        let state = (!thread.status.is_empty()).then(|| {
            use crate::state::ThreadState as S;
            let tone = match crate::state::state(&thread.facts()) {
                S::Ready | S::Asleep | S::Closed => chat::Tone::Idle,
                S::Starting | S::Running => chat::Tone::Working,
                S::Approval | S::Asking => chat::Tone::Attention,
                S::Failed => chat::Tone::Failed,
            };
            // "ready" is set when the agent starts and nothing resets it, so a turn going on says so here.
            let text = if tone == chat::Tone::Working && thread.status == "ready" { "working" } else { thread.status.as_str() };
            (tone, text.to_string())
        });
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
            glass: cx.global::<Palette>().glass.is_some(),
            title: capitalized(thread.title()),
            lead,
            trail,
            statuses,
            state,
            blank: thread.blank,
            busy,
            steer: thread.client.as_ref().is_some_and(|client| client.queues()),
            agents: self.agents.iter().map(|agent| (agent.name.clone(), agent.id.clone())).collect(),
            chosen: thread.agent.clone().or_else(|| self.chosen_agent.clone()),
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
            items: chat_items(&thread.messages),
            picking,
            draft: thread.draft.clone(),
            draft_reset: self.chat_reset.get(),
            focus: self.chat_focus.get(),
            follow,
            follow_soft,
        }
    }

    /// Does what the user asked of on the conversation the NSH pane draws.
    pub(crate) fn chat_cmd(&mut self, cmd: nsh::chat::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use nsh::chat::{Cmd as Chat, GrantChange, Scope as ChatScope};
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
            Chat::History => self.resume_thread(window, cx),
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
    pub(crate) fn run(&mut self, cmd: nsh::bus::Cmd, window: &mut Window, cx: &mut Context<Self>) {
        use nsh::bus::Cmd;
        if !matches!(cmd, Cmd::CloseThread(..)) && self.closing.take().is_some() {
            cx.notify();
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
            Cmd::CloseProject(pid) => self.close_project(pid, window, cx),
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
            Cmd::CloseIssues => self.hide_issues(window, cx),
            Cmd::WorkOnIssue(number) => self.work_on_issue(number, window, cx),
            Cmd::ToggleLeft => self.toggle_left(cx),
            Cmd::ToggleRight => self.toggle_right(cx),
            Cmd::ToggleFocus => self.toggle_focus(cx),
            Cmd::Browse(browse) => self.set_browsing(browse, cx),
            Cmd::ToggleRendered => self.toggle_rendered(cx),
            Cmd::SetSplit(split) => self.set_split(split, cx),
            Cmd::ShowHome => {
                self.thread.shown = None;
                self.project.terminal_shown = false;
                window.focus(&self.focus, cx);
                cx.notify();
            }
            Cmd::ShowView(id) if id == TERMINAL_TAB => self.toggle_terminal_on(window, cx),
            Cmd::CloseView(id) if id == TERMINAL_TAB => self.close_terminal(window, cx),
            Cmd::ShowView(id) => self.show_agent_view(&id, cx),
            Cmd::CloseView(id) => self.close_agent_view(&id, cx),
            Cmd::SetMode(diff) => self.set_mode(if diff { Mode::Diff } else { Mode::Source }, cx),
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
            Cmd::OpenUrl(url) => cx.open_url(&url),
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
    pub(crate) fn pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let tabs = self.view_tabs();
        let has_tabs = tabs.is_some();
        let shown_form = self.thread.shown.as_deref().and_then(|id| self.thread.forms.get(id));
        let shown = self
            .thread
            .shown
            .as_deref()
            .and_then(|id| self.thread.views.list().iter().find(|view| view.id == id));
        let showing_terminal = self.project.terminal_shown.then(|| self.project.terminal.clone()).flatten();
        let showing_agent = shown.is_some() || shown_form.is_some() || showing_terminal.is_some();
        let with_tree = !showing_agent && self.tree_visible();
        // The window's own control goes in the leftmost header: the tree's, or the viewer's.
        // A pane too narrow for the tree beside the file takes turns: the explorer fills it until a
        // file is picked, then the file does, with a button back to the explorer.
        let stacked = !showing_agent && self.viewer_narrow.get();
        let nothing_picked = if self.mode == Mode::Diff { self.project.picked.is_none() } else { self.project.current.is_none() };
        let explorer_only = stacked && (self.browse || nothing_picked);
        // A pane wide enough shows the explorer beside what it shows, with the lead controls in
        // the file's header; stacked, the file's alone gets them, with a way back to the explorer
        // tacked on.
        // The same split of the window's controls, as descriptors, for a body NSH draws.
        let (lead_controls, trail_controls) = self.pane_controls.borrow().clone();
        let tree_lead = if with_tree || explorer_only { lead_controls.clone() } else { vec![] };
        let body_controls = |lead: Vec<nsh::header::Control>| if with_tree || explorer_only { (vec![], trail_controls.clone()) } else if stacked { ([lead, vec![nsh::header::Control::Explorer]].concat(), trail_controls.clone()) } else { (lead, trail_controls.clone()) };
        let body = match (shown, shown_form, self.mode) {
            _ if showing_terminal.is_some() => {
                let background = cx.global::<Palette>().background;
                div().size_full().bg(background).children(showing_terminal).into_any_element()
            }
            _ if explorer_only => {
                let background = cx.global::<Palette>().background;
                div().size_full().when(!has_tabs, |d| d.bg(background)).child(self.explorer(tree_lead.clone(), trail_controls.clone(), cx)).into_any_element()
            }
            (Some(view), _, _) => self.agent_view(view, body_controls(lead_controls), cx).into_any_element(),
            (_, Some(form), _) => self.form_view(form, body_controls(lead_controls), cx).into_any_element(),
            (None, None, Mode::Source) => {
                let (lead, trail) = body_controls(lead_controls);
                self.viewer(lead, trail, cx).into_any_element()
            }
            (None, None, Mode::Diff) => {
                let (lead, trail) = body_controls(lead_controls);
                self.diff_viewer(lead, trail, cx).into_any_element()
            }
        };
        let background = cx.global::<Palette>().background;
        let body = if with_tree {
            // The viewers paint their own background; the column paints its own too, unless the
            // pane does for both, which would lay a second translucent layer under glass.
            let column = div().size_full().when(!has_tabs, |d| d.bg(background)).child(self.explorer(tree_lead, vec![], cx));
            sidebars(&self.tree_cols).id("tree").left(true, TREE_RANGE, column).center(div().size_full().child(body)).into_any_element()
        } else {
            body
        };
        // Measures the pane, which it is drawn at whatever the tree is doing, and draws again
        // when that changes whether the tree fits.
        let narrow = self.viewer_narrow.clone();
        let measure = gpui::canvas(
            move |bounds, window, _| {
                let now = !tree_fits(bounds.size.width.into());
                if narrow.replace(now) != now {
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        floating(div().size_full().flex().flex_col().relative().when(has_tabs, |d| d.bg(background)).child(measure).children(tabs).child(div().flex_1().min_h_0().child(body)), cx)
    }

    /// A view the agent made, as a markdown document; the pane NSH draws from what is published.
    pub(crate) fn agent_view(&self, view: &command::AgentView, (lead, trail): (Vec<nsh::header::Control>, Vec<nsh::header::Control>), cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        nsh::agent::publish(nsh::agent::Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            title: view.title.clone(),
            lead,
            trail,
            body: nsh::agent::Body::Markdown(nsh::source::Shared(Arc::from(view.body.as_str()))),
        });
        div().size_full().child(self.agent_pane.clone())
    }

    /// A form the agent put on screen: what it is for, then the fields the user answers, drawn
    /// and answered by NSH.
    pub(crate) fn form_view(&self, form: &command::AgentForm, (lead, trail): (Vec<nsh::header::Control>, Vec<nsh::header::Control>), cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let fields = form
            .fields
            .iter()
            .map(|field| nsh::agent::Field {
                name: field.name.clone(),
                label: field.label.clone(),
                options: field.options.clone(),
                value: field.value.clone(),
            })
            .collect();
        nsh::agent::publish(nsh::agent::Snapshot {
            glass: cx.global::<Palette>().glass.is_some(),
            title: form.title.clone(),
            lead,
            trail,
            body: nsh::agent::Body::Form {
                id: form.id.clone(),
                fields,
                submit: form.submit.clone(),
                answered: form.answered.is_some(),
                doc: form.body.as_deref().map(|body| nsh::source::Shared(Arc::from(body))),
            },
        });
        div().size_full().child(self.agent_pane.clone())
    }

    /// Files (or changes) and the agent's views as tabs, with a prompt for one it added while
    /// the user was busy. None while the agent has made no views.
    pub(crate) fn view_tabs(&self) -> Option<AnyElement> {
        if self.thread.views.is_empty() && self.thread.forms.is_empty() && self.project.terminal.is_none() {
            return None;
        }
        use nsh::tabs::{Snapshot, Tab};
        let titled = |id: &String| {
            self.thread
                .views
                .list()
                .iter()
                .find(|view| view.id == *id)
                .map(|view| Tab { id: view.id.clone(), title: view.title.clone() })
                .or_else(|| self.thread.forms.list().iter().find(|form| form.id == *id).map(|form| Tab { id: form.id.clone(), title: form.title.clone() }))
        };
        nsh::tabs::publish(Snapshot {
            home: if self.mode == Mode::Diff { "Changes" } else { "Files" }.into(),
            shown: if self.project.terminal_shown { Some(TERMINAL_TAB.to_string()) } else { self.thread.shown.clone() },
            tabs: self
                .project
                .terminal
                .iter()
                .map(|_| Tab { id: TERMINAL_TAB.to_string(), title: "Terminal".into() })
                .chain(self.thread.views.list().iter().map(|view| Tab { id: view.id.clone(), title: view.title.clone() }))
                .chain(self.thread.forms.list().iter().map(|form| Tab { id: form.id.clone(), title: form.title.clone() }))
                .collect(),
            offer: self.thread.offered.as_ref().and_then(titled),
        });
        Some(div().flex_none().h(TABS_HEIGHT).child(self.tabs.clone()).into_any_element())
    }

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
    pub(crate) fn diff_viewer(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        nsh::diff::publish(self.diff_snapshot(lead, trail, cx));
        div().size_full().child(self.diff.clone())
    }

    pub(crate) fn diff_snapshot(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &App) -> nsh::diff::Snapshot {
        use nsh::diff::{Body, Snapshot};
        let project = &self.project;
        let (title, body) = match (&project.picked, &project.versions) {
            (None, _) => ("Changes".to_string(), Body::Hint(self.pick_hint("a changed file"))),
            (Some(path), _) if project.rows.is_empty() && project.diffing.is_some() => (path.clone(), Body::Loading),
            (Some(path), None) => (path.clone(), Body::NoChanges),
            (Some(path), Some((old, new))) => (path.clone(), Body::Versions { path: path.clone(), old: old.clone(), new: new.clone() }),
        };
        Snapshot { glass: cx.global::<Palette>().glass.is_some(), title, lead, trail, split: self.split, counts: project.counts, body }
    }

    /// The open file in the viewer, drawn by NSH from what [`Self::source_snapshot`] publishes.
    pub(crate) fn viewer(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        nsh::source::publish(self.source_snapshot(lead, trail, cx));
        div().size_full().child(self.source.clone())
    }

    pub(crate) fn source_snapshot(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &App) -> nsh::source::Snapshot {
        use nsh::source::{Body, Snapshot};
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
            glass: cx.global::<Palette>().glass.is_some(),
            title: current.map_or_else(|| "No file open".to_string(), str::to_string),
            lead,
            trail,
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
        match thread.parent.and_then(|parent| self.title_of(parent)) {
            Some(parent) => format!("↳ {parent}"),
            None => capitalized(thread.title()),
        }
    }

    /// What the parent of children says in its header: `3 threads · 1 asking · 1 failed`.
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
