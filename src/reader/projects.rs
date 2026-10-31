//! Methods of [`Reader`], grouped by what they do.

use crate::*;

impl Reader {
    /// A project over `root`, showing `files` in its explorer until the next walk.
    pub(crate) fn build_project(root: PathBuf, pr: Option<pr::Pr>, files: Vec<String>) -> Project {
        let files_tree = FileTree::new(false);
        let changes_tree = FileTree::new(true);
        Project {
            root,
            base: pr.as_ref().map_or_else(|| "HEAD".to_string(), |pr| pr.base.clone()),
            pr,
            files,
            changes: vec![],
            marks: Rc::default(),
            diffing: None,
            listing: None,
            scanning: None,
            watching: None,
            changes_tree,
            picked: None,
            rows: Rc::new(vec![]),
            versions: None,
            counts: (0, 0),
            current: None,
            stamp: None,
            code: None,
            is_markdown: false,
            notice: None,
            files_tree,
            worktree: None,
            terminal: None,
            terminal_shown: false,
        }
    }

    /// Rebuilds the explorer from `files`, keeping folders open and the current file selected.
    pub(crate) fn refresh_tree(&mut self) {
        self.project.files_tree.set_paths(&self.project.files, self.project.current.as_deref());
    }

    /// Takes a fresh list of changed files. In diff mode it picks a file if none is, and
    /// reloads the picked file's diff when `rediff` says it may have changed.
    pub(crate) fn apply_changes(&mut self, changes: Vec<Change>, rebuild: bool, rediff: bool, cx: &mut Context<Self>) {
        let listed = rebuild || self.project.changes != changes;
        self.project.changes = changes;
        if listed {
            self.project.marks = Rc::new(self.project.changes.iter().map(|c| (c.path.clone(), c.status)).collect());
        }
        if self.mode != Mode::Diff {
            return;
        }
        if listed {
            let paths: Vec<String> = self.project.changes.iter().map(|c| c.path.clone()).collect();
            self.project.changes_tree.set_paths(&paths, None);
        }
        let keep = self.project.picked.clone().filter(|path| self.project.changes.iter().any(|c| &c.path == path));
        match keep {
            Some(path) if rediff || listed => self.pick(&path, cx),
            Some(_) => {}
            None => match self.project.changes.first().map(|c| c.path.clone()) {
                Some(path) => self.pick(&path, cx),
                None => {
                    self.project.picked = None;
                    self.project.diffing = None;
                    self.set_rows(vec![], None);
                }
            },
        }
        cx.notify();
    }

    /// Lists the changed files with `git` off the UI thread, then applies them as
    /// [`Self::apply_changes`] does and picks `then`, if given.
    pub(crate) fn refresh_changes(&mut self, rebuild: bool, then: Option<String>, cx: &mut Context<Self>) {
        let (root, base) = (self.project.root.clone(), self.project.base.clone());
        self.project.listing = Some(cx.spawn(async move |this, cx| {
            let changes = cx.background_executor().spawn(async move { diff::changes(&root, &base) }).await;
            this.update(cx, |reader, cx| {
                reader.apply_changes(changes, rebuild, true, cx);
                if let Some(path) = then.filter(|_| reader.mode == Mode::Diff) {
                    reader.pick(&path, cx);
                }
            })
            .ok();
        }));
    }

