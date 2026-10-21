//! Data-driven registries as the enabled data packs leave them at startup.
//!
//! Every registry starts from the built-in pack's entries. A pack file that
//! adds or changes an entry makes that entry `Loaded`; a file with the same
//! bytes as the built-in one is skipped, so the entry stays `Builtin` and a
//! registry no pack touched shares the vanilla snapshot. Entries are kept in
//! the order the game sends them: sorted by path, then namespace.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use tuff_vanilla_data::data_registry::{self, DataRegistry};

pub enum Entry<T: 'static> {
    Builtin(&'static T),
    Loaded(Arc<T>),
}

impl<T> Entry<T> {
    pub fn value(&self) -> &T {
        match self {
            Self::Builtin(value) => value,
            Self::Loaded(value) => value,
        }
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self, Self::Builtin(_))
    }
}

impl<T> Clone for Entry<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Builtin(value) => Self::Builtin(value),
            Self::Loaded(value) => Self::Loaded(value.clone()),
        }
    }
}

/// One frozen registry: ids in network order, their entries, and whether
/// any entry differs from the built-in pack.
///
/// Built-in entries keep the index the generated tables give them, so
/// code that keys on generated indices stays valid: pack-added entries
/// follow all built-in ones, and a built-in entry a pack `filter` hid keeps
/// its slot (clients are still sent it) but is invisible to lookups.
pub struct RuntimeRegistry<T: 'static> {
    key: &'static str,
    ids: Vec<Arc<str>>,
    entries: Vec<Entry<T>>,
    hidden: Vec<bool>,
    index: HashMap<Arc<str>, u32>,
    modified: bool,
}
impl<T> RuntimeRegistry<T> {
    fn with_hidden(key: &'static str, mut items: Vec<(Arc<str>, Entry<T>, bool)>) -> Self {
        let builtin = DataRegistry::get(key);
        let added = |id: &str| builtin.is_none_or(|b| b.entries.binary_search(&id).is_err());
        items.sort_by(|(a, ..), (b, ..)| added(a).cmp(&added(b)).then_with(|| network_order(a, b)));
        let modified = items
            .iter()
            .any(|(_, entry, hidden)| *hidden || !entry.is_builtin())
            || builtin.is_some_and(|builtin| {
                builtin
                    .entries
                    .iter()
                    .any(|id| !items.iter().any(|(i, ..)| &**i == *id))
            });
        let index = items
            .iter()
            .enumerate()
            .filter(|(_, (.., hidden))| !hidden)
            .map(|(i, (id, ..))| (id.clone(), i as u32))
            .collect();
        let mut ids = Vec::with_capacity(items.len());
        let mut entries = Vec::with_capacity(items.len());
        let mut hidden = Vec::with_capacity(items.len());
        for (id, entry, is_hidden) in items {
            ids.push(id);
            entries.push(entry);
            hidden.push(is_hidden);
        }
        Self {
            key,
            ids,
            entries,
            hidden,
            index,
            modified,
        }
    }

    pub fn key(&self) -> &'static str {
        self.key
    }

    /// The number of network slots, hidden entries included.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Whether a pack added, changed or hid any entry.
    pub fn modified(&self) -> bool {
        self.modified
    }

    pub fn index_of(&self, id: &str) -> Option<u32> {
        self.index.get(id).copied()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    pub fn id(&self, index: u32) -> Option<&str> {
        let i = index as usize;
        (!*self.hidden.get(i)?).then(|| &*self.ids[i])
    }

    pub fn entry(&self, id: &str) -> Option<&Entry<T>> {
        self.index_of(id).map(|i| &self.entries[i as usize])
    }

    pub fn get(&self, id: &str) -> Option<&T> {
        self.entry(id).map(Entry::value)
    }

    /// The visible entries, in network order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Entry<T>)> {
        self.network_iter()
            .filter(|(.., hidden)| !hidden)
            .map(|(id, entry, _)| (id, entry))
    }

    /// Every network slot in order, with whether a pack hid it.
    pub fn network_iter(&self) -> impl Iterator<Item = (&str, &Entry<T>, bool)> {
        self.ids
            .iter()
            .map(|id| &**id)
            .zip(&self.entries)
            .zip(&self.hidden)
            .map(|((id, entry), hidden)| (id, entry, *hidden))
    }
}

