//! The generated resource pack that carries every feature's custom content
//! assets.
//!
//! [`merge`] folds the registered [`AssetSet`]s into one file table: a path
//! provided by two sets is a conflict naming both, except for JSON object
//! files clients read as merged tables (`sounds.json`, `lang`, `atlases`,
//! `font`, `blockstates`), which are merged key by key. [`build_pack`] zips
//! the table deterministically, so equal inputs give the same SHA-1.
//!
//! One actor, [`CustomPack`], owns the current sets. It registers the pack
//! with [`ResourcePacks`] under a stable id, re-merges when a feature
//! replaces a set at runtime (keeping the previous pack when the new one
//! conflicts), lets the resource pack actor re-push the new hash to every
//! holder, and applies the prompt setting.
//!
//! The pack is required: a client that declines or fails to load it, at
//! join or on a re-push, is disconnected, so every player in play has the
//! current version applied.

pub mod zip;

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use md5::{Digest, Md5};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::{Change, ConfigStore};
use crate::custom::{AssetSet, is_asset_path, is_identifier};
use crate::resource_pack::{PackSource, ResourcePack, ResourcePacks};
use tuff_item::text::TextComponent;

pub const PACK_NAME: &str = "tuff:custom_content";
pub const DESCRIPTION: &str = "Server custom content";
pub const PROMPT_KEY: &str = "tuff.customContent.prompt";

/// The generated pack's UUID: a name-based (version 3) UUID of
/// [`PACK_NAME`], so a re-push replaces the pack on the client in place.
pub fn pack_id() -> u128 {
    static ID: OnceLock<u128> = OnceLock::new();
    *ID.get_or_init(|| {
        let mut hash: [u8; 16] = Md5::digest(PACK_NAME.as_bytes()).into();
        hash[6] = (hash[6] & 0x0f) | 0x30;
        hash[8] = (hash[8] & 0x3f) | 0x80;
        u128::from_be_bytes(hash)
    })
}

/// One registered asset set and who owns it.
#[derive(Debug, Clone)]
pub struct AssetEntry {
    pub key: String,
    pub feature: &'static str,
    pub set: Arc<AssetSet>,
}

/// Two sets that provide the same file, or the same key of a merged file,
/// differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetConflict {
    pub path: String,
    /// Set key and feature of the earlier provider.
    pub first: (String, &'static str),
    pub second: (String, &'static str),
    pub detail: String,
}

impl std::fmt::Display for AssetConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} (asset set {} of feature {} and asset set {} of feature {})",
            self.path, self.detail, self.first.0, self.first.1, self.second.0, self.second.1
        )
    }
}

/// Whether clients read this file as a table several packs contribute to,
/// so features can share it.
pub fn is_mergeable(path: &str) -> bool {
    let Some((_, rest)) = path.strip_prefix("assets/").and_then(|p| p.split_once('/')) else {
        return false;
    };
    rest == "sounds.json"
        || (rest.ends_with(".json")
            && ["lang/", "atlases/", "font/", "blockstates/"]
                .iter()
                .any(|dir| rest.starts_with(dir)))
}

