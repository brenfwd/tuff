//! Background tasks: a plugin's `background-tasks.run-task` export run on
//! separate instances of its module whose imports all trap, off the tick
//! thread. Each plugin has a small pool of idle instances; an instance
//! that faults is discarded. Results come back as `io-event` background
//! through the plugin's event queue, so a plugin that does not keep up
//! also pauses its tasks' delivery.
//!
//! Limits: [`MAX_IN_FLIGHT`] tasks queued or running per plugin, input
//! and output at most the plugin's message limit, the plugin's per-call
//! fuel and memory for each task, and at most [`parallelism`] tasks
//! running at once across all plugins. Dropping a [`Background`] (the
//! plugin unloading or reloading) aborts queued tasks; one already
//! running finishes its fuel-bounded run and its result is discarded.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::Semaphore;

use crate::net::{Events, Handles};
use crate::sandbox::{Fault, Limits};
use crate::wit::bindings::capability_types::{
    BackgroundDone, BackgroundError, Denied, Handle, IoEvent,
};
use crate::wit::bindings::{self, BACKGROUND_TASKS_RUN_TASK};
use crate::worldgen::Pool;

/// Tasks one plugin may have queued or running.
pub const MAX_IN_FLIGHT: u32 = 4;
/// Longest task name, in bytes.
pub const MAX_TASK_NAME: usize = 128;
/// Idle instances kept per plugin.
const MAX_IDLE: usize = 2;
/// Background handles start above this, apart from the handles of net and
/// file operations.
const HANDLE_BASE: Handle = 0x8000_0000;

/// Tasks running at once across all plugins: half the cores, at least one.
pub fn parallelism() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))
}

fn cores() -> &'static Arc<Semaphore> {
    static CORES: OnceLock<Arc<Semaphore>> = OnceLock::new();
    CORES.get_or_init(|| Arc::new(Semaphore::new(parallelism())))
}

struct Slot(Arc<AtomicU32>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One plugin's background tasks. Must be made inside the tokio runtime
/// that runs them.
pub struct Background {
    pool: Arc<Pool>,
    events: Events,
    handles: Arc<Handles>,
    in_flight: Arc<AtomicU32>,
    limits: Limits,
    runtime: tokio::runtime::Handle,
}

impl Drop for Background {
    fn drop(&mut self) {
        self.handles.cancel_all();
    }
}

impl Background {
    pub fn new(module: Vec<u8>, limits: Limits, events: Events) -> Self {
        Self {
            pool: Arc::new(Pool::with_idle(module, limits, MAX_IDLE)),
            events,
            handles: Arc::new(Handles::starting_at(HANDLE_BASE)),
            in_flight: Arc::new(AtomicU32::new(0)),
            limits,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Queues `task` with `input`.
    pub fn run(&self, task: &str, input: &[u8]) -> Result<Handle, Denied> {
        if task.is_empty() || task.len() > MAX_TASK_NAME {
            return Err(Denied::Malformed(format!(
                "a task name is 1 to {MAX_TASK_NAME} bytes"
            )));
        }
        if input.len() > self.limits.message_bytes {
            return Err(Denied::Quota(format!(
                "task input is over {} bytes",
                self.limits.message_bytes
            )));
        }
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(Denied::Quota(format!(
                "{MAX_IN_FLIGHT} background tasks are already queued or running"
            )));
        }
        let slot = Slot(self.in_flight.clone());
        let (pool, events, handles) =
            (self.pool.clone(), self.events.clone(), self.handles.clone());
        let (task, input) = (task.to_owned(), input.to_vec());
        let max_output = self.limits.message_bytes;
        let opened = self.handles.open(|handle| {
            self.runtime
                .spawn(async move {
                    let outcome = execute(pool, task, input, max_output).await;
                    drop(slot);
                    handles.finish(handle);
                    events
                        .send(IoEvent::Background(BackgroundDone { handle, outcome }))
                        .await;
                })
                .abort_handle()
        });
        opened.ok_or_else(|| Denied::Quota("out of background handles".into()))
    }

    /// Cancels a task; no event follows.
    pub fn cancel(&self, handle: Handle) {
        self.handles.cancel(handle);
    }

    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::SeqCst)
    }
}

async fn execute(
    pool: Arc<Pool>,
    task: String,
    input: Vec<u8>,
    max_output: usize,
) -> Result<Vec<u8>, BackgroundError> {
    let Ok(_core) = cores().clone().acquire_owned().await else {
        return Err(BackgroundError::Faulted("the server is stopping".into()));
    };
    let ran = tokio::task::spawn_blocking(move || {
        pool.with(|runtime| {
            if !runtime.exports(BACKGROUND_TASKS_RUN_TASK) {
                return Err(Fault("the plugin exports no background tasks".into()));
            }
            bindings::background_tasks_run_task(runtime, &task, &input)
        })
    })
    .await;
    match ran {
        Ok(Ok(Ok(output))) if output.len() > max_output => Err(BackgroundError::TooLarge),
        Ok(Ok(Ok(output))) => Ok(output),
        Ok(Ok(Err(why))) => Err(BackgroundError::Failed(why)),
        Ok(Err(fault)) => Err(BackgroundError::Faulted(fault.0)),
        Err(_) => Err(BackgroundError::Faulted("the task panicked".into())),
    }
}

#[cfg(test)]
mod tests;