/// The order the game keeps registry entries in: by path, then namespace.
fn network_order(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |id: &str| -> (String, String) {
        match id.split_once(':') {
            Some((namespace, path)) => (path.to_owned(), namespace.to_owned()),
            None => (id.to_owned(), "minecraft".to_owned()),
        }
    };
    split(a).cmp(&split(b))
}

/// What a registry keeps for an entry while it has no codec: the file a
/// pack provided, if any.
#[derive(Debug, Default)]
pub struct RawEntry {
    pub pack: Option<Arc<str>>,
    pub json: Option<Arc<serde_json::Value>>,
    /// The network NBT clients get for a pack entry of a synchronized
    /// registry; unset when the registry has no network codec yet.
    pub network: OnceLock<Vec<u8>>,
}

static BUILTIN_RAW: RawEntry = RawEntry {
    pack: None,
    json: None,
    network: OnceLock::new(),
};

/// Context a codec decodes one entry in.
pub struct DecodeCtx<'a> {
    pub registries: &'a Registries,
    pub id: &'a str,
}

/// Turns a registry's JSON entries into typed values. Built-in entries come
/// from the generated tables, so only pack-provided ones are decoded.
pub trait RegistryCodec {
    const KEY: &'static str;
    type Value: Send + Sync + 'static;

    fn builtin(id: &str) -> Option<&'static Self::Value>;

    fn decode(json: &serde_json::Value, ctx: &DecodeCtx<'_>) -> Result<Self::Value, String>;
}

/// A codec whose values can be sent to clients in Registry Data.
pub trait NetworkCodec: RegistryCodec {
    fn to_network_nbt(value: &Self::Value) -> Result<Vec<u8>, String>;
}

type NetworkEncoder = fn(&serde_json::Value, &DecodeCtx<'_>) -> Result<Vec<u8>, String>;

fn encode_network<C: NetworkCodec>(
    json: &serde_json::Value,
    ctx: &DecodeCtx<'_>,
) -> Result<Vec<u8>, String> {
    C::to_network_nbt(&C::decode(json, ctx)?)
}

/// The network encoder of each synchronized registry that has one.
fn network_encoder(key: &str) -> Option<NetworkEncoder> {
    Some(match key {
        codecs::DamageTypes::KEY => encode_network::<codecs::DamageTypes>,
        codecs::PaintingVariants::KEY => encode_network::<codecs::PaintingVariants>,
        "minecraft:worldgen/biome" => codecs::biome_network_nbt,
        "minecraft:dimension_type" => codecs::dimension_type_network_nbt,
        "minecraft:timeline" => codecs::timeline_network_nbt,
        "minecraft:test_instance" => codecs::test_instance_network_nbt,
        crate::enchantment::EnchantmentCodec::KEY => {
            |json, _| crate::enchantment::network_nbt(json)
        }
        _ => return None,
    })
}

/// A registry file that failed validation; the whole load is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError {
    pub pack: String,
    pub registry: &'static str,
    pub id: String,
    pub message: String,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} entry {} from pack {}: {}",
            self.registry, self.id, self.pack, self.message
        )
    }
}

/// Every data-driven registry, frozen. Immutable once built.
pub struct Registries {
    raw: Vec<Arc<RuntimeRegistry<RawEntry>>>,
    trades: OnceLock<Arc<trades::Trades>>,
}

tokio::task_local! {
    static CURRENT: Arc<Registries>;
}

/// Runs `future` with `registries` as the ones [`current`] returns.
pub fn scope<F: std::future::Future>(
    registries: Arc<Registries>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    CURRENT.scope(registries, future)
}

/// Runs `f` with `registries` as the ones [`current`] returns, outside a task.
pub fn sync_scope<R>(registries: Arc<Registries>, f: impl FnOnce() -> R) -> R {
    CURRENT.sync_scope(registries, f)
}

/// A `'static` copy of a registry entry id, kept once per distinct id (ids
/// come from the frozen registries, so the set stays small).
pub fn intern(id: &str) -> &'static str {
    static IDS: OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> =
        OnceLock::new();
    let mut ids = IDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = ids.get(id) {
        return found;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    ids.insert(leaked);
    leaked
}

/// The registries of the enclosing [`scope`], else the built-in ones.
pub fn current() -> Arc<Registries> {
    CURRENT
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Registries::vanilla())
}