/// Folds `entries` (in order) into one table keyed by full pack path.
pub fn merge(entries: &[AssetEntry]) -> Result<BTreeMap<String, Bytes>, Box<AssetConflict>> {
    let mut providers: BTreeMap<String, Vec<(usize, &Bytes)>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        for (path, bytes) in &entry.set.files {
            let full = format!("assets/{}/{path}", entry.set.namespace);
            providers.entry(full).or_default().push((index, bytes));
        }
    }
    let owner = |index: usize| (entries[index].key.clone(), entries[index].feature);
    let mut files = BTreeMap::new();
    for (path, list) in providers {
        let [(_, only)] = list.as_slice() else {
            let conflict = |a: usize, b: usize, detail: String| {
                Box::new(AssetConflict {
                    path: path.clone(),
                    first: owner(a),
                    second: owner(b),
                    detail,
                })
            };
            if !is_mergeable(&path) {
                return Err(conflict(list[0].0, list[1].0, "provided twice".to_owned()));
            }
            let mut parsed = Vec::with_capacity(list.len());
            for &(index, bytes) in &list {
                match serde_json::from_slice::<Value>(bytes) {
                    Ok(value @ Value::Object(_)) => parsed.push((index, value)),
                    _ => {
                        let other = list
                            .iter()
                            .find(|(i, _)| *i != index)
                            .map_or(index, |p| p.0);
                        return Err(conflict(
                            other,
                            index,
                            "shared file is not a JSON object".to_owned(),
                        ));
                    }
                }
            }
            let mut merged = parsed[0].1.clone();
            for (i, (index, value)) in parsed.iter().enumerate().skip(1) {
                let mut at = String::new();
                if let Err(pointer) = merge_value(&mut merged, value, &mut at) {
                    let first = parsed[..i]
                        .iter()
                        .find(|(_, v)| v.pointer(&pointer).is_some())
                        .map_or(parsed[0].0, |(index, _)| *index);
                    return Err(conflict(first, *index, format!("differs at {pointer}")));
                }
            }
            let bytes = serde_json::to_vec(&merged).expect("a json value serialises");
            files.insert(path, Bytes::from(bytes));
            continue;
        };
        files.insert(path, (*only).clone());
    }
    Ok(files)
}

/// Objects merge per key, arrays concatenate, equal values stay; anything
/// else is a conflict at the returned JSON pointer.
fn merge_value(into: &mut Value, from: &Value, at: &mut String) -> Result<(), String> {
    if into == from {
        return Ok(());
    }
    match (into, from) {
        (Value::Object(into), Value::Object(from)) => {
            for (key, value) in from {
                let len = at.len();
                at.push('/');
                at.push_str(&key.replace('~', "~0").replace('/', "~1"));
                match into.get_mut(key) {
                    Some(existing) => merge_value(existing, value, at)?,
                    None => {
                        into.insert(key.clone(), value.clone());
                    }
                }
                at.truncate(len);
            }
            Ok(())
        }
        (Value::Array(into), Value::Array(from)) => {
            into.extend(from.iter().cloned());
            Ok(())
        }
        _ => Err(at.clone()),
    }
}

/// The pack's `pack.mcmeta`, declaring exactly the running game's resource
/// pack format.
pub fn pack_mcmeta() -> Vec<u8> {
    let version = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
    let format = serde_json::json!([version.major, version.minor]);
    serde_json::to_vec(&serde_json::json!({
        "pack": {
            "description": DESCRIPTION,
            "min_format": format,
            "max_format": format,
        }
    }))
    .expect("static json serialises")
}

/// The zipped pack: `pack.mcmeta` plus every file of the table, sorted by
/// path, with fixed timestamps.
pub fn build_pack(files: &BTreeMap<String, Bytes>) -> io::Result<Bytes> {
    let meta = pack_mcmeta();
    let mut entries: Vec<(&str, &[u8])> = Vec::with_capacity(files.len() + 1);
    entries.extend(files.iter().map(|(p, b)| (p.as_str(), b.as_ref())));
    entries.push(("pack.mcmeta", &meta));
    entries.sort_by(|a, b| a.0.cmp(b.0));
    zip::write(entries, crate::resource_pack::MAX_PACK_BYTES)
}

fn validate(set: &AssetSet) -> Result<(), String> {
    if set.namespace.is_empty() || !is_identifier(&format!("{}:x", set.namespace)) {
        return Err(format!("namespace {:?} is invalid", set.namespace));
    }
    if let Some((path, _)) = set.files.iter().find(|(path, _)| !is_asset_path(path)) {
        return Err(format!("path {path:?} is invalid"));
    }
    Ok(())
}

