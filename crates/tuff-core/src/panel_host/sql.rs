//! The core side of the SQL console: the world backend's console and
//! the loaded chunks a write must not touch.

use std::sync::Arc;

use crate::world::Shared;
use crate::world::messages::{ManagerMessage, RegionRequest};
use tuff_storage::sqlite::console::{ChunkKey, SqlConsole};
use tuff_web_ui::host::HostFuture;
use tuff_web_ui::sql::SqlHost;

pub async fn loaded_among(shared: &Shared, touched: &[ChunkKey]) -> Vec<ChunkKey> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    if shared
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let chunks: Arc<[ChunkKey]> = touched.into();
    let mut loaded = Vec::new();
    for region in answer.await.unwrap_or_default() {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let asked = region
            .request(RegionRequest::LoadedAmong {
                chunks: chunks.clone(),
                reply,
            })
            .await;
        if asked && let Ok(mut found) = answer.await {
            loaded.append(&mut found);
        }
    }
    loaded.sort();
    loaded.dedup();
    loaded
}

impl SqlHost for Shared {
    fn console(&self) -> Option<SqlConsole> {
        self.backend.sql_console()
    }

    fn backend_name(&self) -> String {
        self.backend.name().to_owned()
    }

    fn loaded_among<'a>(&'a self, touched: &'a [ChunkKey]) -> HostFuture<'a, Vec<ChunkKey>> {
        Box::pin(loaded_among(self, touched))
    }
}
