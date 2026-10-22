//! `/save-all` for test worlds: one world save through the save journal,
//! as the coordinator takes it.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tuff_core::world::messages::ManagerMessage;
use tuff_storage::journal::SaveJournal;

pub(crate) fn save_all(
    regions: mpsc::Sender<ManagerMessage>,
    journal: Option<&Arc<SaveJournal>>,
    flush: bool,
    done: oneshot::Sender<Result<(), Arc<str>>>,
) {
    let barrier = journal.map(|journal| journal.barrier());
    tokio::spawn(async move {
        let (saved, flushed) = oneshot::channel();
        let (snapshotted, taken) = oneshot::channel();
        let asked = regions
            .send(ManagerMessage::SaveAll {
                done: flush.then_some(saved),
                snapshotted: Some(snapshotted),
                epoch: barrier.as_ref().map(|barrier| barrier.handle()),
            })
            .await
            .is_ok();
        if asked {
            let _ = taken.await;
        }
        if let Some(barrier) = barrier {
            barrier.seal();
        }
        if flush {
            let _ = flushed.await;
        }
        let _ = done.send(if asked { Ok(()) } else { Err("stopped".into()) });
    });
}
