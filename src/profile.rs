//! Profiling with hotpath (<https://hotpath.rs>), like [`crate::trace`] but for the whole run: built
//! with `--features hotpath`, Po times the functions marked `#[hotpath::measure]`, follows the
//! channels and locks made through hotpath's macros, and writes the report when it quits;
//! `--features hotpath-alloc` also counts what each function allocates. Built without them, the
//! macros are no-ops and this module does nothing. See `docs/profiling.md`.

/// Counts every allocation, so the report can say which function made it.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

/// The channel types `hotpath::channel!` hands back: hotpath's wrappers in a profiled build,
/// the channels themselves otherwise.
#[cfg(feature = "hotpath")]
pub use hotpath::wrap::futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
#[cfg(not(feature = "hotpath"))]
pub use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};

#[cfg(feature = "hotpath")]
static GUARD: std::sync::Mutex<Option<hotpath::HotpathGuard>> = std::sync::Mutex::new(None);

/// Starts measuring, in the process that opens the window (a detached launch's child, not the
/// parent that only spawns it). `HOTPATH_SHUTDOWN_MS` makes it a timed run that writes the
/// report and quits on its own.
pub fn start() {
    #[cfg(feature = "hotpath")]
    {
        let builder = hotpath::HotpathGuardBuilder::new("po").percentiles(&[50.0, 95.0, 99.0]);
        match std::env::var("HOTPATH_SHUTDOWN_MS").ok().and_then(|ms| ms.parse().ok()) {
            Some(ms) => builder.build_with_shutdown(std::time::Duration::from_millis(ms)),
            None => *GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(builder.build()),
        }
        #[cfg(unix)]
        finish_on_interrupt();
    }
}

/// Stops measuring and writes the report; only the first call does anything. Called on every way
/// out that skips the end of `main`: quitting the app, `PO_TRACE=exit`, Ctrl+C.
pub fn finish() {
    #[cfg(feature = "hotpath")]
    drop(GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take());
}

/// Ctrl+C or a `kill` in the terminal Po was started from would end it without a report: the
/// handler only notes the signal, and a thread writes the report and exits.
#[cfg(all(feature = "hotpath", unix))]
fn finish_on_interrupt() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CAUGHT: AtomicBool = AtomicBool::new(false);
    extern "C" fn caught(_: libc::c_int) {
        CAUGHT.store(true, Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, caught as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, caught as *const () as libc::sighandler_t);
    }
    std::thread::Builder::new()
        .name("profile-signal".into())
        .spawn(|| {
            while !CAUGHT.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            finish();
            std::process::exit(130);
        })
        .ok();
}
