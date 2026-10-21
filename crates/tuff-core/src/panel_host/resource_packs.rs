//! The core side of the Resource packs page: the pack actor, and the packs
//! operators upload, kept in the server file area as
//! `resourcepacks/<uuid>.zip` beside a `<uuid>.json` record (written last,
//! so a zip without a record is an interrupted upload and is deleted at the
//! next start) and hosted again at every start.
//!
//! - `GET /resourcepacks`: every hosted pack (feature, custom content and
//!   uploaded; only uploaded ones are deletable or editable) and each
//!   online player's status per pack (`resourcepacks.read`).
//! - `GET /resourcepacks/<uuid>`: the pack's bytes (`resourcepacks.read`).
//! - `PUT /resourcepacks/upload?filename=<file>&name=<name>&push=1`: a zip
//!   with a `Content-Length`, checked completely with the strict zip
//!   reader (`resourcepacks.manage`).
//! - `POST /resourcepacks/<uuid>/settings` with any of `{"name",
//!   "required", "prompt", "offerOnJoin", "order"}` (`resourcepacks.manage`).
//! - `POST /resourcepacks/<uuid>/push` and `/pop` with an optional
//!   `{"player": <uuid or online name>}`, else everyone online
//!   (`resourcepacks.manage`).
//! - `DELETE /resourcepacks/<uuid>` unhosts and deletes an uploaded pack
//!   (`resourcepacks.manage`).
//!
//! Composition: join offers go out by ascending `order` (then name), and a
//! client stacks later packs above earlier ones. Uploaded packs take an
//! order in [`ORDERS`]; feature packs default to 0; the custom content pack
//! is always last ([`CUSTOM_CONTENT_ORDER`]), so no uploaded pack can paint
//! over custom content at join. The custom content pack is essential: it
//! cannot be deleted, popped or reconfigured here. A pack pushed during play
//! lands on top of what the client holds until it rejoins.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::blocking;
use crate::datapack::PackMeta;
use crate::datapack::meta::{Compatibility, Format};
use crate::protocol::pack::PackStatus;
use crate::resource_pack::{
    BuildState, CUSTOM_CONTENT_ORDER, ENABLED_KEY, HostedPack, MAX_PACK_BYTES, ORDERS,
    PackSettings, PackSource, ROUTE_PREFIX, ResourcePack, format_uuid, parse_uuid,
};
use crate::world::Shared;
use crate::world::messages::CoordinatorMessage;
use tuff_item::text::TextComponent;
use tuff_storage::admin::parse_uuid_text;
use tuff_storage::config::ConfigStore;
use tuff_web_ui::host::{HostError, HostFuture};
use tuff_web_ui::resource_packs::ResourcePackHost;
use tuff_zip::{Archive, Limits};

/// Where uploaded packs live in the server file area.
pub const DIRECTORY: &str = "resourcepacks";
/// The most an uploaded pack may unpack to while it is checked.
pub const MAX_UNPACKED: u64 = 4 * MAX_PACK_BYTES as u64;
const MAX_ENTRIES: usize = 100_000;
/// Deflate's best ratio, so only lying sizes trip it.
const MAX_RATIO: u64 = 1032;
const MAX_META: usize = 1 << 20;
const MAX_RECORD: usize = 64 * 1024;
const MAX_NAME: usize = 64;
const MAX_PROMPT: usize = 512;
const METADATA: &str = "pack.mcmeta";

/// An uploaded pack's saved settings and provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub name: String,
    pub required: bool,
    #[serde(default)]
    pub prompt: Option<String>,
    pub offer_on_join: bool,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub uploaded_unix: u64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub description: String,
    /// The declared format range, `[[major, minor], [major, minor]]`.
    #[serde(default)]
    pub formats: Option<[[i32; 2]; 2]>,
}

fn to_pairs(formats: Option<(Format, Format)>) -> Option<[[i32; 2]; 2]> {
    formats.map(|(min, max)| [[min.major, min.minor], [max.major, max.minor]])
}

fn from_pairs(pairs: [[i32; 2]; 2]) -> (Format, Format) {
    let format = |[major, minor]: [i32; 2]| Format { major, minor };
    (format(pairs[0]), format(pairs[1]))
}

