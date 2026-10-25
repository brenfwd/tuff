//! A hang detector for tests: a test that is still running when its
//! watchdog expires aborts the whole run with its name, rather than
//! leaving `cargo test` waiting forever.

use std::sync::mpsc;
use std::time::Duration;

use tuff_item::item::conservation::{FixtureThread, LossScope};

/// Default budget for one test, far above what any test needs even on a
/// heavily loaded machine.
pub const DEFAULT: Duration = Duration::from_secs(600);

/// Disarms the watchdog when dropped, and fails the test if an item was
/// lost on one of its threads.
pub struct Watchdog {
    _disarm: mpsc::Sender<()>,
    _fixtures: FixtureThread,
    losses: LossScope,
    owns_losses: bool,
}

impl Watchdog {
    /// For the test's runtime builder: every thread it starts counts its
    /// item losses against this test.
    pub fn thread_start(&self) -> impl Fn() + Send + Sync + 'static {
        let losses = self.losses.clone();
        move || losses.enter()
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if !self.owns_losses {
            return;
        }
        LossScope::leave();
        let lost = self.losses.lost();
        if lost > 0 && !std::thread::panicking() {
            panic!(
                "{lost} items were lost on this test's threads:\n{}",
                self.losses.reports().join("\n")
            );
        }
    }
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
    let current = LossScope::current();
    let owns_losses = current.is_none();
    let losses = current.unwrap_or_default();
    losses.enter();
    Watchdog {
        _disarm: disarm,
        _fixtures: tuff_item::item::conservation::fixture_thread(),
        losses,
        owns_losses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn a_loss_on_a_thread_of_the_test_fails_it_with_the_stack() {
        let failure = std::thread::spawn(|| {
            let watchdog = arm(DEFAULT);
            let enter = watchdog.thread_start();
            let _ = std::thread::spawn(move || {
                enter();
                drop(ItemStack::mint(Item::Stone, 3, MintReason::Test));
            })
            .join();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(watchdog)))
                .expect_err("a lost stack fails the test")
        })
        .join()
        .unwrap();
        let message = failure.downcast_ref::<String>().unwrap();
        assert!(message.starts_with("3 items were lost"), "{message}");
        assert!(message.contains("Stone"), "{message}");
    }
}
