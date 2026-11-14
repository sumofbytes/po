//! What the [`crate::reader::state::Reader`] needs from a toolkit, with none of its types: wake the window, run
//! something on the window's thread, and wait for work or time off it. Rune has no executor, so a
//! [`Host`] runs each task on a thread of its own and hands every "update the state" back through
//! a channel the window drains in its `poll` ([`Inbox::run`]).
//!
//! A task is a future that talks to the state only through [`Host::update`] and [`Host::query`],
//! which run on the window's thread, so the state needs no locks and no `Send`.

pub(crate) mod app;
pub(crate) mod rt;
use std::future::Future;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use futures::channel::oneshot;

/// Wakes the window from another thread so it polls: Rune's `Waker`.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

/// The handle tasks hold: cheap to clone, and usable from any thread.
pub struct Host<S> {
    jobs: Sender<Job<S>>,
    wake: Wake,
}

impl<S> Clone for Host<S> {
    fn clone(&self) -> Self {
        Self {
            jobs: self.jobs.clone(),
            wake: self.wake.clone(),
        }
    }
}

/// What the window drains: the updates tasks have asked for.
pub struct Inbox<S> {
    jobs: Receiver<Job<S>>,
}

/// A host for state `S`, and the inbox its window drains.
pub fn host<S>(wake: Wake) -> (Host<S>, Inbox<S>) {
    let (jobs, inbox) = mpsc::channel();
    (Host { jobs, wake }, Inbox { jobs: inbox })
}

impl<S: 'static> Inbox<S> {
    /// Runs every update waiting, in the order asked for; true when there were any, which is when
    /// the view is stale.
    pub fn run(&self, state: &mut S) -> bool {
        let mut ran = false;
        while let Ok(job) = self.jobs.try_recv() {
            job(state);
            ran = true;
        }
        ran
    }
}

impl<S: 'static> Host<S> {
    /// Asks the window to poll, as `cx.notify()` asked it to draw.
    pub fn notify(&self) {
        (self.wake)();
    }

    /// Runs `f` on the window's thread with the state, and wakes the window. False once the window
    /// is gone, which is when a task should stop.
    pub fn update(&self, f: impl FnOnce(&mut S) + Send + 'static) -> bool {
        let sent = self.jobs.send(Box::new(f)).is_ok();
        if sent {
            self.notify();
        }
        sent
    }

    /// Runs `f` on the window's thread and answers with what it returned; `None` once the window is
    /// gone.
    #[allow(dead_code)]
    pub async fn query<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = oneshot::channel();
        let sent = self
            .jobs
            .send(Box::new(move |state| drop(tx.send(f(state)))))
            .is_ok();
        if !sent {
            return None;
        }
        self.notify();
        rx.await.ok()
    }

    /// Runs `task` to its end on a thread of its own, like `cx.spawn(..).detach()`.
    #[allow(dead_code)]
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        std::thread::Builder::new()
            .name("po-task".into())
            .spawn(move || futures::executor::block_on(task))
            .expect("a thread for the task");
    }

    /// Runs blocking `work` on a thread of its own, and answers with what it returned: the
    /// counterpart of `cx.background_spawn`. `None` if the work panicked.
    #[allow(dead_code)]
    pub async fn background<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("po-work".into())
            .spawn(move || drop(tx.send(work())))
            .expect("a thread for the work");
        rx.await.ok()
    }
}

/// Resolves after `wait`, on a thread of its own, like `cx.background_executor().timer(wait)`.
pub async fn timer(wait: Duration) {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(wait);
        tx.send(()).ok();
    });
    rx.await.ok();
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    fn counting() -> (Wake, Arc<AtomicUsize>) {
        let woken = Arc::new(AtomicUsize::new(0));
        let wake = woken.clone();
        (
            Arc::new(move || {
                wake.fetch_add(1, Ordering::SeqCst);
            }),
            woken,
        )
    }

    /// Polls `inbox` until `done`, the way a window polls each frame.
    fn pump<S: 'static>(inbox: &Inbox<S>, state: &mut S, done: impl Fn(&S) -> bool) {
        let start = Instant::now();
        while !done(state) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the task finished in time"
            );
            inbox.run(state);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn an_update_runs_on_the_windows_thread_and_wakes_it() {
        let (wake, woken) = counting();
        let (host, inbox) = host::<Vec<u32>>(wake);
        let mut state = vec![];
        host.spawn({
            let host = host.clone();
            async move {
                host.update(|s| s.push(1));
                host.update(|s| s.push(2));
            }
        });
        pump(&inbox, &mut state, |s| s.len() == 2);
        assert_eq!(state, [1, 2], "in the order asked for");
        assert!(
            woken.load(Ordering::SeqCst) >= 2,
            "each update woke the window"
        );
    }

    #[test]
    fn a_query_answers_with_what_the_state_said() {
        let (wake, _) = counting();
        let (host, inbox) = host::<Vec<u32>>(wake);
        let mut state = vec![7, 8, 9];
        let answer = Arc::new(std::sync::Mutex::new(None));
        host.spawn({
            let (host, answer) = (host.clone(), answer.clone());
            async move {
                *answer.lock().unwrap() = host.query(|s| s.len()).await;
            }
        });
        pump(&inbox, &mut state, |_| answer.lock().unwrap().is_some());
        assert_eq!(*answer.lock().unwrap(), Some(3));
    }

    #[test]
    fn background_work_and_timers_resolve_off_the_window_thread() {
        let (wake, _) = counting();
        let (host, inbox) = host::<Vec<String>>(wake);
        let mut state = vec![];
        host.spawn({
            let host = host.clone();
            async move {
                let started = Instant::now();
                timer(Duration::from_millis(30)).await;
                let waited = started.elapsed() >= Duration::from_millis(30);
                let sum = host.background(|| (1..=10).sum::<u32>()).await;
                host.update(move |s| s.push(format!("{waited} {sum:?}")));
            }
        });
        pump(&inbox, &mut state, |s| !s.is_empty());
        assert_eq!(state, ["true Some(55)"]);
    }

    #[test]
    fn once_the_window_is_gone_a_task_hears_of_it() {
        let (wake, _) = counting();
        let (host, inbox) = host::<()>(wake);
        drop(inbox);
        assert!(!host.update(|_| {}), "the update has nowhere to go");
        assert_eq!(futures::executor::block_on(host.query(|_| 1)), None);
    }
}