impl Record {
    fn settings(&self) -> PackSettings {
        PackSettings {
            required: self.required,
            prompt: self.prompt.clone().map(TextComponent::new),
            offer_on_join: self.offer_on_join,
            order: self.order,
        }
    }

    fn pack(&self, id: u128, source: PackSource) -> ResourcePack {
        let mut pack = ResourcePack::new(id, &self.name, source)
            .required(self.required)
            .offer_on_join(self.offer_on_join)
            .order(self.order.clamp(*ORDERS.start(), *ORDERS.end()));
        pack.prompt = self.prompt.clone().map(TextComponent::new);
        pack
    }
}

fn zip_path(id: u128) -> String {
    format!("{DIRECTORY}/{}.zip", format_uuid(id))
}

fn record_path(id: u128) -> String {
    format!("{DIRECTORY}/{}.json", format_uuid(id))
}

fn id_of(path: &str, suffix: &str) -> Option<u128> {
    path.strip_prefix(DIRECTORY)?
        .strip_prefix('/')?
        .strip_suffix(suffix)
        .and_then(parse_uuid)
}

/// Every uploaded pack's record, deleting zips left without one. Records
/// that cannot be read are logged and skipped. Blocking.
pub fn load_records(files: &dyn ConfigStore) -> io::Result<Vec<(u128, Record)>> {
    let paths = match files.list_files(DIRECTORY) {
        Ok(paths) => paths,
        Err(err) if err.kind() == io::ErrorKind::Unsupported => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let zips: HashSet<u128> = paths.iter().filter_map(|p| id_of(p, ".zip")).collect();
    let listed: HashSet<u128> = paths.iter().filter_map(|p| id_of(p, ".json")).collect();
    let mut records = Vec::new();
    for path in &paths {
        let Some(id) = id_of(path, ".json") else {
            continue;
        };
        if !zips.contains(&id) {
            log::warn!("Resource pack record {path} has no pack; ignoring it");
            continue;
        }
        let record = files
            .read_file(path, MAX_RECORD)
            .and_then(|bytes| bytes.ok_or_else(|| io::ErrorKind::NotFound.into()))
            .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).map_err(io::Error::other));
        match record {
            Ok(record) => records.push((id, record)),
            Err(err) => log::warn!("Could not read resource pack record {path}: {err}"),
        }
    }
    for id in zips.difference(&listed) {
        log::info!(
            "Deleting interrupted resource pack upload {}",
            zip_path(*id)
        );
        files.remove_file(&zip_path(*id))?;
    }
    records.sort_by(|a, b| (a.1.order, &a.1.name).cmp(&(b.1.order, &b.1.name)));
    Ok(records)
}

fn stored_source(files: Arc<dyn ConfigStore>, id: u128) -> PackSource {
    PackSource::Generated(Arc::new(move || {
        files
            .read_file(&zip_path(id), MAX_PACK_BYTES)?
            .map(Bytes::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the stored pack is gone"))
    }))
}

/// Hosts every uploaded pack again; called once at startup, after the
/// features' packs are registered. A record whose id is taken is skipped.
pub async fn restore(shared: &Shared) {
    let Some(files) = shared.web.files.clone() else {
        return;
    };
    let reader = files.clone();
    let records = match blocking(move || load_records(&*reader)).await {
        Some(Ok(records)) => records,
        Some(Err(err)) => {
            log::warn!("Could not list uploaded resource packs: {err}");
            return;
        }
        None => return,
    };
    if records.is_empty() {
        return;
    }
    let taken: HashSet<u128> = shared
        .resource_packs
        .list()
        .await
        .iter()
        .map(|p| p.id)
        .collect();
    for (id, record) in records {
        if taken.contains(&id) {
            log::warn!(
                "Uploaded resource pack {} shares id {} with a feature's pack; not hosting it",
                record.name,
                format_uuid(id)
            );
            continue;
        }
        let pack = record.pack(id, stored_source(files.clone(), id));
        shared.resource_packs.register(pack).await;
    }
}

/// What a checked upload declares.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspected {
    pub description: String,
    pub formats: Option<(Format, Format)>,
    pub compatibility: Compatibility,
    pub files: usize,
}

