//! The core side of the Data packs page: pack changes the way
//! `/datapack` and `/reload` make them, over the world's pack store.

use std::path::PathBuf;

use hyper::StatusCode;
use serde_json::{Value, json};

use crate::datapack::store::{self, Kind, StoreError, Upload};
use crate::datapack::{
    Change, Compatibility, DataPacks, MAX_ARCHIVE, PackInfo, PackMeta, PackSource, Status,
};
use crate::world::Shared;
use tuff_storage::backup::BackupSource;
use tuff_web_ui::datapacks::DatapackHost;
use tuff_web_ui::host::{HostError, HostFuture};

use super::blocking;

/// Why a pack change was refused.
#[derive(Debug)]
pub enum PackError {
    Store(StoreError),
    Unknown(String),
    AlreadyEnabled(String),
    NotEnabled(String),
    /// The built-in pack, which is always enabled.
    Required(String),
    MissingFeatures(String, Vec<String>),
    /// The reload failed; nothing changed.
    Reload(String),
    Stopped,
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(err) => write!(f, "{err}"),
            Self::Unknown(id) => write!(f, "unknown data pack {id}"),
            Self::AlreadyEnabled(id) => write!(f, "pack {id} is already enabled"),
            Self::NotEnabled(id) => write!(f, "pack {id} is not enabled"),
            Self::Required(id) => write!(f, "pack {id} is always enabled"),
            Self::MissingFeatures(id, flags) => write!(
                f,
                "pack {id} needs feature flags this world lacks: {}",
                flags.join(", ")
            ),
            Self::Reload(why) => write!(f, "the reload failed: {why}"),
            Self::Stopped => f.write_str("the world has stopped"),
        }
    }
}

impl From<StoreError> for PackError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

