//! Startup timing, like a `--verbose` for launch: `PO_TRACE=1` prints how long each step took,
//! `PO_TRACE=exit` also quits after the first frame so launches can be benchmarked in a loop.
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<(Instant, u8)> = OnceLock::new();

pub fn init() {
    let mode = match std::env::var("PO_TRACE").as_deref() {
        Ok("exit") => 2,
        Ok(_) => 1,
        Err(_) => 0,
    };
    START.get_or_init(|| (Instant::now(), mode));
}

pub fn mark(step: &str) {
    if let Some((start, _)) = START.get().filter(|(_, mode)| *mode > 0) {
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        eprintln!("po trace: {:>7.2} ms  {step}  [{:02}.{:03}]", start.elapsed().as_secs_f64() * 1e3, wall.as_secs() % 60, wall.subsec_millis());
    }
}

pub fn exit_after_first_frame() -> bool {
    START.get().is_some_and(|(_, mode)| *mode == 2)
}