fn current_format() -> Format {
    let version = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
    Format {
        major: version.major,
        minor: version.minor,
    }
}

/// Checks an uploaded pack completely: a strict archive (no traversal,
/// links, duplicates or overlapping entries) whose every entry inflates to
/// its declared size and checksum within [`MAX_UNPACKED`], with a readable
/// `pack.mcmeta` at its root. Blocking.
pub fn inspect(bytes: &[u8]) -> Result<Inspected, String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("the pack is over {MAX_PACK_BYTES} bytes"));
    }
    let archive = Archive::open(
        bytes,
        Limits {
            max_entries: MAX_ENTRIES,
            max_entry_bytes: MAX_UNPACKED,
            max_total_bytes: MAX_UNPACKED,
            max_ratio: Some(MAX_RATIO),
            strict: true,
        },
    )
    .map_err(|err| format!("not a usable zip archive: {err}"))?;
    let Some(meta) = archive.find(METADATA).filter(|e| !e.is_dir()) else {
        let wrapped = archive
            .entries()
            .iter()
            .find_map(|e| e.name.strip_suffix("/pack.mcmeta"))
            .filter(|top| !top.contains('/'));
        return Err(match wrapped {
            Some(top) => format!(
                "no {METADATA} at the pack root; the pack is inside {top}/, so zip that folder's contents instead"
            ),
            None => format!("no {METADATA} at the pack root"),
        });
    };
    if meta.size > MAX_META as u64 {
        return Err(format!("{METADATA} is over {MAX_META} bytes"));
    }
    let meta = archive
        .read(METADATA, MAX_META)
        .map_err(|err| format!("unreadable {METADATA}: {err}"))?;
    let meta = PackMeta::parse(&meta).map_err(|err| format!("unreadable {METADATA}: {err}"))?;
    let mut files = 0;
    for entry in archive.entries().iter().filter(|e| !e.is_dir()) {
        let mut reader = archive
            .reader(entry)
            .map_err(|err| format!("unreadable zip archive: {err}"))?;
        io::copy(&mut reader, &mut io::sink())
            .map_err(|err| format!("unreadable zip archive: {err}"))?;
        files += 1;
    }
    let known = meta.formats.0.major != i32::MAX;
    Ok(Inspected {
        description: meta.description.plain(),
        formats: known.then_some(meta.formats),
        compatibility: Compatibility::of(meta.formats, current_format()),
        files,
    })
}

fn compatibility_name(c: Compatibility) -> &'static str {
    match c {
        Compatibility::Compatible => "compatible",
        Compatibility::TooOld => "too_old",
        Compatibility::TooNew => "too_new",
        Compatibility::Unknown => "unknown",
    }
}

fn formats_json(formats: Option<(Format, Format)>) -> Value {
    match formats {
        Some((min, max)) => json!([[min.major, min.minor], [max.major, max.minor]]),
        None => Value::Null,
    }
}

