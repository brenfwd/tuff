//! Event delivery: results queue on one channel to the host actor, and
//! a plugin may have only `pending_events` of them waiting, so a plugin
//! that does not keep up pauses its own I/O instead of growing memory.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use crate::wit::bindings::capability_types::IoEvent;

/// One result on its way to a plugin. Dropping it frees a queue place.
pub struct Delivery {
    pub plugin: String,
    pub generation: u64,
    pub event: IoEvent,
    _place: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub struct Events {
    plugin: Arc<str>,
    generation: u64,
    queue: mpsc::UnboundedSender<Delivery>,
    places: Arc<Semaphore>,
}

impl Events {
    pub fn new(
        plugin: &str,
        generation: u64,
        pending: u32,
        queue: mpsc::UnboundedSender<Delivery>,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            generation,
            queue,
            places: Arc::new(Semaphore::new(pending.max(1) as usize)),
        }
    }

    /// Waits for a free place, then queues `event`. Returns false when
    /// the host is gone.
    pub async fn send(&self, event: IoEvent) -> bool {
        let Ok(place) = self.places.clone().acquire_owned().await else {
            return false;
        };
        self.queue
            .send(Delivery {
                plugin: self.plugin.to_string(),
                generation: self.generation,
                event,
                _place: place,
            })
            .is_ok()
    }
}