/// Why a runtime asset change was refused; the previous pack stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Invalid(String),
    /// The set key belongs to another feature.
    NotOwner {
        key: String,
        owner: &'static str,
    },
    Conflict(Box<AssetConflict>),
    Stopped,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "invalid asset set: {why}"),
            Self::NotOwner { key, owner } => {
                write!(f, "asset set {key} belongs to feature {owner}")
            }
            Self::Conflict(conflict) => write!(f, "asset conflict: {conflict}"),
            Self::Stopped => f.write_str("custom pack actor is not running"),
        }
    }
}

impl std::error::Error for UpdateError {}

enum Request {
    #[cfg(test)]
    Panic,
    Set {
        feature: &'static str,
        key: String,
        set: Option<AssetSet>,
        reply: oneshot::Sender<Result<bool, UpdateError>>,
    },
    Files(oneshot::Sender<Arc<BTreeMap<String, Bytes>>>),
}

/// Handle to the actor owning the generated pack.
#[derive(Clone)]
pub struct CustomPack {
    tx: mpsc::Sender<Request>,
}

impl std::fmt::Debug for CustomPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CustomPack")
    }
}

impl Default for CustomPack {
    fn default() -> Self {
        Self::detached()
    }
}

impl CustomPack {
    /// A handle whose every request fails with [`UpdateError::Stopped`].
    pub fn detached() -> Self {
        Self {
            tx: mpsc::channel(1).0,
        }
    }

    /// Starts the actor with the startup sets and registers the pack when
    /// there is anything to serve. Returns once the pack is registered, so
    /// the first join already gets it offered.
    pub async fn start(
        packs: ResourcePacks,
        config: ConfigStore,
        initial: Vec<AssetEntry>,
    ) -> Self {
        // Buffered: requests come from feature code at runtime and each one
        // is handled in turn; senders wait when the actor falls behind.
        let (tx, rx) = mpsc::channel(32);
        let changes = config.subscribe();
        let files = match merge(&initial) {
            Ok(files) => files,
            Err(conflict) => {
                log::warn!("custom content assets conflict: {conflict}");
                BTreeMap::new()
            }
        };
        let mut actor = Actor {
            rx,
            packs,
            config,
            changes,
            sets: initial.into_iter().map(|e| (e.key.clone(), e)).collect(),
            files: Arc::new(files),
            published: false,
            prompt: None,
        };
        actor.prompt = actor.read_prompt();
        actor.publish().await;
        tuff_supervise::spawn(
            "the custom content pack",
            tuff_supervise::restarting("the custom content pack", actor, |actor| {
                Box::pin(actor.run())
            }),
        );
        Self { tx }
    }

    /// Adds or replaces the asset set `key` owned by `feature` and rolls the
    /// rebuilt pack out to every client holding it. `Ok(false)` means the
    /// merged pack did not change.
    pub async fn set_assets(
        &self,
        feature: &'static str,
        key: impl Into<String>,
        set: AssetSet,
    ) -> Result<bool, UpdateError> {
        self.request(feature, key.into(), Some(set)).await
    }

    /// Removes the asset set `key`; the pack is unhosted once it is empty.
    pub async fn remove_assets(
        &self,
        feature: &'static str,
        key: impl Into<String>,
    ) -> Result<bool, UpdateError> {
        self.request(feature, key.into(), None).await
    }

    async fn request(
        &self,
        feature: &'static str,
        key: String,
        set: Option<AssetSet>,
    ) -> Result<bool, UpdateError> {
        let (reply, done) = oneshot::channel();
        self.tx
            .send(Request::Set {
                feature,
                key,
                set,
                reply,
            })
            .await
            .map_err(|_| UpdateError::Stopped)?;
        done.await.map_err(|_| UpdateError::Stopped)?
    }

    #[cfg(test)]
    async fn panic_for_test(&self) {
        let _ = self.tx.send(Request::Panic).await;
    }

