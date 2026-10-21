//! The core side of the Backups page: the backup jobs actor and the
//! world's restore target.

use std::path::PathBuf;

use hyper::StatusCode;
use serde_json::{Value, json};

use crate::config::{Key, Value as ConfigValue};
use crate::storage::backup::jobs::{BackupError, CreateOptions};
use crate::world::Shared;
use crate::world::messages::CoordinatorMessage;
use tuff_storage::backup::BackupSource;
use tuff_storage::backup::restore::Kind;
use tuff_web_ui::backups::{BackupHost, DEFAULT_MAX_UPLOAD_MIB, MAX_UPLOAD_KEY};
use tuff_web_ui::host::{HostError, HostFuture};

pub fn config_key() -> Key {
    Key::new(
        MAX_UPLOAD_KEY,
        ConfigValue::Int(DEFAULT_MAX_UPLOAD_MIB),
        "Largest backup the web panel accepts as an upload, in MiB; a restored backup may unpack to 4 times this",
    )
    .validated(|v| match v {
        ConfigValue::Int(n) if *n >= 1 => Ok(()),
        _ => Err("must be 1 or more".to_owned()),
    })
}

impl From<BackupError> for HostError {
    fn from(err: BackupError) -> Self {
        let status = match err {
            BackupError::Busy(_) | BackupError::NotRunning | BackupError::NoDirectory => {
                StatusCode::CONFLICT
            }
            BackupError::NotFound(_) => StatusCode::NOT_FOUND,
            BackupError::Stopped => StatusCode::SERVICE_UNAVAILABLE,
            BackupError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let quiet = matches!(err, BackupError::NotFound(_));
        HostError::new(status, err.to_string()).quiet_if(quiet)
    }
}

impl BackupHost for Shared {
    fn list(&self) -> HostFuture<'_, Result<Value, HostError>> {
        Box::pin(async move {
            match self.backups.list().await {
                Ok(list) => Ok(json!(list)),
                Err(BackupError::NoDirectory) => Ok(json!([])),
                Err(err) => Err(err.into()),
            }
        })
    }

    fn status(&self) -> HostFuture<'_, Value> {
        Box::pin(async move { json!(self.backups.status().await) })
    }

    fn level(&self) -> String {
        self.backups.level().to_owned()
    }

    fn world_target(&self) -> Option<(Kind, PathBuf)> {
        match self.backend.backup_source() {
            BackupSource::Folder(root) => Some((Kind::Folder, root)),
            BackupSource::File(file) => file
                .restorable()
                .map(|path| (Kind::ServerFile, path.to_path_buf())),
            BackupSource::Unsupported => None,
        }
    }

    fn create(
        &self,
        by: String,
        include_caches: bool,
    ) -> HostFuture<'_, Result<(String, Value), HostError>> {
        Box::pin(async move {
            let running = self
                .backups
                .create(CreateOptions::by(by).with_caches(include_caches))
                .await?;
            Ok((running.name.clone(), json!(running)))
        })
    }

    fn cancel(&self) -> HostFuture<'_, Result<String, HostError>> {
        Box::pin(async move { Ok(self.backups.cancel().await?) })
    }

    fn delete<'a>(&'a self, name: &'a str) -> HostFuture<'a, Result<(), HostError>> {
        Box::pin(async move { Ok(self.backups.delete(name).await?) })
    }

    fn path<'a>(&'a self, name: &'a str) -> HostFuture<'a, Result<PathBuf, HostError>> {
        Box::pin(async move { Ok(self.backups.path(name).await?) })
    }

    fn directory(&self) -> HostFuture<'_, Result<PathBuf, HostError>> {
        Box::pin(async move { Ok(self.backups.directory().await?) })
    }

    fn stop(&self) -> HostFuture<'_, bool> {
        Box::pin(async move {
            self.coordinator
                .send(CoordinatorMessage::Stop)
                .await
                .is_ok()
        })
    }
}