impl Registries {
    /// The built-in pack's registries, shared.
    pub fn vanilla() -> Arc<Registries> {
        static VANILLA: OnceLock<Arc<Registries>> = OnceLock::new();
        VANILLA
            .get_or_init(|| {
                Arc::new(Registries {
                    raw: data_registry::ALL
                        .iter()
                        .map(|registry| Arc::new(builtin_registry(registry, Vec::new(), None)))
                        .collect(),
                    trades: OnceLock::new(),
                })
            })
            .clone()
    }

    /// The villager trade and trade set registries.
    pub fn trades(&self) -> &trades::Trades {
        self.trades.get_or_init(|| {
            Arc::new(
                self.decode_trades()
                    .expect("trade registries are validated when frozen"),
            )
        })
    }

    fn decode_trades(&self) -> Result<trades::Trades, RegistryError> {
        Ok(trades::Trades {
            trades: self.decode::<trades::TradeCodec>()?,
            sets: self.decode::<trades::TradeSetCodec>()?,
        })
    }

    pub fn get(&self, key: &str) -> Option<&RuntimeRegistry<RawEntry>> {
        let position = data_registry::ALL.iter().position(|r| r.key == key)?;
        Some(&self.raw[position])
    }

    pub fn iter(&self) -> impl Iterator<Item = &RuntimeRegistry<RawEntry>> {
        self.raw.iter().map(|registry| &**registry)
    }

    /// Whether a pack added or changed any entry of any registry.
    pub fn modified(&self) -> bool {
        self.raw.iter().any(|registry| registry.modified)
    }

    /// Whether `id` is an entry of the data-driven registry `key`; `None`
    /// when `key` is not one.
    pub fn exists(&self, key: &str, id: &str) -> Option<bool> {
        self.get(key).map(|registry| registry.contains(id))
    }

    /// The typed registry `C` describes: built-in entries from the generated
    /// tables, pack entries decoded.
    pub fn decode<C: RegistryCodec>(&self) -> Result<RuntimeRegistry<C::Value>, RegistryError> {
        let raw = self.get(C::KEY).ok_or_else(|| RegistryError {
            pack: String::new(),
            registry: "",
            id: String::new(),
            message: format!("unknown registry {}", C::KEY),
        })?;
        let mut items = Vec::with_capacity(raw.len());
        for (id, entry, hidden) in raw.network_iter() {
            let error = |message: String| RegistryError {
                pack: entry
                    .value()
                    .pack
                    .as_deref()
                    .unwrap_or("vanilla")
                    .to_owned(),
                registry: raw.key,
                id: id.to_owned(),
                message,
            };
            let typed = match (entry, entry.value().json.as_deref()) {
                (Entry::Loaded(_), Some(json)) => Entry::Loaded(Arc::new(
                    C::decode(
                        json,
                        &DecodeCtx {
                            registries: self,
                            id,
                        },
                    )
                    .map_err(error)?,
                )),
                _ => Entry::Builtin(
                    C::builtin(id).ok_or_else(|| error("no built-in value".to_owned()))?,
                ),
            };
            items.push((Arc::<str>::from(id), typed, hidden));
        }
        Ok(RuntimeRegistry::with_hidden(raw.key, items))
    }
}

fn builtin_registry(
    registry: &'static DataRegistry,
    loaded: Vec<(Arc<str>, Entry<RawEntry>)>,
    hidden: Option<&std::collections::BTreeSet<String>>,
) -> RuntimeRegistry<RawEntry> {
    let replaced: std::collections::HashSet<&str> = loaded.iter().map(|(id, _)| &**id).collect();
    let mut items: Vec<(Arc<str>, Entry<RawEntry>, bool)> = registry
        .entries
        .iter()
        .filter(|id| !replaced.contains(**id))
        .map(|id| {
            let hide = hidden.is_some_and(|h| h.contains(*id));
            (Arc::<str>::from(*id), Entry::Builtin(&BUILTIN_RAW), hide)
        })
        .collect();
    items.extend(loaded.into_iter().map(|(id, entry)| (id, entry, false)));
    RuntimeRegistry::with_hidden(registry.key, items)
}

/// The data-driven registry a resource path below a namespace belongs to
/// (`enchantment/x.json`), with the entry path (`x`).
/// Registries data packs reload with `/reload`, which the data pack loader
/// reads itself.
const RELOADABLE: &[&str] = &[
    "minecraft:advancement",
    "minecraft:item_modifier",
    "minecraft:loot_table",
    "minecraft:predicate",
    "minecraft:recipe",
];

