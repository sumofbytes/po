//! Methods of [`Reader`], grouped by what they do.

use crate::*;

impl Reader {
    /// Sends the draft: as a new turn, or, while the agent is working, into the queue
    /// (`later` for after the turn rather than into it).
    pub(crate) fn send(&mut self, later: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.thread.draft.trim().to_string();
        if prompt.is_empty() && self.thread.attachments.is_empty() {
            return;
        }
        self.submit(Outgoing::user(prompt, self.thread.attachments.clone()), later, window, cx);
    }

    /// Takes a message headed for the agent wherever the thread is: a blank or unstarted one
    /// starts; one still starting, loading or waking from sleep waits; a busy one is steered
    /// (dex) or queued; a ready idle one dispatches now. The draft is cleared exactly where it
    /// was before, so a message that can't be sent yet stays where the user can edit it.
    pub(crate) fn submit(&mut self, out: Outgoing, later: bool, window: &mut Window, cx: &mut Context<Self>) -> Where {
        let thread = &self.thread;
        let starting = thread.client.is_some()
            && (thread.applying.is_some() || !thread.ready && !thread.loading && !thread.picking && thread.restore.is_none());
        let takes_messages = thread.client.as_ref().is_some_and(|client| client.queues()) && thread.pending.is_none();
        let where_ = route(thread.blank, thread.unstarted(), thread.asleep(), starting, thread.busy, takes_messages, later);
        // Only the user's text is taken out of the draft; text from a thread or from Po was
        // never in it, and their draft is theirs.
        let from_user = matches!(out.origin, Origin::User);
        match where_ {
            Where::Start => {
                self.start_thread(out, later, window, cx);
                Where::Start
            }
            Where::Wait => {
                // The agent is still starting, or taking its settings: the message waits for
                // it, shown as sent. A sleeping thread is woken by it.
                if thread.asleep() {
                    self.wake_thread(window, cx);
                }
                if from_user {
                    self.clear_draft();
                }
                self.begin_turn(&out);
                self.thread.pending = Some(out);
                cx.notify();
                Where::Wait
            }
            Where::Dispatch => {
                if self.dispatch(out, cx) && from_user {
                    self.clear_draft();
                }
                Where::Dispatch
            }
            Where::Take(kind) => {
                if from_user {
                    self.clear_draft();
                }
                if let Some(client) = self.thread.client.as_ref() {
                    client.enqueue(&out.queued_text(), kind);
                }
                self.thread.queue.push(Queued { out, kind: Some(kind) });
                cx.notify();
                Where::Take(kind)
            }
            Where::Queue => {
                if from_user {
                    self.clear_draft();
                }
                self.thread.queue.push(Queued { out, kind: None });
                cx.notify();
                Where::Queue
            }
        }
    }