    /// The merged file table currently served (without `pack.mcmeta`).
    pub async fn files(&self) -> Arc<BTreeMap<String, Bytes>> {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Request::Files(reply)).await.is_err() {
            return Arc::default();
        }
        done.await.unwrap_or_default()
    }
}

struct Actor {
    rx: mpsc::Receiver<Request>,
    packs: ResourcePacks,
    config: ConfigStore,
    changes: watch::Receiver<Option<Change>>,
    sets: BTreeMap<String, AssetEntry>,
    files: Arc<BTreeMap<String, Bytes>>,
    published: bool,
    prompt: Option<TextComponent>,
}

impl Actor {
    async fn run(&mut self) {
        let mut watching = true;
        loop {
            tokio::select! {
                request = self.rx.recv() => match request {
                    Some(request) => self.handle(request).await,
                    None => return,
                },
                changed = self.changes.changed(), if watching => {
                    if changed.is_err() {
                        watching = false;
                        continue;
                    }
                    self.changes.borrow_and_update();
                    self.apply_prompt().await;
                }
            }
        }
    }

    fn read_prompt(&self) -> Option<TextComponent> {
        self.config
            .get_string(PROMPT_KEY)
            .filter(|p| !p.is_empty())
            .map(TextComponent::new)
    }

    async fn apply_prompt(&mut self) {
        let prompt = self.read_prompt();
        if prompt == self.prompt {
            return;
        }
        self.prompt = prompt;
        if self.published {
            self.packs
                .configure(pack_id(), true, self.prompt.clone())
                .await;
        }
    }

    fn source(&self) -> PackSource {
        let files = self.files.clone();
        PackSource::Generated(Arc::new(move || build_pack(&files)))
    }

    fn pack(&self) -> ResourcePack {
        let mut pack = ResourcePack::new(pack_id(), PACK_NAME, self.source())
            .required(true)
            .essential(true)
            .order(crate::resource_pack::CUSTOM_CONTENT_ORDER);
        pack.prompt = self.prompt.clone();
        pack
    }

    /// Makes the hosted pack match `files`.
    async fn publish(&mut self) {
        match (self.published, self.files.is_empty()) {
            (false, true) => {}
            (false, false) => {
                self.packs.register(self.pack()).await;
                self.published = true;
            }
            (true, true) => {
                self.packs.remove(pack_id()).await;
                self.published = false;
            }
            (true, false) => {
                self.packs.update(pack_id(), self.source()).await;
            }
        }
    }

    async fn handle(&mut self, request: Request) {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Files(reply) => {
                let _ = reply.send(self.files.clone());
            }
            Request::Set {
                feature,
                key,
                set,
                reply,
            } => {
                let result = self.change(feature, key, set).await;
                let _ = reply.send(result);
            }
        }
    }

    async fn change(
        &mut self,
        feature: &'static str,
        key: String,
        set: Option<AssetSet>,
    ) -> Result<bool, UpdateError> {
        if let Some(existing) = self.sets.get(&key)
            && existing.feature != feature
        {
            return Err(UpdateError::NotOwner {
                key,
                owner: existing.feature,
            });
        }
        let mut sets = self.sets.clone();
        match set {
            Some(set) => {
                validate(&set).map_err(UpdateError::Invalid)?;
                sets.insert(
                    key.clone(),
                    AssetEntry {
                        key,
                        feature,
                        set: Arc::new(set),
                    },
                );
            }
            None => {
                if sets.remove(&key).is_none() {
                    return Ok(false);
                }
            }
        }
        let entries: Vec<AssetEntry> = sets.values().cloned().collect();
        let files = merge(&entries).map_err(UpdateError::Conflict)?;
        self.sets = sets;
        if *self.files == files {
            return Ok(false);
        }
        let first = !self.published;
        self.files = Arc::new(files);
        self.publish().await;
        if first && self.published {
            self.packs.push(None, pack_id()).await;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