impl PackError {
    pub fn status(&self) -> StatusCode {
        match self {
            PackError::Store(StoreError::Name(_) | StoreError::Invalid(_)) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            PackError::Store(StoreError::TooLarge(_)) => StatusCode::PAYLOAD_TOO_LARGE,
            PackError::Store(StoreError::Exists(_) | StoreError::Locked(_)) => StatusCode::CONFLICT,
            PackError::Store(StoreError::NotFound(_)) | PackError::Unknown(_) => {
                StatusCode::NOT_FOUND
            }
            PackError::Store(StoreError::Io(err)) => match err.kind() {
                std::io::ErrorKind::ResourceBusy => StatusCode::CONFLICT,
                std::io::ErrorKind::Unsupported => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            PackError::AlreadyEnabled(_)
            | PackError::NotEnabled(_)
            | PackError::Required(_)
            | PackError::MissingFeatures(..) => StatusCode::CONFLICT,
            PackError::Reload(_) => StatusCode::UNPROCESSABLE_ENTITY,
            PackError::Stopped => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl From<PackError> for HostError {
    fn from(err: PackError) -> Self {
        let quiet = matches!(
            err,
            PackError::Unknown(_) | PackError::Store(StoreError::NotFound(_))
        );
        HostError::new(err.status(), err.to_string()).quiet_if(quiet)
    }
}

async fn status(packs: &DataPacks) -> Result<Status, PackError> {
    packs.status(true).await.ok_or(PackError::Stopped)
}

async fn select(packs: &DataPacks, ids: Vec<String>) -> Result<Status, PackError> {
    packs
        .reload(Change::Select(ids))
        .await
        .map_err(PackError::Reload)
}

fn enableable<'s>(status: &'s Status, id: &str) -> Result<&'s PackInfo, PackError> {
    let pack = status
        .pack(id)
        .ok_or_else(|| PackError::Unknown(id.to_owned()))?;
    let missing = pack.missing_features();
    if !missing.is_empty() {
        return Err(PackError::MissingFeatures(
            id.to_owned(),
            missing.into_iter().map(str::to_owned).collect(),
        ));
    }
    Ok(pack)
}

/// Enables pack `id` above every other, as `/datapack enable` does.
pub async fn enable(shared: &Shared, id: &str) -> Result<Status, PackError> {
    let now = status(&shared.datapacks).await?;
    enableable(&now, id)?;
    if now.is_selected(id) {
        return Err(PackError::AlreadyEnabled(id.to_owned()));
    }
    let mut ids = now.selected.clone();
    ids.push(id.to_owned());
    select(&shared.datapacks, ids).await
}

/// Disables pack `id`, as `/datapack disable` does.
pub async fn disable(shared: &Shared, id: &str) -> Result<Status, PackError> {
    let now = status(&shared.datapacks).await?;
    let pack = now
        .pack(id)
        .ok_or_else(|| PackError::Unknown(id.to_owned()))?;
    if pack.required() {
        return Err(PackError::Required(id.to_owned()));
    }
    if !now.is_selected(id) {
        return Err(PackError::NotEnabled(id.to_owned()));
    }
    let ids = now.selected.iter().filter(|s| *s != id).cloned().collect();
    select(&shared.datapacks, ids).await
}

/// Reloads the selection plus every new pack not disabled, as `/reload`.
pub async fn reload(shared: &Shared) -> Result<Status, PackError> {
    shared
        .datapacks
        .reload(Change::Discover)
        .await
        .map_err(PackError::Reload)
}

/// What [`add`] did.
#[derive(Debug)]
pub struct Added {
    pub id: String,
    pub name: String,
    pub meta: PackMeta,
    pub replaced: bool,
    /// Whether the pack is enabled now.
    pub enabled: bool,
}

/// Stores the zip `bytes` as pack `name` and, when `enable`, enables it on
/// top. Replacing an enabled pack reloads it. A pack that stores but fails
/// to load stays stored and the error says why.
pub async fn add(
    shared: &Shared,
    name: &str,
    bytes: Vec<u8>,
    replace: bool,
    enable_it: bool,
) -> Result<Added, PackError> {
    let backend = shared.backend.clone();
    let name = name.to_owned();
    let added = blocking(move || store::add(&*backend, &name, &Upload::Zip(bytes), replace))
        .await
        .ok_or(PackError::Stopped)??;
    let id = added.id();
    let now = status(&shared.datapacks).await?;
    let selected = now.is_selected(&id);
    let enabled = if selected {
        if added.replaced {
            select(&shared.datapacks, now.selected.clone()).await?;
        }
        true
    } else if enable_it {
        enableable(&now, &id)?;
        let mut ids = now.selected.clone();
        ids.push(id.clone());
        select(&shared.datapacks, ids).await?;
        true
    } else {
        false
    };
    Ok(Added {
        id,
        name: added.name,
        meta: added.meta,
        replaced: added.replaced,
        enabled,
    })
}

/// Disables pack `name` if it is enabled, then deletes it from the world.
/// Nothing is deleted when the reload without it fails.
pub async fn remove(shared: &Shared, name: &str) -> Result<String, PackError> {
    let name = name.strip_prefix("file/").unwrap_or(name).to_owned();
    if !store::valid_name(&name) {
        return Err(StoreError::Name(format!("invalid pack name {name:?}")).into());
    }
    let id = format!("file/{name}");
    let now = status(&shared.datapacks).await?;
    if now.is_selected(&id) {
        let ids = now.selected.iter().filter(|s| **s != id).cloned().collect();
        select(&shared.datapacks, ids).await?;
    }
    let backend = shared.backend.clone();
    let removed = blocking(move || store::remove(&*backend, &name))
        .await
        .ok_or(PackError::Stopped)??;
    status(&shared.datapacks).await?;
    Ok(removed.id())
}

fn format_json(format: crate::datapack::meta::Format) -> Value {
    if format.major == i32::MAX {
        Value::Null
    } else {
        json!([format.major, format.minor])
    }
}

/// A pack's metadata for the page.
pub fn meta_json(meta: &PackMeta) -> Value {
    json!({
        "description": meta.description.plain(),
        "formats": [format_json(meta.formats.0), format_json(meta.formats.1)],
        "compatibility": match meta.compatibility() {
            Compatibility::Compatible => "compatible",
            Compatibility::TooOld => "too_old",
            Compatibility::TooNew => "too_new",
            Compatibility::Unknown => "unknown",
        },
        "features": meta.features,
        "missingFeatures": store::missing_features(meta),
    })
}

/// The page's view of the packs: what the actor knows, plus stored packs it
/// could not read.
pub fn listing(status: &Status, stored: &[store::Entry]) -> Value {
    let position = |id: &str| status.selected.iter().position(|s| s == id);
    let mut packs: Vec<Value> = status
        .available
        .iter()
        .map(|pack| {
            let entry = stored.iter().find(|e| e.id() == pack.id);
            json!({
                "id": pack.id,
                "name": entry.map_or(pack.id.as_str(), |e| e.name.as_str()),
                "title": pack.title.plain(),
                "source": match pack.source {
                    PackSource::BuiltIn => "builtin",
                    PackSource::Feature => "feature",
                    PackSource::World => "world",
                },
                "kind": entry.map(|e| kind_name(e.kind)),
                "required": pack.required(),
                "selected": position(&pack.id).is_some(),
                "position": position(&pack.id),
                "meta": meta_json(&pack.meta),
                "error": Value::Null,
            })
        })
        .collect();
    for entry in stored {
        let id = entry.id();
        if status.pack(&id).is_some() {
            continue;
        }
        let error = match &entry.meta {
            Err(why) if why.is_empty() => "the pack could not be read".to_owned(),
            Err(why) => why.clone(),
            Ok(_) => "not loaded yet; reload to pick it up".to_owned(),
        };
        packs.push(json!({
            "id": id,
            "name": entry.name,
            "title": entry.name,
            "source": "world",
            "kind": kind_name(entry.kind),
            "required": false,
            "selected": false,
            "position": Value::Null,
            "meta": entry.meta.as_ref().ok().map(meta_json),
            "error": error,
        }));
    }
    json!({
        "packs": packs,
        "selected": status.selected,
        "maxUpload": MAX_ARCHIVE,
    })
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Zip => "zip",
        Kind::Directory => "folder",
    }
}

/// Where the world's files live on disk, for the free space check.
pub(crate) fn world_dir(shared: &Shared) -> Option<PathBuf> {
    match shared.backend.backup_source() {
        BackupSource::Folder(root) => Some(root),
        BackupSource::File(file) => file
            .restorable()
            .and_then(|path| path.parent())
            .map(std::path::Path::to_path_buf),
        BackupSource::Unsupported => None,
    }
}

fn status_json(status: &Status) -> Value {
    json!({ "selected": status.selected })
}

async fn list(shared: &Shared) -> Result<Value, PackError> {
    let now = status(&shared.datapacks).await?;
    let backend = shared.backend.clone();
    let stored = blocking(move || store::list(&*backend))
        .await
        .ok_or(PackError::Stopped)??;
    Ok(listing(&now, &stored))
}

impl DatapackHost for Shared {
    fn listing(&self) -> HostFuture<'_, Result<Value, HostError>> {
        Box::pin(async move { Ok(list(self).await?) })
    }

