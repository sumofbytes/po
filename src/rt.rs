//! The slice of gpui the [`crate::Reader`] was written against, over [`crate::host`]: a `Context`
//! that notifies and spawns, a `WeakEntity` tasks update the reader through, a `WindowHandle` for
//! the window's own thread, a background executor, and a `Window` that records what the reader
//! asked of it for the Rune host to carry out. The names are gpui's, so the reader's logic reads as
//! it always did; none of it knows a toolkit.
//!
//! Tasks run on a thread each (see [`crate::host`]); what they ask of the reader runs on the
//! window's thread, between frames, in the order asked.
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use futures::channel::oneshot;
use futures::future::{AbortHandle, Abortable};

use crate::Reader;
use crate::host::{self, Host};

/// What the window's thread owns: the reader, and the window it asks things of.
pub struct Core {
    pub reader: Reader,
    pub window: Window,
}

/// The window, as far as the reader goes: what it asked for is recorded here, and the host does it.
pub struct Window {
    host: Host<Core>,
    pub active: bool,
    /// The reader asked for the keyboard to go to the conversation's field.
    pub focus_asked: bool,
    /// The reader asked to close the window.
    pub close_asked: bool,
    /// The reader asked for a folder to be picked: the host asks, and answers with a command.
    pub folder_asked: bool,
    /// Addresses the reader asked to be opened.
    pub urls: Vec<String>,
}

impl Window {
    pub fn new(host: Host<Core>) -> Self {
        Self { host, active: true, focus_asked: false, close_asked: false, folder_asked: false, urls: vec![] }
    }

    pub fn is_window_active(&self) -> bool {
        self.active
    }

    pub fn window_handle(&self) -> WindowHandle {
        WindowHandle { host: self.host.clone() }
    }

    pub fn remove_window(&mut self) {
        self.close_asked = true;
    }

    pub fn pick_folder(&mut self) {
        self.folder_asked = true;
    }
}

/// The window is closed: nothing can be asked of the reader any more.
#[derive(Debug)]
pub struct Gone;

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the window is closed")
    }
}

impl std::error::Error for Gone {}

/// What a reader's method is given besides the window: to ask for a redraw, to start tasks.
pub struct Context<T> {
    host: Host<Core>,
    _of: PhantomData<T>,
}

impl Context<Reader> {
    pub fn new(host: Host<Core>) -> Self {
        Self { host, _of: PhantomData }
    }

    /// The view is stale: the window polls and draws.
    pub fn notify(&mut self) {
        self.host.notify();
    }

    pub fn host(&self) -> Host<Core> {
        self.host.clone()
    }

    pub fn weak_entity(&self) -> WeakEntity<Reader> {
        WeakEntity { host: self.host.clone(), _of: PhantomData }
    }

    /// Runs `f` on a thread of its own, with a way back to the reader. Dropping the task stops it.
    pub fn spawn<F, R>(&self, f: F) -> Task<R>
    where
        F: AsyncFnOnce(WeakEntity<Reader>, &mut AsyncApp) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (weak, mut app) = (self.weak_entity(), AsyncApp { host: self.host.clone() });
        spawn_thread(move || async move { f(weak, &mut app).await })
    }

    /// The window's own thread, as a task reaches it.
    pub fn window_handle(&self) -> WindowHandle {
        WindowHandle { host: self.host.clone() }
    }

    pub fn background_spawn<R: Send + 'static>(&self, work: impl Future<Output = R> + Send + 'static) -> Task<R> {
        Executor.spawn(work)
    }

}

/// What an async task holds to reach the reader.
pub struct AsyncApp {
    host: Host<Core>,
}

impl AsyncApp {
    pub fn background_executor(&self) -> Executor {
        Executor
    }

    /// A task of this task's own, with the same way back.
    pub fn spawn<F, R>(&self, f: F) -> Task<R>
    where
        F: AsyncFnOnce(&mut AsyncApp) -> R + Send + 'static,
        R: Send + 'static,
    {
        let mut app = AsyncApp { host: self.host.clone() };
        spawn_thread(move || async move { f(&mut app).await })
    }
}

/// What the window's own thread hands the closure of [`WindowHandle::update`]: the way to the
/// reader from there, which needs no waiting.
pub struct App {
    reader: *mut Reader,
    window: *mut Window,
}

/// Lets a closure that borrows from a task cross to the window's thread. Sound because the task
/// waits for it to finish ([`Reach::reach`]): the borrows outlive the call, as in a scoped thread,
/// and only one thread touches them at a time.
struct Lent<T>(T);
unsafe impl<T> Send for Lent<T> {}

/// Something a [`WeakEntity`] is updated through: from a task (waits for the window's thread) or
/// from the window's thread (runs at once).
pub trait Reach {
    fn reach<R>(&mut self, host: &Host<Core>, f: impl FnOnce(&mut Reader, &mut Window, &mut Context<Reader>) -> R) -> Result<R, Gone>;
}

