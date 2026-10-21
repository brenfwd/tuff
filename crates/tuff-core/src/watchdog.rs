//! A hang detector for tests: a test that is still running when its
//! watchdog expires aborts the whole run with its name, rather than
//! leaving `cargo test` waiting forever.

use std::sync::mpsc;
use std::time::Duration;

/// Default budget for one test, far above what any test needs even on a
/// heavily loaded machine.
pub const DEFAULT: Duration = Duration::from_secs(600);

/// Disarms the watchdog when dropped.
pub struct Watchdog {
    _disarm: mpsc::Sender<()>,
}

/// Aborts the process if the returned guard is still alive after `limit`.
pub fn arm(limit: Duration) -> Watchdog {
    let name = std::thread::current()
        .name()
        .unwrap_or("unnamed test")
        .to_string();
    let (disarm, disarmed) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = disarmed.recv_timeout(limit) {
                eprintln!(
                    "test {name} is still running after {limit:?}; assuming it hung, aborting"
                );
                std::process::abort();
            }
        })
        .expect("cannot start the watchdog thread");
    Watchdog { _disarm: disarm }
}