pub fn classify(path: &str) -> Option<(&'static DataRegistry, &str)> {
    let json = path.strip_suffix(".json")?;
    data_registry::ALL
        .iter()
        .filter(|registry| !RELOADABLE.contains(&registry.key))
        .filter_map(|registry| {
            let directory = registry.key.strip_prefix("minecraft:")?;
            let entry = json.strip_prefix(directory)?.strip_prefix('/')?;
            (!entry.is_empty()).then_some((registry, entry))
        })
        .max_by_key(|(registry, _)| registry.key.len())
}

/// The first 8 bytes of the SHA-1 of `bytes`, big-endian, as the built-in
/// pack's hashes are recorded.
fn short_sha1(bytes: &[u8]) -> u64 {
    use sha1::Digest;
    let digest = sha1::Sha1::digest(bytes);
    u64::from_be_bytes(digest[..8].try_into().expect("20-byte digest"))
}

/// Checks frozen registries a feature interprets; an error rejects the load.
pub type Validator = fn(&Registries) -> Result<(), RegistryError>;

static VALIDATORS: std::sync::RwLock<BTreeMap<&'static str, Validator>> =
    std::sync::RwLock::new(BTreeMap::new());

/// Registers `validator` under `name` (a later call with the same name
/// replaces it); every freeze that loads pack entries runs it.
pub fn add_validator(name: &'static str, validator: Validator) {
    VALIDATORS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(name, validator);
}

/// A pack's file for one entry: the pack id and the file's bytes.
type PackFile = (Arc<str>, Vec<u8>);

/// Worldgen registries the vanilla generator decodes when it binds.
const WORLDGEN_INTERPRETED: &[&str] = &[
    "minecraft:worldgen/noise",
    "minecraft:worldgen/density_function",
    "minecraft:worldgen/material_rule",
    "minecraft:worldgen/material_condition",
    "minecraft:worldgen/noise_settings",
    "minecraft:worldgen/feature",
    "minecraft:worldgen/placed_feature",
    "minecraft:worldgen/structure",
    "minecraft:worldgen/structure_set",
    "minecraft:worldgen/template_pool",
    "minecraft:worldgen/processor_list",
    "minecraft:worldgen/world_preset",
    "minecraft:worldgen/multi_noise_biome_source_parameter_list",
    "minecraft:dimension",
    "minecraft:enchantment_provider",
];

/// Registry files from the enabled packs, lowest priority first.
#[derive(Default)]
pub struct Loader {
    hidden: BTreeMap<&'static str, std::collections::BTreeSet<String>>,
    files: BTreeMap<&'static str, BTreeMap<String, PackFile>>,
    registries: HashMap<&'static str, &'static DataRegistry>,
}

impl Loader {
    /// Records `pack`'s file for entry `id` of `registry`, replacing what a
    /// lower pack provided. A file identical to the built-in pack's drops
    /// the override, so the entry stays built-in.
    pub fn insert(
        &mut self,
        registry: &'static DataRegistry,
        id: String,
        pack: &str,
        bytes: Vec<u8>,
    ) {
        self.registries.insert(registry.key, registry);
        let files = self.files.entry(registry.key).or_default();
        let builtin = registry
            .entries
            .binary_search(&id.as_str())
            .ok()
            .and_then(|i| registry.hashes.get(i));
        if builtin.is_some_and(|hash| *hash == short_sha1(&bytes)) {
            files.remove(&id);
        } else {
            files.insert(id, (Arc::from(pack), bytes));
        }
    }

    /// Removes built-in entry `id` of `registry` (a pack `filter` hid it)
    /// unless a pack above provides its own file for it.
    pub fn hide(&mut self, registry: &'static DataRegistry, id: &str) {
        self.registries.insert(registry.key, registry);
        self.hidden
            .entry(registry.key)
            .or_default()
            .insert(id.to_owned());
    }

