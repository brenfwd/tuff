//! The per-plugin handle table: handles are issued from a counter that
//! never wraps back, map to the task serving them, and are cancelled
//! together when the plugin unloads.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::task::AbortHandle;

use crate::wit::bindings::capability_types::Handle;

#[derive(Default)]
pub struct Handles {
    inner: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    last: Handle,
    live: HashMap<Handle, AbortHandle>,
}

impl Handles {
    /// A table whose first handle is `last + 1`.
    pub fn starting_at(last: Handle) -> Self {
        Self {
            inner: Mutex::new(Table {
                last,
                live: HashMap::new(),
            }),
        }
    }

    /// Issues a handle and registers the task `spawn` starts for it. The
    /// table stays locked across `spawn`, so the task's own [`finish`]
    /// cannot run before it is registered.
    ///
    /// [`finish`]: Handles::finish
    pub fn open(&self, spawn: impl FnOnce(Handle) -> AbortHandle) -> Option<Handle> {
        let mut table = self.inner.lock().unwrap();
        let handle = table.last.checked_add(1)?;
        table.last = handle;
        let abort = spawn(handle);
        table.live.insert(handle, abort);
        Some(handle)
    }

    pub fn finish(&self, handle: Handle) {
        self.inner.lock().unwrap().live.remove(&handle);
    }

    pub fn contains(&self, handle: Handle) -> bool {
        self.inner.lock().unwrap().live.contains_key(&handle)
    }

    /// Cancels the task behind `handle`; no event follows. False when the
    /// handle is unknown or already finished.
    pub fn cancel(&self, handle: Handle) -> bool {
        match self.inner.lock().unwrap().live.remove(&handle) {
            Some(abort) => {
                abort.abort();
                true
            }
            None => false,
        }
    }

    pub fn cancel_all(&self) {
        for (_, abort) in self.inner.lock().unwrap().live.drain() {
            abort.abort();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
