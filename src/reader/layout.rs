//! Methods of [`Reader`], grouped by what they do.

use crate::*;

impl Reader {
    pub(crate) fn toggle_left(&mut self, cx: &mut Context<Self>) {
        self.left_open = !self.left_open;
        cx.notify();
    }

    /// Tells what the viewer is showing about views and forms that are gone, since the pane the
    /// NSH screen draws picks its view by id.
    pub(crate) fn sync_view_docs(&mut self, cx: &mut Context<Self>) {
        let live: HashSet<String> = self
            .thread
            .views
            .list()
            .iter()
            .map(|view| view.id.clone())
            .chain(self.thread.forms.list().iter().map(|form| form.id.clone()))
            .collect();
        for slot in [&mut self.thread.shown, &mut self.thread.offered] {
            if slot.as_ref().is_some_and(|id| !live.contains(id)) {
                *slot = None;
                cx.notify();
            }
        }
    }

    /// Puts the agent's view `id` in the viewer, opening it if it was closed.
    pub(crate) fn show_agent_view(&mut self, id: &str, cx: &mut Context<Self>) {
        self.thread.shown = Some(id.to_string());
        self.thread.offered = None;
        self.project.terminal_shown = false;
        self.open_viewer_wide(cx);
        cx.notify();
    }

    /// The agent itself changed the layout, not the user: its pending move is not dropped for it.
    pub(crate) fn follow_agent(&mut self) {
        let (thread, now) = (self.thread.id, self.screen());
        if let Some(pending) = &mut self.pending_move {
            pending.follows_agent(thread, &now);
        }
    }

    /// `command` with its path made one inside the project, or why there is nothing to show.
    pub(crate) fn check_move(&self, command: command::Command) -> Result<command::Command, String> {
        use command::Command;
        let root = &self.project.root;
        Ok(match command {
            Command::FileOpen { path, line } => {
                let path = command::project_path(root, &path)?;
                let file = root.join(&path);
                if !file.is_file() {
                    return Err(format!("no file `{path}` in the project"));
                }
                // A link inside the project may lead out of it.
                let inside = file.canonicalize().and_then(|file| root.canonicalize().map(|root| file.starts_with(root))).unwrap_or(false);
                if !inside {
                    return Err(format!("`{path}` is not in the project"));
                }
                Command::FileOpen { path, line }
            }
            Command::DiffShow { path: Some(path) } => {
                let path = command::project_path(root, &path)?;
                if !self.project.changes.iter().any(|change| change.path == path) {
                    return Err(format!("`{path}` has no changes"));
                }
                Command::DiffShow { path: Some(path) }
            }
            other => other,
        })
    }

    pub(crate) fn apply_move(&mut self, command: command::Command, window: &mut Window, cx: &mut Context<Self>) {
        use command::Command;
        match command {
            Command::FileOpen { path, line } => {
                self.show_viewer(Mode::Source, cx);
                self.open(&path, window, cx);
                if let (Some(line), Some(_)) = (line, &self.project.code) {
                    self.reveal_serial += 1;
                    self.reveal = Some((line.saturating_sub(1) as u32, self.reveal_serial));
                }
            }
            Command::DiffShow { path } => {
                self.show_viewer(Mode::Diff, cx);
                if let Some(path) = path {
                    self.pick(&path, cx);
                }
            }
            Command::ViewerClose => self.show_chat(cx),
            _ => {}
        }
    }

    pub(crate) fn panes(&self) -> Panes {
        Panes { viewer: self.side_open, agent: self.agent_open }
    }