    /// Attaches pictures to the draft, read and shrunk off the UI thread. What isn't a picture
    /// is left out.
    pub(crate) fn attach(&mut self, sources: Vec<Source>, cx: &mut Context<Self>) {
        let thread = self.thread.id;
        self.thread.attach_note = None;
        cx.spawn(async move |this, cx| {
            let (made, skipped) = cx
                .background_executor()
                .spawn(async move {
                    let mut made = Vec::new();
                    let mut skipped = 0;
                    for source in sources {
                        let result = match &source {
                            Source::Bytes(bytes) => Attachment::from_bytes(bytes.clone()),
                            Source::Path(path) => Attachment::from_path(path),
                        };
                        match result {
                            Ok(attachment) => made.push(Arc::new(attachment)),
                            Err(error) => {
                                log::info!("attach: {error:#}");
                                skipped += 1;
                            }
                        }
                    }
                    (made, skipped)
                })
                .await;
            // The thread may have been switched away from meanwhile; the pictures still go to it.
            this.update(cx, |reader, cx| {
                reader.with_thread(thread, |reader, _| {
                    let room = attach::MAX_PER_MESSAGE.saturating_sub(reader.thread.attachments.len());
                    let over = made.len().saturating_sub(room);
                    reader.thread.attachments.extend(made.into_iter().take(room));
                    let mut notes = Vec::new();
                    if skipped > 0 {
                        notes.push(format!("{skipped} not attached: only PNG, JPEG, WebP and GIF pictures are taken"));
                    }
                    if over > 0 {
                        notes.push(format!("{over} not attached: a message holds at most {} pictures", attach::MAX_PER_MESSAGE));
                    }
                    reader.thread.attach_note = (!notes.is_empty()).then(|| notes.join("; "));
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sets the draft to `text` from the app itself (a cleared draft after a send, a taken-back
    /// queue row), marking the change so the screen the conversation draws picks the text up.
    pub(crate) fn update_draft(&mut self, text: &str) {
        self.chat_reset.set(self.chat_reset.get() + 1);
        self.thread.draft = text.to_string();
    }

    /// Clears the draft, and the pictures attached to it.
    pub(crate) fn clear_draft(&mut self) {
        self.thread.attachments.clear();
        self.thread.attach_note = None;
        self.update_draft("");
    }

    /// Takes a queued message back into the draft to edit.
    pub(crate) fn recall(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.thread.queue.len() {
            return;
        }
        let item = self.thread.queue.remove(ix);
        if let (Some(kind), Some(client)) = (item.kind, &self.thread.client) {
            client.recall(&item.out.queued_text(), kind);
        }
        let draft = self.thread.draft.clone();
        self.thread.attachments.splice(0..0, item.out.images.iter().cloned());
        self.thread.attachments.truncate(attach::MAX_PER_MESSAGE);
        let text = if draft.trim().is_empty() { item.out.text } else { format!("{}\n{draft}", item.out.text) };
        self.update_draft(&text);
        cx.notify();
    }

    /// If no turn is running, sends the oldest queued message that waits here rather than in
    /// the daemon. A message that can't be sent goes back to the draft with the rest.
    pub(crate) fn send_next(&mut self, cx: &mut Context<Self>) {
        if self.thread.busy {
            return;
        }
        let Some(at) = self.thread.queue.iter().position(|q| q.kind.is_none()) else { return };
        let mut next = self.thread.queue.remove(at);
        if !self.dispatch(std::mem::replace(&mut next.out, Outgoing::user(String::new(), vec![])), cx) {
            self.thread.queue.insert(0, next);
            self.restore_queue(cx);
        }
    }

    /// Moves everything queued back into the draft, taking back what the daemon still holds.
    /// The user's own queued messages are theirs to edit again; text sent through the API is
    /// dropped, and reported to its sender rather than typed over.
    pub(crate) fn restore_queue(&mut self, cx: &mut Context<Self>) {
        if self.thread.queue.is_empty() {
            return;
        }
        let items: Vec<Queued> = std::mem::take(&mut self.thread.queue);
        let mut text = Vec::new();
        for item in items {
            if let (Some(kind), Some(client)) = (item.kind, &self.thread.client) {
                client.recall(&item.out.queued_text(), kind);
            }
            if matches!(item.out.origin, Origin::User) {
                self.thread.attachments.splice(0..0, item.out.images.iter().cloned());
                self.thread.attachments.truncate(attach::MAX_PER_MESSAGE);
                text.push(item.out.text);
            } else {
                // Kept for `thread.get` to report; the sender is notified as well.
                let dropped = &mut self.thread.dropped;
                dropped.push(item.out.text);
                if dropped.len() > 8 {
                    dropped.drain(..dropped.len() - 8);
                }
            }
        }
        let draft = self.thread.draft.clone();
        if !draft.trim().is_empty() {
            text.push(draft);
        }
        let joined = text.join("\n\n");
        self.update_draft(&joined);
        cx.notify();
    }

    /// Sends `out` as a new turn; false if it could not be sent (the error is in the thread).
    pub(crate) fn dispatch(&mut self, out: Outgoing, cx: &mut Context<Self>) -> bool {
        if !self.thread.ready || !self.send_to_agent(&out) {
            let why = if self.thread.client.is_some() && self.thread.ready { "The agent has no session yet." } else { "The agent is not ready yet." };
            self.thread.fail(why.into());
            cx.notify();
            return false;
        }
        self.begin_turn(&out);
        self.request_save(cx);
        cx.notify();
        true
    }

    /// Hands `out` to the agent, labelled by where it came from: the user's messages say what
    /// file is in view (the thread on screen's project; that is where they type), a thread's
    /// messages say which thread sent them, and Po's say they are Po's.
    pub(crate) fn send_to_agent(&self, out: &Outgoing) -> bool {
        let Some(client) = &self.thread.client else { return false };
        let context = match (&self.project.current, &out.origin) {
            (Some(file), Origin::User) => format!("[The user has `{file}` open in the viewer.]\n\n{}", out.text),
            (_, Origin::Thread { id, title }) => format!("[From thread \"{title}\" (#{id}), sent through Po.]\n\n{}", out.text),
            (_, Origin::Po) => format!("[Po]\n\n{}", out.text),
            (None, Origin::User) => out.text.clone(),
        };
        client.prompt(&context, &out.images)
    }

    /// Shows `prompt` as the start of a turn the agent is working on.
    pub(crate) fn begin_turn(&mut self, out: &Outgoing) {
        self.thread.blank = false;
        self.thread.busy = true;
        if self.thread.status == "ready" {
            self.thread.status = "working".into();
        }
        self.thread.failed = false;
        self.thread.last_error = None;
        self.thread.touched = Instant::now();
        self.thread.active_at = now_ms();
        // The message that was last stops being so: it loses its bottom padding and folds its
        // finished runs of tool calls, so it is laid out again too.
        let was_last = self.thread.messages.len().saturating_sub(1);
        self.thread.messages.push(Msg::User(out.shown(), out.images.clone()));
        self.thread.messages.push(Msg::Agent(vec![]));
        self.thread.relist(was_last);
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(client) = &self.thread.client else { return };
        client.cancel();
        for msg in &mut self.thread.messages {
            let Msg::Agent(blocks) = msg else { continue };
            for block in blocks {
                if let Block::Permission(ask) = block
                    && ask.answered.is_none() {
                        client.answer(ask.request.clone(), None);
                        ask.answered = Some("Cancelled".into());
                    }
            }
        }
        self.thread.relist(0);
        // What was waiting goes back to the draft rather than being sent into a stopped turn.
        self.restore_queue(cx);
        cx.notify();
    }

    pub(crate) fn toggle_tool(&mut self, msg: usize, block: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Tool(tool)) = blocks.get_mut(block) else { return };
        tool.open = !tool.open;
        self.thread.relist(msg);
        cx.notify();
    }

    /// Folds or unfolds the run of tool calls that starts at `block`.
    pub(crate) fn fold_run(&mut self, msg: usize, block: usize, folded: bool, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Tool(tool)) = blocks.get_mut(block) else { return };
        tool.fold = Some(folded);
        self.thread.relist(msg);
        cx.notify();
    }

    pub(crate) fn toggle_thought(&mut self, msg: usize, block: usize, cx: &mut Context<Self>) {
        let Some(Msg::Agent(blocks)) = self.thread.messages.get_mut(msg) else { return };
        let Some(Block::Thought(_, open)) = blocks.get_mut(block) else { return };
        *open = !*open;
        self.thread.relist(msg);
        cx.notify();
    }

    /// Opens every tool call's details, or closes them all when none is left to open. Runs folded
    /// to a line count as something left to open: they are unfolded, details and all.
    pub(crate) fn toggle_tools(&mut self, cx: &mut Context<Self>) {
        let mut from = None;
        for (ix, msg) in self.thread.messages.iter_mut().enumerate() {
            let Msg::Agent(blocks) = msg else { continue };
            for start in folded_runs(blocks) {
                let Some(Block::Tool(tool)) = blocks.get_mut(start) else { continue };
                tool.fold = Some(false);
                from.get_or_insert(ix);
            }
        }
        if let Some(from) = from {
            for msg in &mut self.thread.messages {
                let Msg::Agent(blocks) = msg else { continue };
                for block in blocks {
                    if let Block::Tool(tool) = block {
                        tool.open = true;
                    }
                }
            }
            self.thread.relist(from);
            cx.notify();
            return;
        }
        if let Some(from) = toggle_all(&mut self.thread.messages, |b| if let Block::Tool(tool) = b { Some(&mut tool.open) } else { None }) {
            self.thread.relist(from);
            cx.notify();
        }
    }

    /// Opens every thought, or folds them all when none is left to open.
    pub(crate) fn toggle_thoughts(&mut self, cx: &mut Context<Self>) {
        if let Some(from) = toggle_all(&mut self.thread.messages, |b| if let Block::Thought(_, shown) = b { Some(shown) } else { None }) {
            self.thread.relist(from);
            cx.notify();
        }
    }
}