/// Runs `job` on the window's thread and waits for what it answers.
fn on_window<R>(host: &Host<Core>, job: impl FnOnce(&mut Core) -> R) -> Result<R, Gone> {
    let (tx, rx) = std::sync::mpsc::channel::<Lent<R>>();
    let job: Box<dyn FnOnce(&mut Core) + '_> = Box::new(move |core| {
        tx.send(Lent(job(core))).ok();
    });
    // SAFETY: see `Lent`: the call below does not return before the job has run or been dropped.
    let job: Box<dyn FnOnce(&mut Core) + Send + 'static> = unsafe { std::mem::transmute(job) };
    if !host.update(job) {
        return Err(Gone);
    }
    rx.recv().map(|answer| answer.0).map_err(|_| Gone)
}

impl Reach for AsyncApp {
    fn reach<R>(&mut self, host: &Host<Core>, f: impl FnOnce(&mut Reader, &mut Window, &mut Context<Reader>) -> R) -> Result<R, Gone> {
        let own = host.clone();
        on_window(host, move |core| f(&mut core.reader, &mut core.window, &mut Context::new(own)))
    }
}

impl Reach for App {
    fn reach<R>(&mut self, host: &Host<Core>, f: impl FnOnce(&mut Reader, &mut Window, &mut Context<Reader>) -> R) -> Result<R, Gone> {
        // Already on the window's thread, inside one of its jobs: they hold what they reach.
        let (reader, window) = unsafe { (&mut *self.reader, &mut *self.window) };
        Ok(f(reader, window, &mut Context::new(host.clone())))
    }
}

/// A handle the tasks reach the reader through.
pub struct WeakEntity<T> {
    host: Host<Core>,
    _of: PhantomData<T>,
}

impl<T> Clone for WeakEntity<T> {
    fn clone(&self) -> Self {
        Self { host: self.host.clone(), _of: PhantomData }
    }
}

impl WeakEntity<Reader> {
    /// Runs `f` on the window's thread with the reader; waits for what it returned unless already
    /// there. An error once the window is gone.
    pub fn update<C: Reach, R>(&self, cx: &mut C, f: impl FnOnce(&mut Reader, &mut Context<Reader>) -> R) -> Result<R, Gone> {
        cx.reach(&self.host, move |reader, _, cx| f(reader, cx))
    }

    /// Reads the reader the same way.
    pub fn read_with<C: Reach, R>(&self, cx: &mut C, f: impl FnOnce(&Reader, &()) -> R) -> Result<R, Gone> {
        cx.reach(&self.host, move |reader, _, _| f(reader, &()))
    }
}

/// The window's own thread, as a task reaches it.
#[derive(Clone)]
pub struct WindowHandle {
    host: Host<Core>,
}

impl WindowHandle {
    /// Runs `f` on the window's thread, with the window and a way to the reader from there.
    pub fn update<R>(&self, _: &mut AsyncApp, f: impl FnOnce(&mut (), &mut Window, &mut App) -> R) -> Result<R, Gone> {
        on_window(&self.host, move |core| {
            let mut app = App { reader: &mut core.reader, window: &mut core.window };
            f(&mut (), unsafe { &mut *app.window }, &mut app)
        })
    }
}

/// Work for another thread.
#[derive(Clone, Copy)]
pub struct Executor;

impl Executor {
    pub fn spawn<R: Send + 'static>(&self, work: impl Future<Output = R> + Send + 'static) -> Task<R> {
        spawn_thread(move || work)
    }

    pub fn timer(&self, wait: Duration) -> impl Future<Output = ()> + Send + Unpin + use<> {
        Box::pin(host::timer(wait))
    }
}

/// A task running on a thread of its own: awaiting it gives what it returned, dropping it stops
/// it, and [`Task::detach`] lets it run on.
pub struct Task<R> {
    done: oneshot::Receiver<R>,
    abort: Option<AbortHandle>,
}

impl<R> Task<R> {
    pub fn detach(mut self) {
        self.abort = None;
    }
}

impl<R> Drop for Task<R> {
    fn drop(&mut self) {
        if let Some(abort) = &self.abort {
            abort.abort();
        }
    }
}

impl<R> Future for Task<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<R> {
        match Pin::new(&mut self.done).poll(cx) {
            Poll::Ready(Ok(value)) => Poll::Ready(value),
            // Stopped: what was awaited never comes.
            Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
        }
    }
}

/// Runs what `make` builds on a thread of its own. The future is built there, so it need not be
/// `Send`: only what it is made from does.
fn spawn_thread<W: Future<Output = R>, R: Send + 'static>(make: impl FnOnce() -> W + Send + 'static) -> Task<R> {
    let (tx, done) = oneshot::channel();
    let (abort, registration) = AbortHandle::new_pair();
    std::thread::Builder::new()
        .name("po-task".into())
        .spawn(move || {
            if let Ok(value) = futures::executor::block_on(Abortable::new(make(), registration)) {
                tx.send(value).ok();
            }
        })
        .expect("a thread for the task");
    Task { done, abort: Some(abort) }
}
