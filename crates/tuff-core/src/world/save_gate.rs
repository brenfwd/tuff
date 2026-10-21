//! Holding the world's saving still, for jobs that copy its files (a
//! backup): stop automatic saves, flush everything, then keep every write
//! away from storage until the hold is released. The chunk storage actor
//! owns the gate (see `chunk_io`); this composes it with the coordinator's
//! flush.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::world::Shared;
use crate::world::chunk_io::{ChunkIo, HoldBusy};
use crate::world::messages::CoordinatorMessage;

/// How long a hold may last before saving resumes on its own, so a job
/// that died cannot leave saving off.
pub const DEFAULT_HOLD_LIMIT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldError {
    /// Another job holds saving.
    Busy(HoldBusy),
    /// The world could not be flushed (it is stopping).
    FlushFailed,
    /// The hold lapsed or was lifted before it closed.
    Lapsed,
}

impl std::fmt::Display for HoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(busy) => write!(f, "saving is already held by {}", busy.owner),
            Self::FlushFailed => f.write_str("the world could not be saved"),
            Self::Lapsed => f.write_str("the hold on saving lapsed"),
        }
    }
}

impl std::error::Error for HoldError {}

/// Saving held still: everything saved before the hold is on disk and
/// nothing reaches storage until this is released or dropped (or its limit
/// passes). Writes made meanwhile are kept and written on release.
pub struct SaveHold {
    io: ChunkIo,
    id: u64,
    released: bool,
}

impl SaveHold {
    /// Lifts the hold and waits until what it held back is written.
    pub async fn release(mut self) {
        self.released = true;
        self.io.release_hold(self.id).await;
    }

    /// Whether the hold still keeps storage untouched (false once its
    /// limit has passed).
    pub fn holding(&self) -> bool {
        self.io.saves().hold.is_some()
    }
}

impl std::fmt::Debug for SaveHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveHold").field("id", &self.id).finish()
    }
}

impl Drop for SaveHold {
    fn drop(&mut self) {
        if !self.released {
            self.io.release_hold_soon(self.id);
        }
    }
}

/// Stops automatic saving, saves and flushes the whole world, then closes
/// the gate: the files on disk are a consistent snapshot until the
/// returned hold is released. `owner` names the job in logs and errors.
pub async fn hold_saves(
    shared: &Arc<Shared>,
    owner: &str,
    limit: Duration,
) -> Result<SaveHold, HoldError> {
    let io = shared.chunk_io.get(&shared.backend).clone();
    let id = io.begin_hold(owner, limit).await.map_err(HoldError::Busy)?;
    let mut hold = SaveHold {
        io,
        id,
        released: false,
    };
    let (done, flushed) = oneshot::channel();
    let asked = shared
        .coordinator
        .send(CoordinatorMessage::SaveAll { flush: true, done })
        .await
        .is_ok();
    if !asked || !matches!(flushed.await, Ok(Ok(()))) {
        hold.released = true;
        hold.io.release_hold(id).await;
        return Err(HoldError::FlushFailed);
    }
    if !hold.io.close_hold(id).await {
        hold.released = true;
        return Err(HoldError::Lapsed);
    }
    Ok(hold)
}
