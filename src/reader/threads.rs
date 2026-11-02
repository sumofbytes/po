//! Methods of [`Reader`], grouped by what they do.

use crate::*;

impl Reader {
    /// The user's answer to a command Po asked about: remember it and start the agent, or leave it stopped.
    pub(crate) fn answer_approval(&mut self, ix: usize, n: usize, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(ix) else { return };
        let Some(Block::Approve { agent, new_agent, mcp, resume, answered }) = blocks.get_mut(n) else { return };
        if answered.is_some() {
            return;
        }
        *answered = Some(yes);
        let (agent, new_agent, mcp, resume) = (agent.clone(), *new_agent, mcp.clone(), *resume);
        self.thread.relist(ix);
        if yes {
            if new_agent {
                config::approve(&agent);
            }
            config::approve_mcp(&mcp);
            self.connect(&agent.id, resume, window, cx);
        } else if new_agent {
            self.thread.status = "not running".into();
        } else {
            // Only the servers were turned down: the agent runs without them.
            config::decline_mcp(&mcp);
            self.connect(&agent.id, resume, window, cx);
        }
        cx.notify();
    }

    /// Watches the workspace: refreshes the explorer when files come or go, and
    /// reloads the open file when it changes on disk.
    pub(crate) fn watch(&mut self, prepared: Option<futures::channel::oneshot::Receiver<Option<Watch>>>, window: &Window, cx: &mut Context<Self>) {
        use notify::Watcher;
        let root = self.project.root.clone();
        let handle = window.window_handle();
        // Kept so the watcher stops when the root changes.
        self.project.watching = Some(cx.spawn(async move |this, cx| {
            // Adding the watches takes a while on a big tree; the window doesn't wait for it.
            let watch = match prepared {
                Some(prepared) => prepared.await.ok().flatten(),
                None => {
                    let target = root.clone();
                    cx.background_executor().spawn(async move { start_watcher(&target) }).await
                }
            };
            // Moved in so the watcher lives as long as this task.
            let Some((mut watcher, mut rx)) = watch else { return };
            while let Some(first) = rx.next().await {
                // Let a burst of events settle, then handle them together.
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let mut events = vec![first];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                // Files coming, going or being renamed change the explorer; edits alone don't,
                // so only those re-walk the tree.
                let (changed, moved, touched) = hotpath::measure_block!("watch batch", {
                    let (mut changed, mut moved) = (false, false);
                    let mut touched = HashSet::new();
                    let (gitignore, _) = ignore::gitignore::Gitignore::new(root.join(".gitignore"));
                    for event in events.iter().filter(|e| !e.kind.is_access()) {
                        for path in &event.paths {
                            let Ok(rel) = path.strip_prefix(&root) else { continue };
                            if rel.components().any(|c| ignored(&c.as_os_str().to_string_lossy()))
                                || gitignore.matched_path_or_any_parents(rel, path.is_dir()).is_ignore()
                            {
                                continue;
                            }
                            changed = true;
                            moved |= !matches!(
                                event.kind,
                                notify::EventKind::Modify(notify::event::ModifyKind::Data(_) | notify::event::ModifyKind::Metadata(_))
                            );
                            touched.insert(rel.to_string_lossy().replace('\\', "/"));
                            if path.is_dir() {
                                watch_dirs(&mut watcher, path);
                            }
                        }
                    }
                    (changed, moved, touched)
                });
                if !changed {
                    continue;
                }
                let Ok(current) = this.read_with(cx, |reader, _| reader.project.current.clone()) else { break };
                let scan_root = root.clone();
                let Ok(base) = this.read_with(cx, |reader, _| reader.project.base.clone()) else { break };
                let (files, seen, changes) = cx
                    .background_executor()
                    .spawn(async move {
                        let files = moved.then(|| {
                            let mut files = Vec::new();
                            walk(&scan_root, &mut files);
                            files
                        });
                        let seen = current.as_ref().and_then(|path| stamp(&scan_root.join(path)));
                        (files, seen, diff::changes(&scan_root, &base))
                    })
                    .await;
                let alive = handle.update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        if let Some(files) = files {
                            reader.set_files(files, cx);
                        }
                        let rediff = reader.project.picked.as_ref().is_some_and(|path| touched.contains(path));
                        reader.apply_changes(changes, false, rediff, cx);
                        if let (Some(path), Some(seen)) = (reader.project.current.clone(), seen)
                            && reader.project.stamp != Some(seen) {
                                log::info!("reloading {path}");
                                reader.open(&path, window, cx);
                            }
                    })
                });
                if !matches!(alive, Ok(Ok(()))) {
                    break;
                }
            }
            drop(watcher.unwatch(&root));
        }));
    }

    /// Starts `key`'s agent in the thread on screen, with a fresh conversation, or with the
    /// agent's session list to resume from when `resume`.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn connect(&mut self, key: &str, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.generation += 1;
        self.thread.failed = false;
        self.thread.last_error = None;
        let generation = self.thread.generation;
        let id = self.thread.id;
        self.thread.client = None;
        self.thread.agent = Some(key.to_string());
        self.thread.messages.clear();
        self.thread.relist(0);
        self.thread.busy = false;
        self.thread.queue.clear();
        self.thread.ready = false;
        self.thread.options.clear();
        self.thread.applying = None;
        self.thread.loading = false;
        self.thread.picking = resume;
        // Listing sessions to resume is not a blank thread, even before one is picked.
        self.thread.blank &= !resume;
        self.thread.sessions = None;
        let agent = self.agents.iter().find(|agent| agent.id == key).cloned().unwrap_or_else(|| config::Agent {
            id: key.to_string(),
            name: key.to_string(),
            command: key.to_string(),
            daemon: None,
            vouched: false,
        });
        let new_agent = !config::trusted(&agent);
        // A dex daemon is not handed MCP servers.
        let mcp = if agent.daemon.is_none() { config::unapproved_mcp(&config::load()) } else { vec![] };
        if new_agent || !mcp.is_empty() {
            // Nothing is started until the user has seen the commands and said yes.
            self.thread.status = "waiting for approval".into();
            self.thread.messages.push(Msg::Agent(vec![Block::Approve { agent, new_agent, mcp, resume, answered: None }]));
            self.thread.relist(0);
            cx.notify();
            return;
        }
        self.thread.status = if resume { "listing sessions…" } else { "starting…" }.into();
        // The token the agent's MCP server gets is made once the child exists, so that it is
        // bound to the child's process: from then on it works only from that process and its
        // descendants (see `control`).
        let control = self.control.clone();
        let registered = move |pid: Option<u32>| control.as_ref().map(|control| control.register(id, pid)).unwrap_or_default();
        match Client::spawn(&agent, &self.project.root, resume, registered) {
            Ok((client, mut events)) => {
                self.thread.client = Some(client);
                self.thread.started_grant = Some(self.grants.get(&id).copied().unwrap_or_else(command::Grant::initial));
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    // An agent that never answers the handshake would leave the thread on "starting…".
                    let watchdog = this.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(20)).await;
                        handle
                            .update(cx, |_, _, cx| {
                                watchdog.update(cx, |reader, cx| {
                                    reader.with_thread(id, |reader, _| {
                                        let thread = &reader.thread;
                                        if thread.generation == generation && !thread.ready && thread.client.is_some() {
                                            // What was typed goes back to the draft rather than wait for an agent that is gone.
                                            reader.return_pending();
                                            reader.restore_queue(cx);
                                            reader.thread.status = "not responding".into();
                                            reader.thread.fail("The agent did not answer the handshake. Check that its command speaks ACP.".into());
                                            reader.unstart();
                                        }
                                    });
                                    cx.notify();
                                })
                            })
                            .ok();
                    })
                    .detach();
                    while let Some(event) = events.next().await {
                        let done = handle.update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                // The thread may have been closed, or its agent replaced, since.
                                reader
                                    .with_thread(id, |reader, visible| {
                                        let current = reader.thread.generation == generation;
                                        if current {
                                            // The waiters and the parent hear of a change of state.
                                            reader.observed(window, cx, |reader, window, cx| reader.on_event(event, visible, window, cx));
                                        }
                                        current
                                    })
                                    .unwrap_or(false)
                            })
                        });
                        if !matches!(done, Ok(Ok(true))) {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                log::error!("agent: {error:#}");
                self.thread.status = "not running".into();
                self.thread.fail(format!("{error:#}"));
            }
        }
        self.enforce_cap(window, cx);
        cx.notify();
    }

    /// Opens a blank thread, ready for a message, with its agent starting so that its settings
    /// can be chosen first. The thread on screen keeps running in the background; if it is
    /// itself still blank it is used instead.
    pub(crate) fn new_blank_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread.blank {
            self.add_thread();
        }
        self.start_blank(window, cx);
        self.focus_draft(window, cx);
        cx.notify();
    }

    /// Asks the conversation to put the keyboard in its message field.
    pub(crate) fn focus_draft(&self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.chat_focus.set(self.chat_focus.get() + 1);
    }

    /// The agent a new thread would start with.
    pub(crate) fn chosen(&self) -> Option<&config::Agent> {
        self.chosen_agent.as_ref().and_then(|key| self.agents.iter().find(|agent| agent.id == *key)).or(self.agents.first())
    }

    /// Sets a new session's settings to what was last chosen for its agent, in the order of
    /// [`Thread::ordered`]. A first message waits until they are taken.
    pub(crate) fn apply_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.applying = Some(Applying { carrying: true, ..Default::default() });
        self.apply_next(window, cx);
    }

    /// Applies what the API asked a thread's session to change, without carrying over the
    /// choices of earlier threads; when the session has none to take, the waiting message goes.
    pub(crate) fn apply_overrides(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.overrides.is_empty() {
            return self.send_pending(cx);
        }
        self.thread.applying = Some(Applying::default());
        self.apply_next(window, cx);
    }

    /// Asks the agent for the next setting to take: what the API asked for first, then (while
    /// `carrying`) what was last chosen for its agent — judged by what it offers now, so that
    /// one it only offers after a change is carried over too. Once none are left, the first
    /// message goes.
    pub(crate) fn apply_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = &mut self.thread;
        let (Some(applying), Some(key), Some(client)) = (thread.applying.as_mut(), &thread.agent, &thread.client) else {
            thread.applying = None;
            return self.send_pending(cx);
        };
        let chosen = self.choices.get(key);
        let options: Vec<&ConfigOption> = thread.options.iter().filter(|option| !applying.settled.contains(&option.id)).collect();
        for option in Thread::ordered(options) {
            applying.settled.insert(option.id.clone());
            // What the API asked for beats what was carried over; when this session was woken
            // only for the API's settings, carried choices don't come in at all.
            // Never a mode, though: see `thread.set_option`.
            let asked = thread.overrides.get(&option.id).filter(|_| option.category != "mode");
            let want = if applying.carrying { asked.or(chosen.and_then(|chosen| chosen.get(&option.id))) } else { asked };
            let Some(value) = want.cloned() else { continue };
            if value != option.current && option.choices.iter().any(|(choice, _)| *choice == value) && client.set_option(&option.id, &value) {
                applying.asked = Some(option.id.clone());
                applying.asks += 1;
                // An agent that never answers would hold the first message back for good.
                let (id, generation, asked, ask, handle) =
                    (thread.id, thread.generation, option.id.clone(), applying.asks, window.window_handle());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(20)).await;
                    handle
                        .update(cx, |_, window, cx| {
                            this.update(cx, |reader, cx| {
                                reader.with_thread(id, |reader, _| {
                                    let thread = &mut reader.thread;
                                    let waiting = thread.generation == generation
                                        && thread.applying.as_ref().is_some_and(|applying| applying.asks == ask && applying.asked.is_some());
                                    if waiting {
                                        log::warn!("agent: no answer to the setting {asked}");
                                        thread.unanswered.insert(asked);
                                        reader.apply_next(window, cx);
                                    }
                                });
                            })
                            .ok();
                        })
                        .ok();
                })
                .detach();
                return;
            }
        }
        thread.applying = None;
        self.send_pending(cx);
        // The queued messages waited for the settings; they go now.
        self.send_next(cx);
    }

    /// Sends the first message, once the agent is ready and has the settings it starts with.
    pub(crate) fn send_pending(&mut self, cx: &mut Context<Self>) {
        if !self.thread.ready || self.thread.applying.is_some() {
            return;
        }
        if let Some(out) = self.thread.pending.take()
            && !self.send_to_agent(&out)
        {
            self.thread.pending = Some(out);
            self.return_pending();
            self.thread.fail("The agent has no session yet.".into());
            self.restore_queue(cx);
        }
    }

    /// The agent answered a change of setting `id`. If it was the one being carried over, the
    /// next follows; true then, as the choice it carried over is already remembered.
    pub(crate) fn took_setting(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let carried = self.thread.applying.as_ref().is_some_and(|applying| applying.asked.as_deref() == Some(id));
        if carried {
            self.apply_next(window, cx);
        }
        // One given up on was asked for too, so it isn't the user's choice either.
        carried || self.thread.unanswered.remove(id)
    }

    /// Chooses the agent new threads start with. A blank thread on screen switches to it.
    pub(crate) fn pick_agent(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chosen_agent = Some(key.clone());
        if self.thread.blank && self.thread.agent.as_ref() != Some(&key) {
            log::info!("agent: blank thread switches to {key}");
            self.connect(&key, false, window, cx);
        }
        cx.notify();
    }

    /// Asks the agent to change a setting of the thread's session. Once it accepts, the choice is
    /// remembered for the agent's next threads.
    pub(crate) fn choose_option(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        let thread = &mut self.thread;
        if let Some(client) = &thread.client
            && client.set_option(id, value)
            && let Some(applying) = &mut thread.applying
        {
            // What is chosen here isn't then undone by carrying over an earlier choice.
            applying.settled.insert(id.to_string());
        }
        cx.notify();
    }

    /// Lists an agent's sessions in a new thread, to resume one: those of the agent the thread on
    /// screen is running, or of the chosen one if it has not started.
    pub(crate) fn resume_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else { return };
        log::info!("agent: resuming with {key}");
        self.add_thread();
        self.connect(&key, true, window, cx);
    }

    /// Starts the blank thread on screen with `out` as its first message: in the session its
    /// agent already has open, or in a worktree of its own if that was asked for. `later` is as
    /// for [`Self::send`].
    pub(crate) fn start_thread(&mut self, out: Outgoing, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone())) else {
            self.thread.fail("No agents are configured.".into());
            cx.notify();
            return;
        };
        if self.finishing {
            return;
        }
        // The agent the user last chose is theirs to change; text sent through the API never
        // moves that default.
        if matches!(out.origin, Origin::User) {
            self.chosen_agent = Some(key.clone());
        }
        if self.thread.isolate && worktree::in_repo(&self.project.root) {
            // The draft stays until the worktree exists, so a failure loses nothing.
            let blank = (self.project_id, self.thread.id);
            self.thread.status = "creating worktree…".into();
            self.new_worktree_thread(&key, Some((out, blank)), window, cx);
            return;
        }
        log::info!("agent: new thread with {key}");
        // An agent that failed or was stopped starts again, without the error it left.
        if self.thread.client.is_none() {
            self.connect(&key, false, window, cx);
        }
        // One that couldn't be started has said why, and the next message tries again.
        if self.thread.client.is_none() {
            // Text the user typed stays in their draft; text sent through the API has nowhere
            // else to be, so it waits here for the agent to be approved and started.
            if !matches!(out.origin, Origin::User) {
                self.thread.pending = Some(out);
            }
            return;
        }
        self.thread.blank = false;
        self.submit(out, later, window, cx);
        self.request_save(cx);
    }

    /// Starts a child of the thread `id`: parked in its project, never shown, its grant
    /// Viewer, its first message waiting for the agent. The reply says where it got to: a
    /// thread with an agent that isn't approved says `approval` until the user has clicked.
    pub(crate) fn create_thread(&mut self, id: u64, command: command::Command, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        let command::Command::ThreadCreate { prompt, agent, options, notify } = command else {
            return Err("not a thread.create".into());
        };
        let key = self.child_agent(id, agent)?;
        let Some(project) = self.project_of(id) else { return Err("that thread is gone".into()) };
        self.start_child(id, project, key, prompt, options, notify, window, cx)
    }

    /// The agent a thread of `id`'s may start, if it may start another: one `agent.list` knows,
    /// the caller's own by default.
    pub(crate) fn child_agent(&self, id: u64, agent: Option<String>) -> Result<String, String> {
        let key = match agent {
            Some(key) => key,
            None => self.threads_all().find(|thread| thread.id == id).and_then(|thread| thread.agent.clone()).unwrap_or_default(),
        };
        if self.agents.iter().all(|known| known.id != key) {
            return Err(format!("no agent `{key}`; agent.list has the ids"));
        }
        if self.threads_all().filter(|thread| thread.parent == Some(id)).count() >= MAX_CHILDREN {
            return Err(format!("a thread starts at most {MAX_CHILDREN} others; close one first"));
        }
        Ok(key)
    }

    /// Starts a thread of parent `id`'s in project `project`, parked, with `prompt` as its
    /// first message.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_child(&mut self, id: u64, project: u64, key: String, prompt: String, options: Vec<(String, String)>, notify: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> command::Reply {
        // The parent may have gone, or started others, since the request was made: a worktree
        // takes a while, and this is where it comes to a thread.
        let Some(title) = self.title_of(id) else { return Err("that thread is gone".into()) };
        if self.threads_all().filter(|thread| thread.parent == Some(id)).count() >= MAX_CHILDREN {
            return Err(format!("a thread starts at most {MAX_CHILDREN} others; close one first"));
        }
        let level = notify.as_deref().and_then(Notify::from_name).unwrap_or_default();
        let out = Outgoing { text: prompt, origin: Origin::Thread { id, title }, images: vec![] };
        let mut fresh = self.blank_thread();
        fresh.blank = false;
        fresh.parent = Some(id);
        fresh.notify = level;
        for (setting, value) in options {
            fresh.overrides.insert(setting.clone(), value);
            fresh.api_options.insert(setting);
        }
        let fresh_id = fresh.id;
        if project == self.project_id {
            self.parked.push(fresh);
        } else if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == project) {
            slot.parked.push(fresh);
        } else {
            return Err("that thread is gone".into());
        }
        let state = self.with_thread(fresh_id, |reader, _| {
            reader.observed(window, cx, |reader, window, cx| {
                reader.connect(&key, false, window, cx);
                if reader.thread.client.is_some() {
                    // Shown as sent, as a message typed while the agent starts is.
                    reader.begin_turn(&out);
                    reader.thread.pending = Some(out);
                } else {
                    // The agent isn't approved or won't run yet; the message waits for it, the
                    // way one typed while the agent starts does.
                    reader.thread.pending = Some(out);
                }
            });
            crate::state::state(&reader.thread.facts()).name().to_string()
        });
        self.request_save(cx);
        cx.notify();
        Ok(serde_json::json!({ "thread": fresh_id, "state": state.unwrap_or_else(|| "starting".into()) }))
    }

    /// Puts a first message back in the queue after the agent failed before it could be sent,
    /// still labelled by where it came from.
    pub(crate) fn return_pending(&mut self) {
        let Some(out) = self.thread.pending.take() else { return };
        let prompt = out.shown();
        let shown = matches!(self.thread.messages.as_slice(), [.., Msg::User(text, _), Msg::Agent(blocks)] if *text == prompt && blocks.is_empty());
        if shown {
            let keep = self.thread.messages.len() - 2;
            self.thread.messages.truncate(keep);
            self.thread.relist(keep);
        }
        self.thread.queue.insert(0, Queued { out, kind: None });
    }

    /// Makes a thread with no agent yet.
    pub(crate) fn blank_thread(&mut self) -> Thread {
        let id = self.next_thread;
        self.next_thread += 1;
        Thread::blank(id)
    }

    /// Puts a blank thread on screen, parking the one that was, unless that is still blank.
    pub(crate) fn add_thread(&mut self) {
        let fresh = self.blank_thread();
        let old = std::mem::replace(&mut self.thread, fresh);
        // A blank thread, or a session listing nothing was picked from, isn't kept.
        if old.worth_keeping() {
            self.parked.push(old);
        }
    }

    /// Runs `work` with thread `id` as [`Self::thread`] (in whichever project it belongs to), and
    /// whether it is the one on screen. `None` if there is no such thread.
    pub(crate) fn with_thread<R>(&mut self, id: u64, work: impl FnOnce(&mut Self, bool) -> R) -> Option<R> {
        if self.thread.id == id || self.parked.iter().any(|thread| thread.id == id) {
            return self.with_thread_here(id, work);
        }
        let at = self.slots.iter().position(|slot| slot.thread.id == id || slot.parked.iter().any(|thread| thread.id == id))?;
        self.swap_slot(at);
        let out = self.with_thread_here(id, |reader, _| work(reader, false));
        self.swap_slot(at);
        out
    }

    /// [`Self::with_thread`] for a thread of the project on screen.
    pub(crate) fn with_thread_here<R>(&mut self, id: u64, work: impl FnOnce(&mut Self, bool) -> R) -> Option<R> {
        if self.thread.id == id {
            return Some(work(self, true));
        }
        let at = self.parked.iter().position(|thread| thread.id == id)?;
        std::mem::swap(&mut self.thread, &mut self.parked[at]);
        let out = work(self, false);
        std::mem::swap(&mut self.thread, &mut self.parked[at]);
        Some(out)
    }

    /// Brings thread `id` on screen.
    pub(crate) fn show_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(at) = self.parked.iter().position(|thread| thread.id == id) {
            std::mem::swap(&mut self.thread, &mut self.parked[at]);
            self.parked.retain(Thread::worth_keeping);
            self.thread.unread = false;
            self.wake_thread(window, cx);
            self.request_save(cx);
            cx.notify();
        }
    }

    /// Starts the agent of the thread on screen again if it was stopped, resuming its session.
    /// A blank thread's agent starts afresh, unless it failed: that shows why until a message
    /// or a new thread asks for it again.
    pub(crate) fn wake_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.blank {
            if self.thread.messages.is_empty() {
                self.start_blank(window, cx);
            }
            return;
        }
        if !self.thread.asleep() {
            return;
        }
        let (Some(key), Some(session)) = (self.thread.agent.clone(), self.thread.session.clone()) else { return };
        log::info!("agent: waking {key} session {session}");
        self.connect(&key, true, window, cx);
        self.thread.restore = Some(session);
        // An agent waiting for approval hasn't started, and says so.
        if self.thread.client.is_some() {
            self.thread.status = "waking…".into();
        }
    }

    /// Starts the agent of the blank thread on screen if it has none running: its own agent, or
    /// else the chosen one.
    pub(crate) fn start_blank(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.blank
            && self.thread.client.is_none()
            && let Some(key) = self.thread.agent.clone().or_else(|| self.chosen().map(|agent| agent.id.clone()))
        {
            self.connect(&key, false, window, cx);
        }
    }

    /// The threads not on screen, in every project.
    pub(crate) fn background_threads(&mut self) -> impl Iterator<Item = &mut Thread> {
        self.parked.iter_mut().chain(self.slots.iter_mut().flat_map(|slot| std::iter::once(&mut slot.thread).chain(slot.parked.iter_mut())))
    }

    /// A thread with no agent running, to be resumed from `saved`'s session when it is shown.
    pub(crate) fn sleeping_thread(&mut self, saved: workspace::ThreadState) -> Thread {
        let mut thread = self.blank_thread();
        thread.agent = Some(saved.agent);
        thread.session = Some(saved.session);
        thread.saved_title = Some(saved.title).filter(|title| !title.is_empty());
        thread.blank = false;
        thread.active_at = saved.active_at;
        thread.status = "asleep".into();
        thread
    }

    /// Brings back the projects and threads the last window started here left. The project on
    /// screen is the one `po` was started on; the others wait in the background and the
    /// threads sleep until shown. True if the project on screen got its threads back.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn restore(&mut self, saved: workspace::Workspace, cx: &mut Context<Self>) -> bool {
        let mut resumed = false;
        let mut seen = HashSet::new();
        // Children of threads that came back with them, by the parent's session.
        let mut linked: Vec<(u64, String)> = vec![];
        self.side_open = saved.viewer_open;
        // The Settings page's choice wins over the layout the last window saved.
        self.swapped = config::viewer_left(&config::load()).unwrap_or(saved.swapped);
        self.agent_open = saved.agent_open;
        self.sidebars.update(cx, |sidebars, _| sidebars.set_widths(saved.left_width.map(px), saved.right_width.map(px)));
        self.tree_cols.update(cx, |cols, _| cols.set_widths(saved.explorer_width.map(px), None));
        self.chosen_agent = saved.agent.clone().or(self.chosen_agent.take());
        for state in saved.projects {
            let worktree = match state.worktree {
                Some(spec) => {
                    let branch = spec.branch.clone();
                    match worktree::Worktree::adopt(spec) {
                        Some(wt) => Some(wt),
                        None => {
                            log::warn!("worktree {} could not be restored; branch {branch} is left in its repository", state.root.display());
                            self.worktree_status = Some(format!("A worktree could not be restored; its branch {branch} is left in the repository"));
                            continue;
                        }
                    }
                }
                None => None,
            };
            let start = worktree.is_none() && self.project.pr.is_none() && self.project.worktree.is_none() && state.root == self.project.root;
            if !start && (!state.root.is_dir() || !seen.insert(state.root.clone()) || state.root == self.project.root) {
                continue;
            }
            // Files saved before the time was kept list the threads oldest first.
            let mut threads: Vec<Thread> = state
                .threads
                .into_iter()
                .enumerate()
                .map(|(ix, mut t)| {
                    if t.active_at == 0 {
                        t.active_at = ix as u64 + 1;
                    }
                    let parent = t.parent_session.clone();
                    let thread = self.sleeping_thread(t);
                    linked.extend(parent.map(|parent| (thread.id, parent)));
                    thread
                })
                .collect();
            let shown = (!threads.is_empty()).then(|| threads.remove(state.active.min(threads.len() - 1)));
            if start {
                self.project.closed = state.closed;
                if let Some(shown) = shown {
                    self.thread = shown;
                    self.parked = threads;
                    resumed = true;
                }
                continue;
            }
            let mut project = Self::build_project(state.root, None, vec![]);
            project.closed = state.closed;
            if let Some(wt) = worktree {
                project.base = wt.spec.base.clone();
                project.worktree = Some(wt);
            }
            let thread = match shown {
                Some(thread) => thread,
                None => self.blank_thread(),
            };
            let id = self.next_project;
            self.next_project += 1;
            let mode = if state.review { Mode::Diff } else { Mode::Source };
            self.slots.push(Slot { id, mode, project, thread, parked: threads });
        }
        self.relink_children(linked);
        resumed
    }

    /// Links each `(child, parent session)` to the thread of this window that has that session.
    /// A parent that is not here leaves its child an ordinary thread.
    pub(crate) fn relink_children(&mut self, linked: Vec<(u64, String)>) {
        let parents: HashMap<String, u64> = self.threads_all().filter_map(|thread| Some((thread.session.clone()?, thread.id))).collect();
        let mut wanted: HashMap<u64, u64> = linked.into_iter().filter_map(|(child, session)| Some((child, *parents.get(&session)?))).filter(|(child, parent)| child != parent).collect();
        // The file can be written by an agent, so it is not trusted to keep the plan's shape:
        // depth stays at 1 (a child of a child is left ordinary, which also rules out cycles),
        // and a parent keeps at most MAX_CHILDREN.
        let children: HashSet<u64> = wanted.keys().copied().collect();
        wanted.retain(|_, parent| !children.contains(parent));
        let mut count: HashMap<u64, usize> = HashMap::new();
        let mut order: Vec<u64> = wanted.keys().copied().collect();
        order.sort_unstable();
        for child in order {
            let n = count.entry(wanted[&child]).or_default();
            *n += 1;
            if *n > MAX_CHILDREN {
                wanted.remove(&child);
            }
        }
        if wanted.is_empty() {
            return;
        }
        let link = |thread: &mut Thread| thread.parent = wanted.get(&thread.id).copied().or(thread.parent);
        link(&mut self.thread);
        self.background_threads().for_each(link);
    }

    /// Every open project with its threads, oldest first.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn views(&self) -> Vec<View<'_>> {
        let mut views = vec![View {
            id: self.project_id,
            mode: self.mode,
            project: &self.project,
            shown: self.thread.id,
            threads: std::iter::once(&self.thread).chain(&self.parked).collect(),
        }];
        views.extend(self.slots.iter().map(|slot| View {
            id: slot.id,
            mode: slot.mode,
            project: &slot.project,
            shown: slot.thread.id,
            threads: std::iter::once(&slot.thread).chain(&slot.parked).collect(),
        }));
        views.sort_by_key(|view| view.id);
        // The thread on screen is first among them; lists keep the order they were made in.
        for view in &mut views {
            view.threads.sort_by_key(|thread| thread.id);
        }
        views
    }

    /// What is worth keeping of the projects and threads: those with an agent session.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn workspace(&self, cx: &App) -> workspace::Workspace {
        // A PR is a throwaway checkout; it is not brought back.
        let all: Vec<&Thread> = self.threads_all().collect();
        let projects = self
            .views()
            .into_iter()
            .filter(|view| view.project.pr.is_none())
            .map(|view| {
                let mut threads: Vec<&Thread> = view.threads.into_iter().filter(|t| !t.blank && t.agent.is_some() && t.session.is_some()).collect();
                threads.sort_by_key(|t| t.id);
                let open: HashSet<&str> = all.iter().filter_map(|t| t.session.as_deref()).collect();
                workspace::ProjectState {
                    closed: view.project.closed.iter().filter(|c| !open.contains(c.session.as_str())).cloned().collect(),
                    root: view.project.root.clone(),
                    review: AgentChange::saved(self.agent_modes.get(&view.id), view.mode) == Mode::Diff,
                    worktree: view.project.worktree.as_ref().map(|wt| wt.spec.clone()),
                    active: threads.iter().position(|t| t.id == view.shown).unwrap_or(0),
                    threads: threads
                        .into_iter()
                        .filter_map(|t| {
                            let parent_session = t.parent.and_then(|parent| all.iter().find(|p| p.id == parent)).and_then(|p| p.session.clone());
                            Some(workspace::ThreadState { agent: t.agent.clone()?, session: t.session.clone()?, title: t.title(), active_at: t.active_at, parent_session })
                        })
                        .collect(),
                }
            })
            .collect();
        let panes = AgentChange::saved(self.agent_panes.as_ref(), self.panes());
        workspace::Workspace { projects, viewer_open: panes.viewer, agent_open: panes.agent, swapped: self.swapped, agent: self.chosen_agent.clone(), explorer_width: Some(self.tree_cols.read(cx).widths().0.into()), left_width: Some(self.sidebars.read(cx).widths().0.into()), right_width: self.sidebars.read(cx).widths().1.map(f32::from) }
    }

    /// Saves the workspace once the current change has settled, so that a thread that is
    /// swapped in to handle an event isn't saved as the one on screen.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn request_save(&mut self, cx: &mut Context<Self>) {
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        let this = cx.entity();
        cx.defer(move |cx| {
            this.update(cx, |reader, cx| {
                reader.save_pending = false;
                reader.workspace(cx).save(&mut reader.known_projects);
            })
        });
    }

    /// Stops the agents of threads that have been idle in the background for too long.
    pub(crate) fn sleep_idle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(after) = self.idle_after else { return };
        let mut slept: Vec<u64> = vec![];
        for thread in self.background_threads().filter(|t| t.restful() && t.touched.elapsed() > after) {
            log::info!("agent: stopping idle thread {}", thread.id);
            thread.sleep();
            slept.push(thread.id);
        }
        for id in slept {
            // The parent of a sleeping child hears, so it can wait or wake it.
            self.changed(id, crate::state::ThreadState::Asleep, window, cx);
            cx.notify();
        }
    }

    /// Stops the longest idle agents in the background while more than `max_live` run.
    pub(crate) fn enforce_cap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut live = usize::from(self.thread.client.is_some());
        live += self.background_threads().filter(|t| t.client.is_some()).count();
        let Some(excess) = live.checked_sub(self.max_live).filter(|n| *n > 0) else { return };
        let mut idle: Vec<&mut Thread> = self.background_threads().filter(|t| t.restful()).collect();
        // Blank threads lose nothing by stopping, so they go first.
        idle.sort_by_key(|t| (!t.blank, t.touched));
        let mut slept = vec![];
        for thread in idle.into_iter().take(excess) {
            log::info!("agent: stopping thread {} to stay under the limit", thread.id);
            thread.sleep();
            slept.push(thread.id);
        }
        for id in slept {
            self.changed(id, crate::state::ThreadState::Asleep, window, cx);
        }
    }

    /// Tells the desktop about `what` the thread on screen did, unless that is already in view.
    pub(crate) fn alert(&self, window: &Window, visible: bool, what: &str) {
        if self.notifications && !(visible && window.is_window_active()) {
            alert::send(&format!("{} · {}", self.project.name(), self.thread.title()), what);
        }
    }

    /// Brings up thread `tid` of project `pid`, whichever is on screen.
    pub(crate) fn open_thread(&mut self, pid: u64, tid: u64, window: &mut Window, cx: &mut Context<Self>) {
        // The issues page stands in for the conversation, so a thread picked from the sidebar closes it.
        self.hide_issues(window, cx);
        if pid == self.project_id {
            self.show_thread(tid, window, cx);
        } else {
            self.show_project_with(pid, Some(tid), window, cx);
        }
    }

    /// Ends thread `tid` of project `pid`, whichever is on screen.
    pub(crate) fn close_any_thread(&mut self, pid: u64, tid: u64, window: &mut Window, cx: &mut Context<Self>) {
        if pid == self.project_id {
            return self.close_thread(tid, window, cx);
        }
        let before = self.threads_all().find(|thread| thread.id == tid).map(|thread| crate::state::state(&thread.facts()));
        if before.is_some() && before != Some(crate::state::ThreadState::Closed) {
            self.changed(tid, crate::state::ThreadState::Closed, window, cx);
        }
        self.remember_closed(pid, tid);
        let last = self.slots.iter().find(|slot| slot.id == pid).is_some_and(|slot| slot.thread.id == tid && slot.parked.is_empty());
        let blank = last.then(|| self.blank_thread());
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == pid) else { return };
        if let Some(at) = slot.parked.iter().position(|thread| thread.id == tid) {
            Thread::retire(slot.parked.remove(at));
        } else if slot.thread.id == tid
            && let Some(next) = slot.parked.pop().or(blank)
        {
            Thread::retire(std::mem::replace(&mut slot.thread, next));
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Notes thread `tid` in project `pid` as closed there, for the session picker to offer back.
    fn remember_closed(&mut self, pid: u64, tid: u64) {
        let Some(closed) = self.threads_all().find(|thread| thread.id == tid).and_then(|thread| thread.closed_state()) else { return };
        if pid == self.project_id {
            self.project.remember(closed);
        } else if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == pid) {
            slot.project.remember(closed);
        }
    }

    /// The threads `tid` started, and the ones they started in turn, as (project, thread),
    /// deepest first so each is closed before the thread that started it.
    pub(crate) fn children_of(&self, tid: u64) -> Vec<(u64, u64)> {
        let mut found: Vec<(u64, u64)> = vec![];
        let mut queue = vec![tid];
        while let Some(parent) = queue.pop() {
            for view in self.views() {
                for thread in view.threads.iter().filter(|thread| thread.parent == Some(parent)) {
                    if !found.iter().any(|(_, known)| *known == thread.id) && thread.id != tid {
                        found.push((view.id, thread.id));
                        queue.push(thread.id);
                    }
                }
            }
        }
        found.reverse();
        found
    }

    /// Whether closing `tid`, with the threads it started, would lose work.
    pub(crate) fn close_loses_work(&self, tid: u64) -> bool {
        let children = self.children_of(tid);
        self.threads_all().filter(|thread| thread.id == tid || children.iter().any(|(_, child)| *child == thread.id)).any(|thread| thread.at_risk())
    }

    /// The user pressed close on a thread. It goes with the threads it started, which have no
    /// parent left to report to. If that would lose work (a running turn, a question, queued
    /// messages) in any of them, the first press only marks it and the second closes.
    pub(crate) fn close_asked(&mut self, pid: u64, tid: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_loses_work(tid) && self.closing != Some((pid, tid)) {
            self.closing = Some((pid, tid));
            cx.notify();
            return;
        }
        self.closing = None;
        for (child_project, child) in self.children_of(tid) {
            self.close_any_thread(child_project, child, window, cx);
        }
        self.close_any_thread(pid, tid, window, cx);
    }

    /// Opens a blank thread in project `pid`.
    pub(crate) fn start_thread_in(&mut self, pid: u64, window: &mut Window, cx: &mut Context<Self>) {
        if pid != self.project_id {
            self.show_project(pid, window, cx);
        }
        self.new_blank_thread(window, cx);
    }

    /// Ends thread `id`, stopping its agent. Closing the one on screen brings up another.
    pub(crate) fn close_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        // Whoever waited on it hears, as does its parent: while it is still here to be looked
        // up, since they find its parent by it.
        let before = self.threads_all().find(|thread| thread.id == id).map(|thread| crate::state::state(&thread.facts()));
        if before.is_some() && before != Some(crate::state::ThreadState::Closed) {
            self.changed(id, crate::state::ThreadState::Closed, window, cx);
        }
        self.remember_closed(self.project_id, id);
        if let Some(at) = self.parked.iter().position(|thread| thread.id == id) {
            Thread::retire(self.parked.remove(at));
        } else if self.thread.id == id {
            let next = match self.parked.pop() {
                Some(next) => next,
                None => self.blank_thread(),
            };
            Thread::retire(std::mem::replace(&mut self.thread, next));
            self.thread.unread = false;
            self.wake_thread(window, cx);
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Opens a listed session; its history arrives as updates before the agent is ready.
    pub(crate) fn resume_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A session already open in a thread is shown rather than opened a second time, and the
        // listing it was picked from goes away.
        let open = self
            .views()
            .iter()
            .find_map(|view| view.threads.iter().find(|t| t.session.as_deref() == Some(id)).map(|t| (view.id, t.id)));
        if let Some((pid, tid)) = open {
            let listing = (self.project_id, self.thread.id);
            self.open_thread(pid, tid, window, cx);
            if self.views().iter().any(|v| v.id == listing.0 && v.threads.iter().any(|t| t.id == listing.1)) {
                self.close_any_thread(listing.0, listing.1, window, cx);
            }
            return;
        }
        let Some(client) = &self.thread.client else { return };
        if self.thread.loading || !client.load(id) {
            return;
        }
        self.thread.loading = true;
        self.thread.sessions = None;
        self.thread.status = "loading session…".into();
        cx.notify();
    }


    /// `visible`: this thread is the one on screen, so its edits may move the viewer.
    #[hotpath::measure(impl_type = "Reader")]
    pub(crate) fn on_event(&mut self, event: Event, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Most events change the last message; a tool call update may change an earlier one.
        let mut changed = self.thread.messages.len().saturating_sub(1);
        self.thread.touched = Instant::now();
        // Only the agent's work counts as activity, not a session opening or its settings, and
        // not a resumed session replaying its history.
        if !self.thread.loading && matches!(event, Event::Update(_) | Event::Permission { .. } | Event::Stopped(_) | Event::Accepted { .. }) {
            self.thread.active_at = now_ms();
        }
        match event {
            Event::Sessions(sessions) => match self.thread.restore.take() {
                // A thread being woken goes straight to its own session.
                Some(id) => match &self.thread.client {
                    Some(client) if client.load(&id) => {
                        self.thread.loading = true;
                        self.thread.status = "loading session…".into();
                    }
                    _ => {
                        self.thread.status = "not running".into();
                        self.thread.fail("The session could not be resumed.".into());
                    }
                },
                None => {
                    self.thread.status = if sessions.is_empty() { "no sessions".into() } else { "pick a session".into() };
                    self.thread.sessions = Some(sessions);
                }
            },
            Event::Options(options) => {
                self.thread.options = options;
            }
            Event::OptionSet { id, value } => {
                if !self.took_setting(&id, window, cx)
                    && !self.thread.api_options.contains(&id)
                    && let Some(key) = self.thread.agent.clone()
                {
                    self.choices.entry(key.clone()).or_default().insert(id.clone(), value.clone());
                    cx.background_spawn(async move { config::save_choice(&key, &id, &value) }).detach();
                }
            }
            Event::Refused { id, text } => {
                self.thread.status = text;
                self.took_setting(&id, window, cx);
            }
            Event::Ready { agent, fresh } => {
                self.thread.ready = true;
                self.thread.loading = false;
                self.thread.picking = false;
                log::info!("agent: {agent} ready");
                self.thread.status = "ready".into();
                self.thread.session = self.thread.client.as_ref().and_then(|client| client.session());
                self.request_save(cx);
                if fresh {
                    self.apply_choices(window, cx);
                } else {
                    // A resumed session applies what the API asked for; with none, the waiting
                    // message goes.
                    self.apply_overrides(window, cx);
                }
            }
            Event::Update(update) => {
                if let Some(usage) = state::Usage::from_update(&update) {
                    self.thread.usage = Some(usage);
                }
                if let Some(ix) = self.thread.tool_message(&update) {
                    changed = changed.min(ix);
                }
                // A replayed history would otherwise open every file it ever touched.
                if let Some((path, finished)) = self.thread.update(&update).filter(|_| !self.thread.loading && visible) {
                    self.follow(&path, finished, window, cx);
                }
            }
            Event::Permission { request, title, options } => {
                self.alert(window, visible, &format!("Needs your approval: {title}"));
                self.thread.blocks().push(Block::Permission(Ask { request, title, options, answered: None }));
            }
            Event::Stopped(reason) => {
                self.thread.busy = false;
                if self.thread.status == "working" {
                    self.thread.status = "ready".into();
                }
                self.thread.unread = !visible;
                // Only when nothing else holds the keyboard: the reader's own focus is where it
                // rests, so an input (a search, the finder, a rename, the editor) has it otherwise.
                // Nothing focused, or the conversation holding it (its transcript, a chip, the Stop
                // button), counts as nothing else: the field is where the next message goes.
                let resting = window.focused(cx).is_none() || self.focus.is_focused(window) || self.chat.focus_handle(cx).contains_focused(window, cx);
                if visible && resting && !self.settings_open && !self.finder_open && !self.issues_open {
                    self.focus_draft(window, cx);
                }
                // A child's parent is told instead; the desktop hears of it only when nobody
                // is waiting on the child.
                let parent_hears = self.thread.parent.is_some() && self.thread.notify != Notify::Never;
                if !(parent_hears || visible) {
                    self.alert(window, visible, "Finished");
                }
                log::info!("agent: stopped ({reason})");
                // What the daemon didn't take into the turn is no longer in its queue.
                for item in &mut self.thread.queue {
                    item.kind = None;
                }
                // What the API asked the session to change applies between turns.
                if self.thread.client.is_some() && !self.thread.overrides.is_empty() {
                    self.apply_overrides(window, cx);
                } else {
                    self.send_next(cx);
                }
                // The order of the thread list is saved with the time the turn ended.
                self.request_save(cx);
                self.drop_waiters(command::Caller::Thread(self.thread.id), "the turn ended");
            }
            Event::Accepted { content, queue } => {
                if let Some(at) = self.thread.queue.iter().position(|q| q.kind == Some(queue) && q.out.queued_text() == content) {
                    self.thread.queue.remove(at);
                }
                self.thread.messages.push(Msg::User(content, vec![]));
            }
            Event::Rejected { content, queue } => {
                // No turn to join: it waits for the turn to end like any other queued message.
                if let Some(item) = self.thread.queue.iter_mut().find(|q| q.kind == Some(queue) && q.out.queued_text() == content) {
                    item.kind = None;
                }
                self.send_next(cx);
            }
            Event::Failed(error) => {
                if !self.thread.ready {
                    self.thread.status = "failed to start".into();
                }
                self.return_pending();
                self.thread.fail(error);
                self.restore_queue(cx);
                self.thread.applying = None;
                self.unstart();
                self.drop_waiters(command::Caller::Thread(self.thread.id), "the turn failed");
            }
            Event::Exited(status) => {
                log::warn!("agent exited: {status}");
                self.thread.ready = false;
                self.thread.status = "exited".into();
                if self.thread.busy {
                    self.return_pending();
                    self.thread.fail(format!("the agent exited ({status})"));
                    self.restore_queue(cx);
                }
                self.thread.applying = None;
                self.unstart();
                self.drop_waiters(command::Caller::Thread(self.thread.id), "the agent exited");
            }
        }
        self.thread.relist(changed);
        cx.notify();
    }

    /// After its agent failed or exited: a blank thread, or one whose first message never got
    /// a session, starts its agent again when it is next asked for. That includes one whose
    /// agent is still held but never became ready, as it would never answer.
    pub(crate) fn unstart(&mut self) {
        let thread = &mut self.thread;
        if thread.blank || thread.unstarted() || thread.session.is_none() && !thread.ready && !thread.picking && !thread.loading {
            thread.drop_agent();
        }
    }

    /// Shows the file a tool call touches; a finished call reloads it to show the change.
    pub(crate) fn follow(&mut self, path: &str, finished: bool, window: &mut Window, cx: &mut Context<Self>) {
        let path = Path::new(path);
        let relative = path.strip_prefix(&self.project.root).unwrap_or(path);
        if relative.is_absolute() || !self.project.root.join(relative).is_file() {
            return;
        }
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !self.project.files.contains(&relative) {
            self.rescan(cx);
        }
        if self.project.current.as_deref() != Some(&relative) || finished {
            log::info!("agent: showing {relative}");
            self.open(&relative, window, cx);
        }
        if self.mode == Mode::Diff {
            self.refresh_changes(false, Some(relative), cx);
        }
    }

    pub(crate) fn answer(&mut self, msg: usize, block: usize, option: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Permission(ask)) = blocks.get_mut(block) else { return };
        let (Some(choice), None) = (ask.options.get(option), &ask.answered) else { return };
        if let Some(client) = &self.thread.client {
            client.answer(ask.request.clone(), Some(&choice.id));
        }
        ask.answered = Some(choice.name.clone());
        self.thread.relist(msg);
        cx.notify();
    }
}