/// A display name: printable, at most [`MAX_NAME`] characters.
pub fn clean_name(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect();
    let name = name.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

fn clean_prompt(prompt: &str) -> Option<String> {
    let prompt: String = prompt
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(MAX_PROMPT)
        .collect();
    (!prompt.trim().is_empty()).then_some(prompt)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn new_id() -> u128 {
    let mut bytes: [u8; 16] = rand::random::<u128>().to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    u128::from_be_bytes(bytes)
}

/// Why a resource pack request was refused.
#[derive(Debug)]
pub enum PackError {
    NoFileArea,
    Invalid(String),
    NotFound,
    /// The pack belongs to a feature or is essential.
    NotYours(&'static str),
    NotOnline(String),
    Unavailable(String),
    Io(io::Error),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFileArea => {
                f.write_str("this server has no file area to keep uploaded packs in")
            }
            Self::Invalid(why) | Self::Unavailable(why) => f.write_str(why),
            Self::NotFound => f.write_str("no such resource pack"),
            Self::NotYours(why) => f.write_str(why),
            Self::NotOnline(who) => write!(f, "{who} is not online"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl PackError {
    pub fn status(&self) -> StatusCode {
        match self {
            PackError::NoFileArea | PackError::NotYours(_) | PackError::Unavailable(_) => {
                StatusCode::CONFLICT
            }
            PackError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            PackError::NotFound | PackError::NotOnline(_) => StatusCode::NOT_FOUND,
            PackError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<PackError> for HostError {
    fn from(err: PackError) -> Self {
        let quiet = matches!(err, PackError::NotFound);
        HostError::new(err.status(), err.to_string()).quiet_if(quiet)
    }
}

/// What [`add`] stored.
#[derive(Debug, Clone)]
pub struct Added {
    pub id: u128,
    pub record: Record,
    pub inspected: Inspected,
}

/// Checks `bytes`, stores them with a new record and hosts the pack.
pub async fn add(
    shared: &Shared,
    name: &str,
    by: &str,
    bytes: Vec<u8>,
) -> Result<Added, PackError> {
    let files = shared.web.files.clone().ok_or(PackError::NoFileArea)?;
    let name =
        clean_name(name).ok_or_else(|| PackError::Invalid("the pack needs a name".into()))?;
    let bytes = Bytes::from(bytes);
    let checked = bytes.clone();
    let inspected = blocking(move || inspect(&checked))
        .await
        .ok_or_else(|| PackError::Invalid("the check failed".into()))?
        .map_err(PackError::Invalid)?;
    let taken: HashSet<u128> = shared
        .resource_packs
        .list()
        .await
        .iter()
        .map(|p| p.id)
        .collect();
    let id = std::iter::repeat_with(new_id)
        .find(|id| !taken.contains(id))
        .unwrap_or_else(new_id);
    let record = Record {
        name,
        required: false,
        prompt: None,
        offer_on_join: true,
        order: 0,
        by: by.to_owned(),
        uploaded_unix: now_unix(),
        size: bytes.len() as u64,
        description: inspected.description.clone(),
        formats: to_pairs(inspected.formats),
    };
    let stored = {
        let (bytes, record) = (bytes.clone(), record.clone());
        blocking(move || -> io::Result<()> {
            files.write_file(&zip_path(id), &bytes)?;
            let json = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
            if let Err(err) = files.write_file(&record_path(id), &json) {
                let _ = files.remove_file(&zip_path(id));
                return Err(err);
            }
            Ok(())
        })
        .await
    };
    match stored {
        Some(Ok(())) => {}
        Some(Err(err)) => return Err(PackError::Io(err)),
        None => return Err(PackError::Io(io::Error::other("the upload was not stored"))),
    }
    shared
        .resource_packs
        .register(record.pack(id, PackSource::Bytes(bytes)))
        .await;
    Ok(Added {
        id,
        record,
        inspected,
    })
}

async fn record_of(shared: &Shared, id: u128) -> Result<Record, PackError> {
    let files = shared.web.files.clone().ok_or(PackError::NotFound)?;
    let read = blocking(move || files.read_file(&record_path(id), MAX_RECORD)).await;
    match read {
        Some(Ok(Some(bytes))) => {
            serde_json::from_slice(&bytes).map_err(|err| PackError::Io(io::Error::other(err)))
        }
        Some(Ok(None)) => Err(PackError::NotFound),
        Some(Err(err)) => Err(PackError::Io(err)),
        None => Err(PackError::NotFound),
    }
}

async fn hosted(shared: &Shared, id: u128) -> Result<HostedPack, PackError> {
    shared
        .resource_packs
        .list()
        .await
        .into_iter()
        .find(|p| p.id == id)
        .ok_or(PackError::NotFound)
}

/// Unhosts the uploaded pack `id` (popping it from every holder) and
/// deletes its files.
pub async fn delete(shared: &Shared, id: u128) -> Result<Record, PackError> {
    let record = match record_of(shared, id).await {
        Err(PackError::NotFound) if hosted(shared, id).await.is_ok() => {
            return Err(PackError::NotYours("only uploaded packs can be deleted"));
        }
        other => other?,
    };
    shared.resource_packs.remove(id).await;
    let files = shared.web.files.clone().ok_or(PackError::NoFileArea)?;
    let removed = blocking(move || -> io::Result<()> {
        files.remove_file(&record_path(id))?;
        files.remove_file(&zip_path(id))
    })
    .await;
    match removed {
        Some(Ok(())) => Ok(record),
        Some(Err(err)) => Err(PackError::Io(err)),
        None => Err(PackError::Io(io::Error::other("the pack was not deleted"))),
    }
}

/// A settings change; absent fields stay as they are.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub name: Option<String>,
    pub required: Option<bool>,
    /// `Some(None)` clears the prompt.
    pub prompt: Option<Option<String>>,
    pub offer_on_join: Option<bool>,
    pub order: Option<i32>,
}

impl Change {
    fn from_json(body: &Value) -> Result<Self, PackError> {
        let bad = |field: &str| PackError::Invalid(format!("bad {field}"));
        let bool_field = |field: &str| match &body[field] {
            Value::Null => Ok(None),
            Value::Bool(b) => Ok(Some(*b)),
            _ => Err(bad(field)),
        };
        Ok(Self {
            name: match &body["name"] {
                Value::Null => None,
                Value::String(name) => Some(clean_name(name).ok_or_else(|| bad("name"))?),
                _ => return Err(bad("name")),
            },
            required: bool_field("required")?,
            prompt: match body.get("prompt") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::String(prompt)) => Some(clean_prompt(prompt)),
                Some(_) => return Err(bad("prompt")),
            },
            offer_on_join: bool_field("offerOnJoin")?,
            order: match &body["order"] {
                Value::Null => None,
                value => {
                    let order = value
                        .as_i64()
                        .and_then(|o| i32::try_from(o).ok())
                        .filter(|o| ORDERS.contains(o))
                        .ok_or_else(|| {
                            PackError::Invalid(format!(
                                "order must be a whole number from {} to {}",
                                ORDERS.start(),
                                ORDERS.end()
                            ))
                        })?;
                    Some(order)
                }
            },
        })
    }
}

/// Changes an uploaded pack's settings, saved and applied to offers from
/// now on.
pub async fn configure(shared: &Shared, id: u128, change: Change) -> Result<Record, PackError> {
    let mut record = match record_of(shared, id).await {
        Err(PackError::NotFound) if hosted(shared, id).await.is_ok() => {
            return Err(PackError::NotYours(
                "only uploaded packs can be changed here; a feature sets its own packs",
            ));
        }
        other => other?,
    };
    if let Some(name) = change.name {
        record.name = name;
    }
    if let Some(required) = change.required {
        record.required = required;
    }
    if let Some(prompt) = change.prompt {
        record.prompt = prompt;
    }
    if let Some(offer) = change.offer_on_join {
        record.offer_on_join = offer;
    }
    if let Some(order) = change.order {
        record.order = order;
    }
    let files = shared.web.files.clone().ok_or(PackError::NoFileArea)?;
    let saved = {
        let record = record.clone();
        blocking(move || -> io::Result<()> {
            let json = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
            files.write_file(&record_path(id), &json)
        })
        .await
    };
    match saved {
        Some(Ok(())) => {}
        Some(Err(err)) => return Err(PackError::Io(err)),
        None => {
            return Err(PackError::Io(io::Error::other(
                "the settings were not saved",
            )));
        }
    }
    if !shared
        .resource_packs
        .set_settings(id, Some(record.name.clone()), record.settings())
        .await
    {
        return Err(PackError::NotFound);
    }
    Ok(record)
}

async fn online(shared: &Shared) -> Vec<(u128, String)> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    if shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let mut players: Vec<(u128, String)> = answer
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.uuid, p.name))
        .collect();
    players.sort_by_key(|a| a.1.to_lowercase());
    players
}

/// The online player `who` names (uuid or name), or everyone for `None`.
async fn target(shared: &Shared, who: Option<&str>) -> Result<Option<u128>, PackError> {
    let Some(who) = who.filter(|w| !w.is_empty()) else {
        return Ok(None);
    };
    let players = online(shared).await;
    let uuid = parse_uuid_text(who);
    players
        .iter()
        .find(|(id, name)| Some(*id) == uuid || name.eq_ignore_ascii_case(who))
        .map(|(id, _)| Some(*id))
        .ok_or_else(|| PackError::NotOnline(who.to_owned()))
}

/// Offers pack `id` to one online player or everyone online.
pub async fn push(shared: &Shared, id: u128, who: Option<&str>) -> Result<(), PackError> {
    let pack = hosted(shared, id).await?;
    if pack.state == BuildState::Failed {
        return Err(PackError::Unavailable(format!(
            "pack {} failed to build",
            pack.name
        )));
    }
    if !pack.essential && !shared.config.get_bool(ENABLED_KEY).unwrap_or(true) {
        return Err(PackError::Unavailable(format!(
            "resource packs are switched off ({ENABLED_KEY})"
        )));
    }
    let player = target(shared, who).await?;
    shared.resource_packs.push(player, id).await;
    Ok(())
}

/// Removes pack `id` from one online player or everyone online.
pub async fn pop(shared: &Shared, id: u128, who: Option<&str>) -> Result<(), PackError> {
    let pack = hosted(shared, id).await?;
    if pack.essential {
        return Err(PackError::NotYours(
            "this pack is essential: every player must keep it",
        ));
    }
    let player = target(shared, who).await?;
    shared.resource_packs.pop(player, Some(id)).await;
    Ok(())
}

fn status_name(status: PackStatus) -> &'static str {
    match status {
        PackStatus::Loaded => "loaded",
        PackStatus::Declined => "declined",
        PackStatus::FailedDownload => "failed_download",
        PackStatus::Accepted => "accepted",
        PackStatus::Downloaded => "downloaded",
        PackStatus::InvalidUrl => "invalid_url",
        PackStatus::FailedReload => "failed_reload",
        PackStatus::Discarded => "discarded",
    }
}

/// The page's view: hosted packs and online players' statuses.
pub async fn state(shared: &Shared) -> Value {
    let packs = shared.resource_packs.list().await;
    let records: HashMap<u128, Record> = match shared.web.files.clone() {
        Some(files) => blocking(move || load_records(&*files))
            .await
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .collect(),
        None => HashMap::new(),
    };
    let custom = crate::custom_pack::pack_id();
    let hashes: HashMap<u128, String> = packs
        .iter()
        .filter_map(|p| match &p.state {
            BuildState::Ready { sha1, .. } => Some((p.id, sha1.clone())),
            _ => None,
        })
        .collect();
    let packs_json: Vec<Value> = packs
        .iter()
        .map(|p| {
            let record = records.get(&p.id);
            let kind = if p.id == custom {
                "custom"
            } else if record.is_some() {
                "uploaded"
            } else {
                "feature"
            };
            let (state, sha1, size) = match &p.state {
                BuildState::Building => ("building", Value::Null, Value::Null),
                BuildState::Ready { sha1, size } => ("ready", json!(sha1), json!(size)),
                BuildState::Failed => ("failed", Value::Null, Value::Null),
            };
            json!({
                "id": format_uuid(p.id),
                "name": p.name,
                "kind": kind,
                "required": p.settings.required,
                "prompt": p.settings.prompt.as_ref().map(TextComponent::plain),
                "offerOnJoin": p.settings.offer_on_join,
                "order": if p.settings.order == CUSTOM_CONTENT_ORDER { Value::Null } else { json!(p.settings.order) },
                "essential": p.essential,
                "state": state,
                "sha1": sha1,
                "size": size,
                "holders": p.holders,
                "editable": record.is_some(),
                "url": format!("{ROUTE_PREFIX}{}.zip", format_uuid(p.id)),
                "description": record.map(|r| r.description.clone()),
                "formats": record.map_or(Value::Null, |r| formats_json(r.formats.map(from_pairs))),
                "compatibility": record.map(|r| {
                    r.formats.map_or("unknown", |f| {
                        compatibility_name(Compatibility::of(from_pairs(f), current_format()))
                    })
                }),
                "uploadedBy": record.map(|r| r.by.clone()),
                "uploadedUnix": record.map(|r| r.uploaded_unix),
            })
        })
        .collect();
    let states = shared.resource_packs.player_states().await;
    let players: Vec<Value> = online(shared)
        .await
        .into_iter()
        .map(|(uuid, name)| {
            let mut packs: Vec<(String, Value)> = states
                .get(&uuid)
                .map(|states| {
                    states
                        .iter()
                        .map(|(id, state)| {
                            (
                                format_uuid(*id),
                                json!({
                                    "id": format_uuid(*id),
                                    "status": state.status.map(status_name),
                                    "loaded": state.loaded,
                                    "current": hashes.get(id) == Some(&state.hash),
                                }),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            packs.sort_by(|a, b| a.0.cmp(&b.0));
            json!({
                "uuid": format_uuid(uuid),
                "name": name,
                "packs": packs.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "packs": packs_json,
        "players": players,
        "enabled": shared.config.get_bool(ENABLED_KEY).unwrap_or(true),
        "maxUpload": MAX_PACK_BYTES,
        "fileArea": shared.web.files.is_some(),
        "orders": [ORDERS.start(), ORDERS.end()],
    })
}

fn record_json(id: u128, record: &Record) -> Value {
    json!({
        "id": format_uuid(id),
        "name": record.name,
        "required": record.required,
        "prompt": record.prompt,
        "offerOnJoin": record.offer_on_join,
        "order": record.order,
    })
}

impl ResourcePackHost for Shared {
    fn state(&self) -> HostFuture<'_, Value> {
        Box::pin(state(self))
    }

    fn has_file_area(&self) -> bool {
        self.web.files.is_some()
    }

    fn max_upload(&self) -> u64 {
        MAX_PACK_BYTES as u64
    }

    fn clean_name(&self, name: &str) -> Option<String> {
        clean_name(name)
    }

    fn add<'a>(
        &'a self,
        name: &'a str,
        by: &'a str,
        bytes: Vec<u8>,
    ) -> HostFuture<'a, Result<(u128, String, Value), HostError>> {
        Box::pin(async move {
            let added = add(self, name, by, bytes).await?;
            let body = json!({
                "pack": record_json(added.id, &added.record),
                "checked": {
                    "description": added.inspected.description,
                    "formats": formats_json(added.inspected.formats),
                    "compatibility": compatibility_name(added.inspected.compatibility),
                    "files": added.inspected.files,
                },
            });
            Ok((added.id, added.record.name, body))
        })
    }

    fn delete(&self, id: u128) -> HostFuture<'_, Result<String, HostError>> {
        Box::pin(async move { Ok(delete(self, id).await?.name) })
    }

    fn configure<'a>(
        &'a self,
        id: u128,
        body: &'a Value,
    ) -> HostFuture<'a, Result<Value, HostError>> {
        Box::pin(async move {
            let change =
                Change::from_json(body).map_err(|err| HostError::from(err).quiet_if(true))?;
            let record = configure(self, id, change).await?;
            Ok(json!({ "pack": record_json(id, &record) }))
        })
    }

    fn push<'a>(&'a self, id: u128, who: Option<&'a str>) -> HostFuture<'a, Result<(), HostError>> {
        Box::pin(async move { Ok(push(self, id, who).await?) })
    }

    fn pop<'a>(&'a self, id: u128, who: Option<&'a str>) -> HostFuture<'a, Result<(), HostError>> {
        Box::pin(async move { Ok(pop(self, id, who).await?) })
    }

    fn fetch(&self, id: u128) -> HostFuture<'_, Result<(String, Bytes), HostError>> {
        Box::pin(async move {
            let pack = hosted(self, id).await?;
            let Some(built) = self.resource_packs.fetch(id).await else {
                return Err(
                    PackError::Unavailable(format!("pack {} is not built", pack.name)).into(),
                );
            };
            Ok((pack.name, built.bytes.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_zip::{DosTime, write_all};

    fn meta() -> Vec<u8> {
        let v = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
        format!(
            r#"{{"pack":{{"description":"Shiny","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
            v.major, v.minor
        )
        .into_bytes()
    }

    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        write_all(files, DosTime::MIN, 6, u64::MAX).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn packs_are_checked_completely() {
        let meta = meta();
        let good = zip(&[
            ("pack.mcmeta", &meta),
            ("assets/minecraft/textures/block/stone.png", b"png"),
        ]);
        let checked = inspect(&good).unwrap();
        assert_eq!(checked.description, "Shiny");
        assert_eq!(checked.compatibility, Compatibility::Compatible);
        assert_eq!(checked.files, 2);

        assert!(inspect(b"PK nope").unwrap_err().contains("zip"));
        let none = zip(&[("assets/x.png", b"x")]);
        assert!(inspect(&none).unwrap_err().contains("no pack.mcmeta"));
        let wrapped = zip(&[("inner/pack.mcmeta", &meta)]);
        assert!(inspect(&wrapped).unwrap_err().contains("inside inner/"));
        let unreadable = zip(&[("pack.mcmeta", b"{")]);
        assert!(inspect(&unreadable).unwrap_err().contains("pack.mcmeta"));
        for name in ["../escape.png", "/abs.png", "a\\b.png", "C:/x.png"] {
            let bad = zip(&[("pack.mcmeta", &meta), (name, b"x")]);
            assert!(inspect(&bad).is_err(), "{name}");
        }
        let twice = zip(&[("pack.mcmeta", &meta), ("a.png", b"1"), ("A.png", b"2")]);
        assert!(inspect(&twice).is_err());

        let mut corrupt = zip(&[("pack.mcmeta", &meta), ("big.json", &[b'a'; 4096])]);
        let at = corrupt
            .windows(4)
            .rposition(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        corrupt[at + 16] ^= 0xff;
        assert!(inspect(&corrupt).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn lying_sizes_are_refused() {
        let meta = meta();
        let mut liar = zip(&[("pack.mcmeta", &meta), ("big.json", &vec![b'a'; 1 << 16])]);
        let at = liar
            .windows(4)
            .rposition(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        liar[at + 24..at + 28].copy_from_slice(&16u32.to_le_bytes());
        assert!(inspect(&liar).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn settings_changes_are_validated() {
        let change =
            Change::from_json(&json!({"required": true, "order": -5, "prompt": null})).unwrap();
        assert_eq!(change.required, Some(true));
        assert_eq!(change.order, Some(-5));
        assert_eq!(change.prompt, Some(None));
        assert!(change.name.is_none());
        assert!(Change::from_json(&json!({"order": 5000})).is_err());
        assert!(Change::from_json(&json!({"required": "yes"})).is_err());
        assert!(Change::from_json(&json!({"name": "\u{7}"})).is_err());
        assert_eq!(clean_name("  My pack\u{0}  ").as_deref(), Some("My pack"));
        assert_eq!(clean_name(&"x".repeat(100)).unwrap().len(), MAX_NAME);
    }

    #[tuff_core::watchdog]
    #[test]
    fn records_round_trip_and_orphans_are_dropped() {
        let dir = tuff_storage::test_dir::TestDir::new();
        let files: Arc<dyn ConfigStore> = Arc::new(
            tuff_storage::vanilla::config::VanillaConfig::new(dir.path()),
        );
        let record = Record {
            name: "Shiny".into(),
            required: true,
            prompt: Some("please".into()),
            offer_on_join: false,
            order: 3,
            by: "Console".into(),
            uploaded_unix: 1,
            size: 3,
            description: "d".into(),
            formats: to_pairs(Some((current_format(), current_format()))),
        };
        let id = new_id();
        files.write_file(&zip_path(id), b"zip").unwrap();
        files
            .write_file(&record_path(id), &serde_json::to_vec(&record).unwrap())
            .unwrap();
        let orphan = new_id();
        files.write_file(&zip_path(orphan), b"zip").unwrap();
        let loaded = load_records(&*files).unwrap();
        assert_eq!(loaded, vec![(id, record.clone())]);
        assert!(files.read_file(&zip_path(orphan), 16).unwrap().is_none());
        assert!(files.read_file(&zip_path(id), 16).unwrap().is_some());
        let pack = record.pack(id, PackSource::Bytes(Bytes::new()));
        assert!(pack.required && !pack.offer_on_join);
        assert_eq!(pack.order, 3);
        assert_eq!(pack.prompt.unwrap().plain(), "please");
        assert_eq!(new_id() >> 76 & 0xf, 4);
    }
}