    /// Runs `change`, which an agent asked for, noting what it did to the layout so the
    /// workspace goes on saving the user's.
    pub(crate) fn as_agent(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Self, &mut Context<Self>)) {
        let (panes, project, mode) = (self.panes(), self.project_id, self.mode);
        change(self, cx);
        if self.panes() != panes {
            self.agent_panes = Some(AgentChange::after(self.agent_panes, panes, self.panes()));
        }
        if self.mode != mode {
            let change = AgentChange::after(self.agent_modes.get(&project).copied(), mode, self.mode);
            self.agent_modes.insert(project, change);
        }
    }

    /// Forgets an agent's change once the layout is no longer what it left: the user moved on, and
    /// coming back to it by hand is their choice, not the agent's.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn settle_agent_changes(&mut self) {
        self.agent_panes = AgentChange::settled(self.agent_panes, self.panes());
        let mode = AgentChange::settled(self.agent_modes.get(&self.project_id).copied(), self.mode);
        match mode {
            Some(change) => self.agent_modes.insert(self.project_id, change),
            None => self.agent_modes.remove(&self.project_id),
        };
    }

    pub(crate) fn screen(&self) -> Screen {
        Screen {
            side_open: self.side_open,
            agent_open: self.agent_open,
            mode: self.mode,
            current: self.project.current.clone(),
            picked: self.project.picked.clone(),
            shown: self.thread.shown.clone(),
        }
    }

    /// Holds `command` from thread `thread` until the user has been still for [`QUIET`]. A later
    /// request replaces it; it is dropped if another thread comes on screen or the user moves the
    /// viewer first.
    pub(crate) fn defer_move(&mut self, thread: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) {
        let waiting = self.pending_move.is_some();
        self.pending_move = Some(PendingMove { thread, command, from: self.screen() });
        if waiting {
            return;
        }
        let mut wait = QUIET.saturating_sub(self.last_input.elapsed());
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(wait).await;
                let next = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.try_pending_move(window, cx)));
                match next {
                    Ok(Ok(Some(more))) => wait = more,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Applies the held move if the user has gone still, or says how much longer to wait.
    pub(crate) fn try_pending_move(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Duration> {
        let pending = self.pending_move.as_ref()?;
        if pending.thread != self.thread.id || pending.from != self.screen() {
            self.pending_move = None;
            return None;
        }
        let still = self.last_input.elapsed();
        if still < QUIET {
            return Some(QUIET - still);
        }
        let pending = self.pending_move.take()?;
        self.as_agent(cx, |reader, cx| reader.apply_move(pending.command, window, cx));
        cx.notify();
        None
    }

    /// Browse (files) or Review (changes) in the viewer pane. If the pane was closed it opens wide,
    /// since looking at code or a diff wants room; if it is open its width is left as it was.
    pub(crate) fn show_viewer(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.thread.shown = None;
        self.project.terminal_shown = false;
        self.set_mode(mode, cx);
        self.open_viewer_wide(cx);
    }

    /// The terminal in the viewer, started if the project has none yet, or back to the files if it
    /// is what the viewer shows.
    pub(crate) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.terminal_shown && self.side_open {
            self.project.terminal_shown = false;
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        let root = self.project.root.clone();
        if self.project.terminal.is_none() {
            let view = cx.new(|cx| terminal_view::TerminalView::new(root, cx));
            // A shell that ends (Ctrl+D, `exit`) takes its tab with it, in whichever project it was.
            cx.subscribe_in(&view, window, |reader, view, _: &terminal_view::Exited, window, cx| reader.terminal_exited(view, window, cx)).detach();
            self.project.terminal = Some(view);
        }
        let terminal = self.project.terminal.clone().expect("just made");
        self.project.terminal_shown = true;
        self.thread.offered = None;
        self.open_viewer_wide(cx);
        // The view is not in the tree until it draws; the focus follows.
        cx.defer_in(window, move |_, window, cx| terminal.update(cx, |terminal, cx| terminal.focus(window, cx)));
    }

    /// The terminal's tab was pressed: show it, which never hides it.
    pub(crate) fn toggle_terminal_on(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project.terminal_shown = false;
        self.toggle_terminal(window, cx);
    }

    /// `view`'s shell ended: its tab closes.
    pub(crate) fn terminal_exited(&mut self, view: &Entity<terminal_view::TerminalView>, window: &mut Window, cx: &mut Context<Self>) {
        if view.focus_handle(cx).is_focused(window) {
            window.focus(&self.focus, cx);
        }
        for project in std::iter::once(&mut self.project).chain(self.slots.iter_mut().map(|slot| &mut slot.project)) {
            if project.terminal.as_ref() == Some(view) {
                project.terminal = None;
                project.terminal_shown = false;
            }
        }
        cx.notify();
    }

    /// The terminal's tab was closed: the shell ends with its view.
    pub(crate) fn close_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project.terminal = None;
        self.project.terminal_shown = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens the viewer if it is closed, wide since looking at code or a diff wants room; if it is
    /// open its width is left as it was.
    pub(crate) fn open_viewer_wide(&mut self, cx: &mut Context<Self>) {
        if !self.side_open {
            self.side_open = true;
            // It may have been opened from the middle, where the conversation always shows.
            self.agent_open = true;
            self.sidebars.update(cx, |sidebars, _| sidebars.set_right_share(0.55));
            self.request_save(cx);
        }
        cx.notify();
    }

    /// Chat: the viewer pane closed, so the conversation has the middle to itself.
    pub(crate) fn show_chat(&mut self, cx: &mut Context<Self>) {
        if self.side_open {
            self.side_open = false;
            self.request_save(cx);
        }
        cx.notify();
    }

    /// Shows the viewer pane, which a file was just opened for, at the width it last had.
    pub(crate) fn reveal_side(&mut self, cx: &mut Context<Self>) {
        if !self.side_open {
            self.side_open = true;
            // It may have been opened from the middle, where the conversation always shows.
            self.agent_open = true;
            self.request_save(cx);
            cx.notify();
        }
    }

    /// Shows `path` where it is most useful: its diff if it has changes, the file otherwise.
    pub(crate) fn reveal(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.shown = None;
        self.project.terminal_shown = false;
        let full = Path::new(path);
        let relative = full.strip_prefix(&self.project.root).unwrap_or(full).to_string_lossy().replace('\\', "/");
        if self.project.changes.iter().any(|change| change.path == relative) {
            self.set_mode(Mode::Diff, cx);
            self.pick(&relative, cx);
        } else if self.project.root.join(&relative).is_file() {
            self.set_mode(Mode::Source, cx);
            self.open(&relative, window, cx);
        }
        self.reveal_side(cx);
    }

    pub(crate) fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        self.split = split;
        cx.notify();
    }

    /// Puts the viewer on the other side of the conversation, and remembers it in `config.json`
    /// so the Settings page shows it. The conversation shows after a swap.
    pub(crate) fn swap_panes(&mut self, cx: &mut Context<Self>) {
        self.swapped = !self.swapped;
        self.agent_open = true;
        let position = if self.swapped { "left" } else { "right" };
        if let Err(error) = config::update(|config| {
            config.insert("viewer_position".into(), position.into());
        }) {
            log::warn!("viewer position: {error}");
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Shows the viewer, or closes it. It is always the viewer, whichever side it is on.
    pub(crate) fn toggle_right(&mut self, cx: &mut Context<Self>) {
        if self.side_open {
            self.show_chat(cx);
        } else {
            self.open_viewer_wide(cx);
        }
    }

    /// With the viewer open, hides the conversation so the viewer has the room, or brings it back.
    pub(crate) fn toggle_focus(&mut self, cx: &mut Context<Self>) {
        if self.side_open {
            self.agent_open = !self.agent_open;
            self.request_save(cx);
            cx.notify();
        }
    }

    pub(crate) fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Diff {
            // Show the last known changes at once; `git` confirms them in the background.
            self.apply_changes(self.project.changes.clone(), true, false, cx);
            self.refresh_changes(true, None, cx);
        }
        cx.notify();
    }

    /// Ctrl+,: opens the settings page, or closes it when it is already open.
    pub(crate) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            return self.hide_settings(window, cx);
        }
        nsh::settings::start(self.swapped);
        self.settings_open = true;
        self.settings.update(cx, |settings, cx| settings.open(window, cx));
        cx.notify();
    }

    pub(crate) fn toggle_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.issues_open {
            return self.hide_issues(window, cx);
        }
        self.follow_issues();
        self.issues_open = true;
        self.issues.update(cx, |issues, cx| issues.open(window, cx));
        cx.notify();
    }

    pub(crate) fn hide_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.issues_open {
            return;
        }
        self.issues_open = false;
        self.issues.update(cx, |issues, _| issues.close());
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Points the shared issues at the project on screen, if it isn't already. The sidebar's
    /// snapshot does this, so the page and the counts follow the project whatever switched it.
    pub(crate) fn follow_issues(&self) {
        let mut shown = self.issues_root.borrow_mut();
        if *shown != self.project.root {
            *shown = self.project.root.clone();
            issues::shared::set_project(&self.project.root);
        }
    }

    /// Starts a thread on an issue, in a worktree of its own when the project is a repo, and moves
    /// the issue to In Progress with the branch linked.
    pub(crate) fn work_on_issue(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_issues();
        let tracker = issues::shared::watch().borrow().tracker.clone();
        if let Err(message) = issues::api::check_workable(&tracker, number) {
            self.worktree_status = Some(message);
            cx.notify();
            return;
        }
        let Some(prompt) = issues::api::work_prompt(&tracker, number) else {
            return;
        };
        // Another worktree is being made or merged, or there is no agent to start: nothing would
        // begin, so the issue is left as it is.
        if self.finishing || self.chosen().is_none() {
            return;
        }
        self.hide_issues(window, cx);
        self.new_blank_thread(window, cx);
        self.thread.isolate = true;
        let root = self.project.root.clone();
        self.issue_for_worktree = Some((root.clone(), number));
        let out = Outgoing { text: prompt, origin: Origin::User, images: vec![] };
        self.start_thread(out, false, window, cx);
        // With a worktree being made, the issue waits for its branch. Without one the thread
        // went ahead in the project itself, if it started at all.
        if !self.finishing {
            self.issue_for_worktree = None;
            if !self.thread.blank {
                Self::issue_started(&root, number, None);
            }
        }
        cx.notify();
    }

    /// `issue.show`: puts the issues page on screen where the request says. Only for the project
    /// on screen, since the page shows that one.
    pub(crate) fn show_issues(&mut self, id: u64, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        if self.project_of(id).is_some_and(|project| project != self.project_id) {
            return Err("that project is not on screen; the issues page shows the one that is".into());
        }
        // Like any move of what the user is looking at, it is the thread on screen's to make, and
        // not while the user is typing or scrolling.
        if self.threads_all().any(|thread| thread.id == id) {
            if self.thread.id != id {
                return Err("only the thread on screen can put the issues page up".into());
            }
            if self.last_input.elapsed() < QUIET {
                return Err("the user is typing or scrolling; ask again in a moment".into());
            }
        }
        self.follow_issues();
        let nav = issues::api::show_nav(&issues::shared::watch().borrow().tracker, args)?;
        if !self.issues_open {
            self.toggle_issues(window, cx);
        }
        issues::shared::navigate(nav);
        Ok(serde_json::json!({ "shown": true }))
    }

    /// An issue's work has begun: In Progress, and the branch it is on. In the issues of the repo
    /// `root` belongs to, which need not be the one on screen.
    pub(crate) fn issue_started(root: &std::path::Path, number: u64, branch: Option<String>) {
        issues::shared::change_repo(root, |tracker, now| {
            tracker.update(number, now, |issue| {
                if issue.status.is_open() && issue.status != issues::Status::InReview {
                    issue.status = issues::Status::InProgress;
                }
                if let Some(branch) = &branch {
                    let link = issues::Link::Branch(branch.clone());
                    if !issue.links.contains(&link) {
                        issue.links.push(link);
                    }
                }
            });
        });
    }

    pub(crate) fn hide_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.settings.update(cx, |settings, _| settings.close());
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Takes up the options of `config.json` that are read once, after the settings page saved
    /// them. The theme and fonts are reapplied by the config watcher.
    pub(crate) fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let settings = config::load();
        self.notifications = settings["notifications"].as_bool().unwrap_or(true);
        let idle_minutes = settings["idle_minutes"].as_u64().unwrap_or(15);
        self.idle_after = (idle_minutes > 0).then(|| Duration::from_secs(idle_minutes * 60));
        self.max_live = settings["max_live_agents"].as_u64().unwrap_or(8).max(1) as usize;
        if let Some(left) = config::viewer_left(&settings) {
            self.swapped = left;
            self.request_save(cx);
        }
        // An agent that was added on the command line isn't in the config; it stays.
        let mut agents = config::agents(&settings);
        let kept: Vec<_> = self.agents.iter().filter(|agent| agent.vouched && !agents.iter().any(|known| known.id == agent.id)).cloned().collect();
        agents.extend(kept);
        self.agents = agents;
        cx.notify();
    }
}