    /// Re-walks the workspace off the UI thread and refreshes the explorer.
    pub(crate) fn rescan(&mut self, cx: &mut Context<Self>) {
        let root = self.project.root.clone();
        self.project.scanning = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let mut files = Vec::new();
                    walk(&root, &mut files);
                    files
                })
                .await;
            this.update(cx, |reader, cx| {
                reader.set_files(files, cx);
            })
            .ok();
        }));
    }

    /// Adopts a fresh walk of the workspace, if it changed, in the explorer and an open finder.
    pub(crate) fn set_files(&mut self, files: Vec<String>, cx: &mut Context<Self>) {
        if self.project.files == files {
            return;
        }
        self.project.files = files;
        self.refresh_tree();
        if self.finder_open {
            self.refind();
        }
        cx.notify();
    }

    /// Shows the diff `rows` colored by `colors`, and works out their side-by-side pairs and counts once.
    pub(crate) fn set_rows(&mut self, rows: Vec<Row>, versions: Option<(String, String)>) {
        self.project.counts = rows.iter().fold((0, 0), |(a, r), row| match row.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            _ => (a, r),
        });
        self.project.rows = Rc::new(rows);
        self.project.versions = versions.map(|(old, new)| (nsh::source::Shared(Arc::from(old)), nsh::source::Shared(Arc::from(new))));
    }

    /// Shows `path`'s diff in the viewer, once `git` has produced it off the UI thread.
    pub(crate) fn pick(&mut self, path: &str, cx: &mut Context<Self>) {
        self.browse = false;
        let Some(change) = self.project.changes.iter().find(|c| c.path == path).cloned() else { return };
        if self.project.picked.as_deref() != Some(path) {
            self.project.picked = Some(path.to_string());
            // The last file's diff isn't shown under this one's name while this one loads.
            self.set_rows(vec![], None);
            self.project.changes_tree.select(path);
        }
        let (root, base, path) = (self.project.root.clone(), self.project.base.clone(), path.to_string());
        self.project.diffing = Some(cx.spawn(async move |this, cx| {
            let (rows, versions) = cx
                .background_executor()
                .spawn(async move {
                    let rows = diff::file_diff(&root, &base, &change);
                    // Only files with changed lines need their versions.
                    let versions = (!rows.is_empty()).then(|| {
                        let (old, new) = diff::versions(&root, &base, &change);
                        (old.unwrap_or_default(), new.unwrap_or_default())
                    });
                    (rows, versions)
                })
                .await;
            this.update(cx, |reader, cx| {
                if reader.project.picked.as_deref() == Some(&path) {
                    reader.set_rows(rows, versions);
                    reader.project.diffing = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Starts checking out the PR `spec` the strip asked for; the window switches to it when
    /// that finishes.
    pub(crate) fn open_pr(&mut self, spec: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = spec.trim().to_string();
        if spec.is_empty() || self.opening {
            return;
        }
        self.opening = true;
        self.pr_status = Some(format!("Checking out {spec}…"));
        cx.notify();
        // Reviewed from the checkout on screen, or from where `po` started if that is a PR.
        let repo = if self.project.pr.is_none() { self.project.root.clone() } else { self.home.clone() };
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { pr::Pr::open(&repo, &spec) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.opening = false;
                        match result {
                            Ok(pr) => {
                                reader.pr_status = None;
                                // The strip's field clears itself on seeing a bumped serial.
                                reader.pr_serial += 1;
                                let root = pr.worktree.clone();
                                reader.add_project(root, Some(pr), window, cx);
                            }
                            Err(error) => {
                                log::warn!("open PR: {error}");
                                reader.pr_status = Some(error);
                                cx.notify();
                            }
                        }
                    })
                })
                .ok();
        })
        .detach();
    }

    /// Closes the PR review, going back to another project, or to the directory `po` started in
    /// if this was the only one.
    pub(crate) fn close_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.project_id;
        if self.slots.is_empty() {
            let home = self.home.clone();
            self.add_project(home, None, window, cx);
        }
        self.close_project(id, window, cx);
    }

    /// Swaps the project on screen, with its threads and mode, for `slots[at]`.
    pub(crate) fn swap_slot(&mut self, at: usize) {
        let slot = &mut self.slots[at];
        std::mem::swap(&mut self.project_id, &mut slot.id);
        std::mem::swap(&mut self.mode, &mut slot.mode);
        std::mem::swap(&mut self.project, &mut slot.project);
        std::mem::swap(&mut self.thread, &mut slot.thread);
        std::mem::swap(&mut self.parked, &mut slot.parked);
    }

    /// Stops what only the project on screen needs (its watcher, `git` runs and the finder), so
    /// that nothing keeps updating it once another is shown. Its threads keep going, except a
    /// blank one, whose agent starts again when it is back on screen.
    pub(crate) fn suspend_project(&mut self) {
        if self.thread.blank && self.thread.client.is_some() {
            self.thread.sleep();
        }
        let project = &mut self.project;
        project.watching = None;
        project.listing = None;
        project.scanning = None;
        project.diffing = None;
        self.finder_open = false;
    }

    /// Brings a project that was swapped in up to date with the disk, and starts watching it.
    pub(crate) fn resume_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_tree();
        self.rescan(cx);
        self.watch(None, window, cx);
        if self.mode == Mode::Diff {
            self.apply_changes(self.project.changes.clone(), true, false, cx);
            self.refresh_changes(true, None, cx);
        }
        if let Some(path) = self.project.current.clone() {
            self.open(&path, window, cx);
        }
        self.thread.unread = false;
        self.pr_status = None;
        self.worktree_status = None;
        self.confirm_discard = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Shows project `id`; its threads keep going wherever they were.
    pub(crate) fn show_project(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_project_with(id, None, window, cx);
    }

    /// [`Self::show_project`], with thread `thread` of it on screen if it has one.
    pub(crate) fn show_project_with(&mut self, id: u64, thread: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.slots.iter().any(|slot| slot.id == id) {
            return;
        }
        // Leaving a session listing unused drops it.
        if self.thread.is_listing() {
            self.close_thread(self.thread.id, window, cx);
        }
        let Some(at) = self.slots.iter().position(|slot| slot.id == id) else { return };
        self.suspend_project();
        self.swap_slot(at);
        if let Some(parked) = thread.and_then(|id| self.parked.iter().position(|t| t.id == id)) {
            std::mem::swap(&mut self.thread, &mut self.parked[parked]);
            self.parked.retain(Thread::worth_keeping);
        }
        self.resume_project(window, cx);
        self.wake_thread(window, cx);
        self.request_save(cx);
    }

    /// Adds a project over `root` (a PR's worktree when `pr` is given) and shows it. If it is
    /// already open, that one is shown. An agent starts in it, the same one as in the project
    /// it was opened from.
    pub(crate) fn add_project(&mut self, root: PathBuf, pr: Option<pr::Pr>, window: &mut Window, cx: &mut Context<Self>) {
        if pr.is_none() {
            if self.project.pr.is_none() && self.project.root == root {
                return;
            }
            if let Some(id) = self.slots.iter().find(|slot| slot.project.pr.is_none() && slot.project.root == root).map(|slot| slot.id) {
                return self.show_project(id, window, cx);
            }
        }
        let agent = self.thread.agent.clone();
        let review = pr.is_some();
        let project = Self::build_project(root, pr, vec![]);
        self.open_project(project, if review { Mode::Diff } else { Mode::Source }, agent, window, cx);
    }

    /// Shows `project` in `mode`, with a thread of its own that runs `agent`, if any. The
    /// project on screen goes on running in the background.
    pub(crate) fn open_project(&mut self, project: Project, mode: Mode, agent: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.blank_thread();
        self.suspend_project();
        let id = self.next_project;
        self.next_project += 1;
        // The project on screen goes into a slot, and the new one takes its fields.
        self.slots.push(Slot { id, mode, project, thread, parked: vec![] });
        let at = self.slots.len() - 1;
        self.swap_slot(at);
        self.resume_project(window, cx);
        match agent {
            Some(agent) => self.connect(&agent, false, window, cx),
            None => self.wake_thread(window, cx),
        }
        self.request_save(cx);
    }

    /// Starts a thread with `key`'s agent in a git worktree of its own, shown as a project:
    /// a branch whose changes can be reviewed here, then merged or discarded.
    /// With `first`, the message to start it with and the blank thread it was typed in, which
    /// goes away once the worktree is up.
    pub(crate) fn new_worktree_thread(&mut self, key: &str, first: Option<(Outgoing, (u64, u64))>, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        // A worktree is made from the repository, not from another worktree.
        let from = match &self.project.worktree {
            Some(wt) => wt.spec.repo.clone(),
            None => self.project.root.clone(),
        };
        self.finishing = true;
        self.worktree_status = Some("Creating worktree…".into());
        cx.notify();
        let (key, handle) = (key.to_string(), window.window_handle());
        cx.spawn(async move |this, cx| {
            let made = cx.background_executor().spawn(async move { worktree::Worktree::create(&from) }).await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.finishing = false;
                        match made {
                            Ok(wt) => {
                                reader.worktree_status = None;
                                reader.open_worktree(wt, key, first, window, cx);
                            }
                            Err(error) => {
                                log::warn!("worktree: {error}");
                                reader.issue_for_worktree = None;
                                reader.worktree_status = Some(error);
                                if let Some((_, (_, tid))) = first {
                                    reader.with_thread(tid, |reader, _| reader.thread.status.clear());
                                }
                                cx.notify();
                            }
                        }
                    })
                })
                .ok();
        })
        .detach();
    }

    pub(crate) fn open_worktree(&mut self, wt: worktree::Worktree, agent: String, first: Option<(Outgoing, (u64, u64))>, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("worktree: {} for {agent}", wt.spec.branch);
        if let Some((root, number)) = self.issue_for_worktree.take() {
            Self::issue_started(&root, number, Some(wt.spec.branch.clone()));
        }
        let mut project = Self::build_project(wt.spec.path.clone(), None, vec![]);
        project.base = wt.spec.base.clone();
        project.worktree = Some(wt);
        self.confirm_discard = false;
        self.open_project(project, Mode::Diff, Some(agent), window, cx);
        if let Some((out, (pid, tid))) = first {
            // The new thread starts under the grant the blank one was given.
            if let Some(grant) = self.grants.get(&tid).copied() {
                self.grants.insert(self.thread.id, grant);
            }
            self.begin_turn(&out);
            self.thread.pending = Some(out);
            // The thread it was typed in stays only if something has happened to it since.
            let blank = self.views().iter().any(|v| v.id == pid && v.threads.iter().any(|t| t.id == tid && t.blank));
            if blank {
                self.close_any_thread(pid, tid, window, cx);
            }
        }
    }

    /// Merges the worktree on screen into the checkout it came from, or throws it away.
    pub(crate) fn finish_worktree(&mut self, merge: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.worktree.is_none() || self.finishing {
            return;
        }
        if !merge && !self.confirm_discard {
            self.confirm_discard = true;
            cx.notify();
            return;
        }
        self.confirm_discard = false;
        self.finish_worktree_of(self.project_id, merge, None, window, cx);
    }

    /// [`Self::finish_worktree`] for project `id`, on screen or not, once it is decided. With
    /// `tell`, that thread hears how it went.
    pub(crate) fn finish_worktree_of(&mut self, id: u64, merge: bool, tell: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.project_by_id(id).and_then(|project| project.worktree.as_ref()).map(|wt| wt.spec.clone()) else {
            if let Some(thread) = tell {
                self.tell(thread, format!("Project {id} is not a worktree any more."), window, cx);
            }
            return;
        };
        if self.finishing {
            if let Some(thread) = tell {
                self.tell(thread, "The user agreed to merge, but another worktree was being made or merged; ask again.".into(), window, cx);
            }
            return;
        }
        self.finishing = true;
        self.worktree_status = Some(if merge { "Merging…" } else { "Discarding…" }.into());
        let handle = window.window_handle();
        let title = self.views().iter().find(|view| view.id == id).and_then(|view| view.threads.iter().find(|thread| thread.id == view.shown)).map(|thread| thread.title()).unwrap_or_default();
        let message = format!("po: {title}");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if merge {
                        worktree::Worktree::merge(&spec, &message).map(Some)
                    } else {
                        worktree::Worktree::discard(&spec);
                        Ok(None)
                    }
                })
                .await;
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |reader, cx| {
                        reader.finishing = false;
                        let told = match result {
                            Ok(into) => {
                                // A merged branch finishes the issues linked to it.
                                let landed = into.is_some().then(|| reader.project_by_id(id).and_then(|p| p.worktree.as_ref()).map(|wt| (wt.spec.repo.clone(), wt.spec.branch.clone()))).flatten();
                                if let Some((repo, branch)) = landed {
                                    issues::shared::branch_merged(&repo, &branch);
                                }
                                reader.close_worktree_project(id, window, cx);
                                reader.api_projects.remove(&id);
                                let status = match into {
                                    Some(into) => format!("Merged into {into}"),
                                    None => "Discarded".into(),
                                };
                                reader.worktree_status = Some(status.clone());
                                status
                            }
                            Err(error) => {
                                log::warn!("worktree: {error}");
                                reader.worktree_status = Some(error.clone());
                                format!("Not merged: {error}")
                            }
                        };
                        if let Some(thread) = tell {
                            reader.tell(thread, told, window, cx);
                        }
                        cx.notify();
                    })
                })
                .ok();
        })
        .detach();
    }

    /// Project `id`, on screen or not.
    pub(crate) fn project_by_id(&mut self, id: u64) -> Option<&mut Project> {
        if self.project_id == id {
            return Some(&mut self.project);
        }
        self.slots.iter_mut().find(|slot| slot.id == id).map(|slot| &mut slot.project)
    }

    /// Closes the project of a worktree that has been merged or discarded, going back to the
    /// project it came from if that is open.
    pub(crate) fn close_worktree_project(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let origin = self.project_by_id(id).and_then(|project| project.worktree.as_ref()).map(|wt| wt.spec.repo.clone());
        if let Some(wt) = self.project_by_id(id).and_then(|project| project.worktree.as_mut()) {
            wt.finished();
        }
        let parent = origin.and_then(|repo| {
            std::iter::once((self.project_id, &self.project)).chain(self.slots.iter().map(|s| (s.id, &s.project)))
                .find(|(pid, project)| *pid != id && project.worktree.is_none() && project.pr.is_none() && project.root == repo)
                .map(|(pid, _)| pid)
        });
        if let Some(parent) = parent {
            self.show_project(parent, window, cx);
        }
        self.close_project(id, window, cx);
    }

    /// Asks for a folder and opens it as a project.
    pub(crate) fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open project".into()),
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(root) = paths.into_iter().next().and_then(|path| path.canonicalize().ok()) else { return };
            handle.update(cx, |_, window, cx| this.update(cx, |reader, cx| reader.add_project(root, None, window, cx))).ok();
        })
        .detach();
    }

    /// Closes project `id`: its agents stop, and a PR's worktree is removed. Never the last one.
    pub(crate) fn close_project(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.slots.is_empty() {
            return;
        }
        if self.project_id == id {
            // Show another first; the closed project is then the one in the slot.
            let Some(other) = self.slots.last().map(|slot| slot.id) else { return };
            self.show_project(other, window, cx);
        }
        let Some(at) = self.slots.iter().position(|slot| slot.id == id) else { return };
        // Its agents stop with the slot; a worktree is then closed off the UI thread without
        // losing what was done in it, which the user is told.
        let mut slot = self.slots.remove(at);
        let worktree = slot.project.worktree.take();
        drop(slot);
        if let Some(mut wt) = worktree {
            cx.spawn(async move |this, cx| {
                let kept = cx.background_executor().spawn(async move { wt.retire() }).await;
                if let Some(branch) = kept {
                    this.update(cx, |reader, cx| {
                        reader.worktree_status = Some(format!("Closed. Its work is kept on branch {branch}"));
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }
        self.request_save(cx);
        cx.notify();
    }

    /// Ctrl+P: shows the finder over the workspace's files, with the last query selected.
    pub(crate) fn show_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder_open = true;
        self.refind();
        nsh::finder::start(&self.finder_query);
        self.finder.update(cx, |finder, cx| finder.open(window, cx));
        cx.notify();
    }

    /// Refilters the finder's matches for its query, and shows them.
    pub(crate) fn refind(&mut self) {
        self.finder_matches = match_files(&self.project.files, &self.finder_query);
        nsh::finder::publish(nsh::finder::Snapshot {
            matches: self.finder_matches.clone(),
            capped: (self.project.files.len() >= MAX_FILES).then_some(MAX_FILES),
        });
    }

    /// Opens `path` in the viewer and closes the finder.
    pub(crate) fn accept_path(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.thread.shown = None;
        self.project.terminal_shown = false;
        self.open(path, window, cx);
        // A file that can't be shown leaves the finder up so the query isn't lost.
        if self.project.notice.is_none() {
            self.hide_finder(window, cx);
            self.set_mode(Mode::Source, cx);
            self.reveal_side(cx);
        }
    }

    /// Hides the finder and gives the keys back to the reader.
    pub(crate) fn hide_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder_open = false;
        self.finder.update(cx, |finder, _| finder.close());
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn open(&mut self, path: &str, _window: &mut Window, cx: &mut Context<Self>) {
        self.browse = false;
        let full = self.project.root.join(path);
        let too_big = std::fs::metadata(&full).is_ok_and(|m| m.len() > 4 * 1024 * 1024);
        // A file too big to show isn't read at all.
        let read = if too_big { Ok(None) } else { std::fs::read_to_string(&full).map(Some) };
        match read {
            Ok(None) => {
                self.project.code = None;
                self.project.is_markdown = false;
                self.project.notice = Some(format!("{path} is too large to show"));
            }
            Ok(Some(text)) => {
                self.project.is_markdown = is_markdown(path);
                // The same file keeps its place: the editor swaps the text in, and the scroll position
                // and caret survive.
                self.project.code = Some(nsh::source::Shared(Arc::from(text)));
                self.project.notice = None;
            }
            Err(error) => {
                log::warn!("open {path}: {error}");
                self.project.code = None;
                self.project.is_markdown = false;
                self.project.notice = Some(format!("{path} is not a text file"));
            }
        }
        self.project.current = Some(path.to_string());
        self.project.stamp = stamp(&full);
        self.project.files_tree.select(path);
        cx.notify();
    }

    /// The file tree beside the viewer, drawn by NSH from what it publishes. `lead` and `trail` are
    /// the window's own controls, when this is the leftmost or rightmost pane.
    pub(crate) fn explorer(&self, lead: Vec<nsh::header::Control>, trail: Vec<nsh::header::Control>, cx: &mut Context<Self>) -> AnyElement {
        let diff = self.mode == Mode::Diff;
        let tree = if diff { &self.project.changes_tree } else { &self.project.files_tree };
        let body = if diff && self.project.changes.is_empty() {
            nsh::explorer::Body::Empty("No changes".into())
        } else {
            let marks = &self.project.marks;
            nsh::explorer::Body::Rows(
                tree.rows()
                    .into_iter()
                    .map(|row| nsh::explorer::Entry { mark: diff.then(|| marks.get(row.id.as_str()).copied()).flatten(), row })
                    .collect(),
            )
        };
        nsh::strip::publish(nsh::strip::Snapshot {
            worktree: self.project.worktree.as_ref().map(|wt| nsh::strip::Worktree {
                branch: wt.spec.branch.clone(),
                confirm_discard: self.confirm_discard,
            }),
            pr: self
                .project
                .pr
                .as_ref()
                .map(|pr| nsh::strip::Pr::Opened { label: format!("PR #{} · {} files", pr.number, self.project.changes.len()) })
                .or_else(|| diff.then(|| nsh::strip::Pr::Asking { status: self.pr_status.clone(), opening: self.opening })),
            pr_reset: self.pr_serial,
        });
        nsh::explorer::publish(nsh::explorer::Snapshot { glass: cx.global::<Palette>().glass.is_some(), changes: diff, lead, trail, body });
        div().size_full().child(self.explorer_pane.clone()).into_any_element()
    }

    /// Opens, or shows the diff of, the file selected in the explorer's tree, as clicking it does.
    pub(crate) fn open_selected(&mut self, by_user: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Source => {
                let Some(path) = self.project.files_tree.selected_file().map(str::to_string) else { return };
                // A file picked in the tree is what the viewer shows, not an agent's view.
                if by_user {
                    self.thread.shown = None;
                    self.project.terminal_shown = false;
                }
                if self.project.current.as_deref() != Some(&path) {
                    self.open(&path, window, cx);
                }
            }
            Mode::Diff => {
                let Some(path) = self.project.changes_tree.selected_file().map(str::to_string) else { return };
                if by_user {
                    self.thread.shown = None;
                    self.project.terminal_shown = false;
                }
                if self.project.picked.as_deref() != Some(&path) {
                    self.pick(&path, cx);
                }
            }
        }
        if by_user {
            self.reveal_side(cx);
        }
    }

    /// Switches a narrow viewer pane between the explorer and the file.
    pub(crate) fn set_browsing(&mut self, browse: bool, cx: &mut Context<Self>) {
        self.browse = browse;
        cx.notify();
    }

    /// Whether a thread answers the sidebar's filter: its title or its latest line says it.
    pub(crate) fn matches_query(&self, thread: &Thread, query: &str) -> bool {
        self.thread_label(thread).to_lowercase().contains(query) || thread.preview().to_lowercase().contains(query)
    }

    /// The file tree is drawn beside the viewer: the pane is wide enough for both.
    pub(crate) fn tree_visible(&self) -> bool {
        !self.viewer_narrow.get()
    }

    pub(crate) fn pick_hint(&self, what: &str) -> String {
        pick_hint(what, self.tree_visible())
    }
}
