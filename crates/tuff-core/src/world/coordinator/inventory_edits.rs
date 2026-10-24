use super::*;
use crate::storage::player_data;
use crate::world::inventory_edit::InventoryEdit;
use crate::world::messages::SessionTask;

impl Coordinator {
    /// Applies `edit` to the newest session of `uuid`, or to its stored
    /// data when it has none. An offline edit holds the uuid's player lease:
    /// it waits for the last session's save, and a login waits for the
    /// edit's save.
    pub(super) fn edit_inventory(
        &mut self,
        uuid: u128,
        edit: InventoryEdit,
        reply: oneshot::Sender<bool>,
    ) {
        let online = self
            .players
            .iter()
            .filter(|(_, p)| p.profile.id == uuid)
            .max_by_key(|(id, _)| **id)
            .map(|(_, p)| p.handle.clone());
        if let Some(handle) = online {
            let saved = self.sessions.get(&uuid).cloned();
            let (taken, outcome) = oneshot::channel();
            let task_edit = edit.clone();
            handle.send(PlayerEvent::Run(SessionTask::new(move |session| {
                Box::pin(async move {
                    if session.disconnected {
                        return;
                    }
                    let applied = task_edit.apply(&mut session.inventory);
                    if applied {
                        session.send_inventory();
                    }
                    let _ = taken.send(applied);
                })
            })));
            let coordinator = self.shared.coordinator.clone();
            tokio::spawn(async move {
                if let Ok(applied) = outcome.await {
                    let _ = reply.send(applied);
                    return;
                }
                // The session left without running the edit; once its
                // logout save is done (and its Left queued ahead of us),
                // the edit is routed again, to stored data or a new login.
                if let Some(mut saved) = saved {
                    let _ = saved.wait_for(|saved| *saved).await;
                }
                let _ = coordinator
                    .send(CoordinatorMessage::EditInventory { uuid, edit, reply })
                    .await;
            });
            return;
        }
        let lease = self.leases.request(uuid);
        let backend = self.shared.backend.clone();
        tokio::spawn(async move {
            let lease = lease.granted().await;
            let access = lease.access();
            let applied = tokio::task::spawn_blocking(move || {
                let docs = backend.documents();
                let Ok(Some(mut raw)) = access.read(|| player_data::load(docs, uuid)) else {
                    return false;
                };
                if !player_data::edit_inventory(&mut raw, |inventory| edit.apply(inventory)) {
                    return false;
                }
                let mut saved = Ok(());
                access.write(|| saved = player_data::save(docs, uuid, raw));
                match saved {
                    Ok(()) => true,
                    Err(err) => {
                        log::error!("failed to save an offline inventory edit: {err}");
                        false
                    }
                }
            })
            .await
            .unwrap_or(false);
            lease.release();
            let _ = reply.send(applied);
        });
    }
}
