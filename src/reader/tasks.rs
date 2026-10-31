//! The tasks a window runs for as long as it lives.

use crate::*;

impl Reader {
    /// Answers what agents ask of this window through its control socket.
    pub(crate) fn serve_control(mut requests: futures::channel::mpsc::UnboundedReceiver<control::Request>, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            while let Some(request) = requests.next().await {
                let answered = handle.update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        if matches!(request.command, command::Command::EventsSubscribe) {
                            let authorized = reader.authorize(request.caller, &request.command);
                            reader.record(request.caller, "events.subscribe", authorized.as_ref().map_or_else(|why| format!("refused: {why}"), |()| "ok".into()), true);
                            match authorized {
                                Ok(()) => {
                                    request.reply.send(Ok(serde_json::json!({ "subscribed": true }))).ok();
                                    reader.subscribers.push(request.reply);
                                }
                                Err(why) => {
                                    request.reply.send(Err(why)).ok();
                                }
                            }
                            return;
                        }
                        match reader.run_command(request.caller, request.command, window, cx) {
                            command::Answer::Now(reply) => {
                                request.reply.send(reply).ok();
                            }
                            command::Answer::Later(wait) => {
                                reader.add_waiter(request.caller, wait, request.reply, window, cx);
                            }
                        };
                    })
                });
                if answered.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// What the user does on a screen drawn by NSH, run here where the window and the agents are.
    pub(crate) fn serve_nsh(window: &Window, cx: &mut Context<Self>) {
        let Some(mut commands) = nsh::bus::commands() else { return };
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            while let Some(cmd) = commands.recv().await {
                let ran = handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.run(cmd, window, cx)));
                if !matches!(ran, Ok(Ok(()))) {
                    break;
                }
            }
        })
        .detach();
    }

    /// The sidebar's count of issues follows them, whoever changes them.
    pub(crate) fn watch_issues(cx: &mut Context<Self>) {
        let mut issue_changes = issues::shared::watch();
        cx.spawn(async move |this, cx| {
            while issue_changes.changed().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The branch beside each project's name follows checkouts made outside po too.
    pub(crate) fn poll_heads(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(roots) = this.update(cx, |reader, _| {
                    reader.views().iter().filter(|view| view.project.shows_branch()).map(|view| view.project.root.clone()).collect::<Vec<_>>()
                }) else {
                    break;
                };
                // Each in its own run, so one slow repository doesn't hold up the rest. One
                // that takes too long keeps what it showed before.
                let runs = roots.into_iter().map(|root| {
                    let run = cx.background_executor().spawn({
                        let root = root.clone();
                        async move { worktree::head_of(&root) }
                    });
                    let limit = cx.background_executor().timer(Duration::from_secs(10));
                    async move {
                        match futures::future::select(run, limit).await {
                            futures::future::Either::Left((head, _)) => (root, Some(head)),
                            futures::future::Either::Right(_) => (root, None),
                        }
                    }
                });
                let found = futures::future::join_all(runs).await;
                if this
                    .update(cx, |reader, cx| {
                        let mut heads = HashMap::new();
                        for (root, head) in found {
                            if let Some(head) = head.unwrap_or_else(|| reader.heads.get(&root).cloned()) {
                                heads.insert(root, head);
                            }
                        }
                        if reader.heads != heads {
                            reader.heads = heads;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        })
        .detach();
    }

    /// Agents nobody is talking to are stopped now and then; their threads wake on demand.
    pub(crate) fn sweep_idle(window: &Window, cx: &mut Context<Self>) {
        let sweep = window.window_handle();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                let alive = sweep.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.sleep_idle(window, cx)));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
}