    fn enable<'a>(&'a self, id: &'a str) -> HostFuture<'a, Result<Value, HostError>> {
        Box::pin(async move { Ok(status_json(&enable(self, id).await?)) })
    }

    fn disable<'a>(&'a self, id: &'a str) -> HostFuture<'a, Result<Value, HostError>> {
        Box::pin(async move { Ok(status_json(&disable(self, id).await?)) })
    }

    fn reload(&self) -> HostFuture<'_, Result<Value, HostError>> {
        Box::pin(async move { Ok(status_json(&reload(self).await?)) })
    }

    fn remove<'a>(&'a self, name: &'a str) -> HostFuture<'a, Result<String, HostError>> {
        Box::pin(async move { Ok(remove(self, name).await?) })
    }

    fn archive<'a>(
        &'a self,
        name: &'a str,
    ) -> HostFuture<'a, Result<(String, Vec<u8>), HostError>> {
        Box::pin(async move {
            let backend = self.backend.clone();
            let wanted = name.to_owned();
            let found = blocking(move || store::archive(&*backend, &wanted))
                .await
                .ok_or(PackError::Stopped)?;
            Ok(found.map_err(PackError::from)?)
        })
    }

    fn add<'a>(
        &'a self,
        name: &'a str,
        bytes: Vec<u8>,
        replace: bool,
        enable: bool,
    ) -> HostFuture<'a, Result<(String, Value), HostError>> {
        Box::pin(async move {
            let added = add(self, name, bytes, replace, enable).await?;
            let body = json!({
                "id": added.id,
                "name": added.name,
                "replaced": added.replaced,
                "enabled": added.enabled,
                "meta": meta_json(&added.meta),
            });
            Ok((added.id, body))
        })
    }

    fn suggested_name(&self, file: &str) -> Option<String> {
        store::suggested_name(std::path::Path::new(file))
    }

    fn max_upload(&self) -> u64 {
        MAX_ARCHIVE as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datapack::meta::Format;
    use tuff_item::text::TextComponent;

    fn meta(features: &[&str]) -> PackMeta {
        PackMeta {
            description: TextComponent::new("d"),
            formats: (Format::current(), Format::current()),
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            overlays: Vec::new(),
            filter: Vec::new(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn metadata_reports_formats_and_missing_features() {
        let value = meta_json(&meta(&["minecraft:vanilla", "minecraft:trade_rebalance"]));
        assert_eq!(value["compatibility"], "compatible");
        assert_eq!(
            value["missingFeatures"],
            json!(["minecraft:trade_rebalance"])
        );
        let current = Format::current();
        assert_eq!(value["formats"][0], json!([current.major, current.minor]));
        let mut unknown = meta(&[]);
        unknown.formats = (Format::UNKNOWN, Format::UNKNOWN);
        let value = meta_json(&unknown);
        assert_eq!(value["formats"], json!([null, null]));
        assert_eq!(value["compatibility"], "unknown");
    }

    #[tuff_core::watchdog]
    #[test]
    fn refusals_map_to_statuses() {
        let status = |err: PackError| err.status();
        assert_eq!(
            status(StoreError::Invalid("x".into()).into()),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            status(StoreError::TooLarge("x".into()).into()),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            status(StoreError::Exists("x".into()).into()),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(PackError::Required("vanilla".into())),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(PackError::Reload("pack file/x: bad".into())),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            status(StoreError::Io(std::io::ErrorKind::ResourceBusy.into()).into()),
            StatusCode::CONFLICT
        );
    }
}
