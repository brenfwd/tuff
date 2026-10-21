// SPDX-License-Identifier: MIT
//! Running code later or repeatedly, in server ticks (20 per second):
//!
//! ```ignore
//! use tuff_plugin_sdk::schedule::{after, every};
//!
//! every(20 * 60, || { broadcast("a minute passed"); });
//! let reminder = after(100, || { broadcast("five seconds later"); });
//! reminder.cancel();
//! ```
//!
//! Both work from any callback. The host refuses timers while
//! registering, so what [`crate::Plugin::register`] schedules starts when
//! the plugin is enabled. Each runs as a task on the SDK's executor (see
//! [`crate::exec`]); there are no threads.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::exec;

type Start = Box<dyn FnOnce()>;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static WAITING: RefCell<Vec<Start>> = const { RefCell::new(Vec::new()) };
}

/// A scheduled run; [`Repeat::cancel`] stops it. Dropping it does not.
#[derive(Clone, Debug, Default)]
pub struct Repeat {
    cancelled: Rc<Cell<bool>>,
}

impl Repeat {
    pub fn cancel(&self) {
        self.cancelled.set(true);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }
}

fn start(task: impl FnOnce() + 'static) {
    if ENABLED.with(Cell::get) {
        task();
    } else {
        WAITING.with(|w| w.borrow_mut().push(Box::new(task)));
    }
}

/// Runs `f` once after `ticks` ticks.
pub fn after(ticks: u64, f: impl FnOnce() + 'static) -> Repeat {
    let repeat = Repeat::default();
    let cancelled = repeat.cancelled.clone();
    start(move || {
        exec::spawn(async move {
            if exec::wait(ticks).await && !cancelled.get() {
                f();
            }
        });
    });
    repeat
}

/// Runs `f` every `ticks` ticks (at least 1) until cancelled.
pub fn every(ticks: u64, mut f: impl FnMut() + 'static) -> Repeat {
    let repeat = Repeat::default();
    let cancelled = repeat.cancelled.clone();
    start(move || {
        exec::spawn(async move {
            while exec::wait(ticks.max(1)).await && !cancelled.get() {
                f();
            }
        });
    });
    repeat
}

/// The plugin was enabled: starts what was scheduled while registering.
/// [`crate::plugin!`] calls it before [`crate::Plugin::enable`].
#[doc(hidden)]
pub fn enabled() {
    ENABLED.with(|e| e.set(true));
    for task in WAITING.with(|w| std::mem::take(&mut *w.borrow_mut())) {
        task();
    }
}

/// The plugin is being disabled; [`crate::plugin!`] calls it.
#[doc(hidden)]
pub fn disabled() {
    ENABLED.with(|e| e.set(false));
}