    /// Parses every pack entry and freezes the registries. Any entry that
    /// fails rejects the whole set.
    pub fn freeze(self) -> Result<Arc<Registries>, RegistryError> {
        let vanilla = Registries::vanilla();
        if self.files.values().all(BTreeMap::is_empty)
            && self
                .hidden
                .values()
                .all(std::collections::BTreeSet::is_empty)
        {
            return Ok(vanilla);
        }
        let mut raw = Vec::with_capacity(data_registry::ALL.len());
        let mut uninterpreted = Vec::new();
        for (registry, shared) in data_registry::ALL.iter().zip(&vanilla.raw) {
            let hidden = self.hidden.get(registry.key);
            let files = self.files.get(registry.key).filter(|f| !f.is_empty());
            if files.is_none() && hidden.is_none_or(|h| h.is_empty()) {
                raw.push(shared.clone());
                continue;
            }
            let empty = BTreeMap::new();
            let files = files.unwrap_or(&empty);
            let mut loaded = Vec::with_capacity(files.len());
            for (id, (pack, bytes)) in files {
                let error = |message: String| RegistryError {
                    pack: pack.to_string(),
                    registry: registry.key,
                    id: id.clone(),
                    message,
                };
                let json: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|err| error(err.to_string()))?;
                if registry.list_entries() {
                    if !json.is_array() {
                        return Err(error("not a JSON list".to_owned()));
                    }
                } else if !json.is_object() {
                    return Err(error("not a JSON object".to_owned()));
                }
                loaded.push((
                    Arc::<str>::from(id.as_str()),
                    Entry::Loaded(Arc::new(RawEntry {
                        pack: Some(pack.clone()),
                        json: Some(Arc::new(json)),
                        network: OnceLock::new(),
                    })),
                ));
            }
            if !registry.synchronized
                && ![trades::TradeCodec::KEY, trades::TradeSetCodec::KEY]
                    .iter()
                    .chain(WORLDGEN_INTERPRETED)
                    .any(|key| *key == registry.key)
            {
                uninterpreted.push(format!("{} ({})", registry.key, loaded.len()));
            }
            raw.push(Arc::new(builtin_registry(registry, loaded, hidden)));
        }
        if !uninterpreted.is_empty() {
            log::warn!(
                "Data pack registry entries are not interpreted yet, only their ids are kept: {}",
                uninterpreted.join(", ")
            );
        }
        let registries = Registries {
            raw,
            trades: OnceLock::new(),
        };
        self.encode_network(&registries)?;
        if [trades::TradeCodec::KEY, trades::TradeSetCodec::KEY]
            .iter()
            .any(|key| registries.get(key).is_some_and(RuntimeRegistry::modified))
        {
            let _ = registries.trades.set(Arc::new(registries.decode_trades()?));
        }
        let validators: Vec<Validator> = VALIDATORS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .copied()
            .collect();
        for validate in validators {
            validate(&registries)?;
        }
        Ok(Arc::new(registries))
    }

    /// Encodes every pack entry of a synchronized registry for clients.
    /// An entry missing a field every built-in entry has rejects the set.
    fn encode_network(&self, registries: &Registries) -> Result<(), RegistryError> {
        for (key, registry) in &self.registries {
            if !registry.synchronized {
                continue;
            }
            let Some(frozen) = registries.get(key) else {
                continue;
            };
            if *key == "minecraft:worldgen/biome"
                && frozen.len() > tuff_world_model::biomes::max_biomes()
                && let Some((id, _)) = frozen.iter().last()
            {
                return Err(RegistryError {
                    pack: "data packs".to_owned(),
                    registry: registry.key,
                    id: id.to_owned(),
                    message: format!(
                        "at most {} biomes fit in chunk packets",
                        tuff_world_model::biomes::max_biomes()
                    ),
                });
            }
            let encoder = network_encoder(key);
            for (id, entry) in frozen.iter() {
                let Entry::Loaded(raw) = entry else {
                    continue;
                };
                let error = |message: String| RegistryError {
                    pack: raw.pack.as_deref().unwrap_or("vanilla").to_owned(),
                    registry: registry.key,
                    id: id.to_owned(),
                    message,
                };
                let Some(json) = raw.json.as_deref() else {
                    continue;
                };
                let ctx = DecodeCtx { registries, id };
                let nbt = match encoder {
                    Some(encoder) => encoder(json, &ctx),
                    None => codecs::generic_network_nbt(key, json),
                }
                .map_err(error)?;
                let _ = raw.network.set(nbt);
            }
        }
        Ok(())
    }
}

pub mod codecs;
pub mod trades;

#[cfg(test)]
mod tests;
