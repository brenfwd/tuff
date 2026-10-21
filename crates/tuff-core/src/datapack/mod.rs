//! Data packs: the built-in `vanilla` pack (whose contents are compiled
//! in) and the world's own packs, folders and zip archives under
//! `datapacks/`, read through the world backend (files for a vanilla world,
//! `sqlar` entries for a sqlite one). Pack files are untrusted: reads are
//! capped per file, per archive and per reload.
//!
//! One actor owns the selection and the loaded contents. Readers take the
//! published [`Contents`] from a `watch` and never wait on the actor; only
//! listing, enabling, disabling and reloading go through it. A reload is
//! prepared on the blocking pool (compilation on rayon) and replaces the
//! contents whole, so a failed reload keeps the old ones.
//!
//! Loaded today: functions, tags of every registry this server knows
//! (merged over the built-in pack's, sent to clients), recipes (laid over
//! the features' recipes by name), loot tables (over the built-in ones,
//! with stored predicates inlined), stored predicates (for selectors and
//! `execute if predicate`) and advancements (over the features' tree by
//! name). Other data pack content (world generation, ...) is counted and
//! reported, not yet applied.

use tuff_datapack::{loot, recipe, tags, valid_namespace, valid_path, zip};

pub mod function;
mod hidden;
pub mod meta;
pub mod store;

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::sync::Arc;

use bytes::Bytes;
use rayon::prelude::*;
use tokio::sync::{mpsc, oneshot, watch};

use crate::command::Roots;
use crate::world::advancement::{Advancements, Overlay, OverlayFile};
use crate::world::crafting::{DataRecipes, RecipeSet};
use crate::world::drops::BlockLoot;
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;
use tuff_storage::backend::WorldBackend;

pub use function::Function;
use hidden::HiddenBuiltIn;
pub use meta::{Compatibility, PackMeta};

/// Where a world keeps its packs, relative to the backend's root.
pub const DIRECTORY: &str = "datapacks/";
/// The largest single file read from a pack.
pub const MAX_FILE: usize = 16 * 1024 * 1024;
/// The largest zip archive a pack may be.
pub const MAX_ARCHIVE: usize = 256 * 1024 * 1024;
/// The most entries a zip archive may list.
pub const MAX_ENTRIES: usize = 65_535;
/// The most bytes one reload reads out of every pack together.
pub const MAX_TOTAL: usize = 256 * 1024 * 1024;
/// The built-in pack, always enabled at the bottom.
pub const VANILLA: &str = "vanilla";
const METADATA: &str = "pack.mcmeta";
const FUNCTIONS: &str = "function/";
const FUNCTION_TAGS: &str = "tags/function/";
const TAGS: &str = "tags/";
const RECIPES: &str = "recipe/";
const LOOT_TABLES: &str = "loot_table/";
const ITEM_MODIFIERS: &str = "item_modifier/";
const SLOT_SOURCES: &str = "slot_source/";
const PREDICATES: &str = "predicate/";
const ADVANCEMENTS: &str = "advancement/";
const STRUCTURES: &str = "structure/";
const TEST_INSTANCES: &str = "test_instance/";
const TEST_ENVIRONMENTS: &str = "test_environment/";
const REQUEST_QUEUE: usize = 16;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSource {
    BuiltIn,
    /// A built-in pack that turns on experimental features; never enabled
    /// on its own.
    Feature,
    World,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    BuiltIn,
    /// A built-in feature pack, whose files are compiled in.
    Feature(&'static tuff_vanilla_data::version::FeaturePack),
    Directory(String),
    Zip(String),
}

/// A pack the server can enable.
#[derive(Debug, Clone)]
pub struct PackInfo {
    /// `vanilla`, or `file/<name>` for the world's packs.
    pub id: String,
    pub title: TextComponent,
    pub source: PackSource,
    pub meta: PackMeta,
    location: Location,
    /// The feature flags of the world it was found for.
    world_features: Arc<[String]>,
}

impl PackInfo {
    fn vanilla() -> Self {
        Self {
            id: VANILLA.to_owned(),
            title: TextComponent::translate("dataPack.vanilla.name", []),
            source: PackSource::BuiltIn,
            meta: PackMeta {
                description: TextComponent::translate("dataPack.vanilla.description", []),
                formats: (meta::Format::current(), meta::Format::current()),
                features: Vec::new(),
                overlays: Vec::new(),
                filter: Vec::new(),
            },
            location: Location::BuiltIn,
            world_features: default_features(),
        }
    }

    fn feature(pack: &'static tuff_vanilla_data::version::FeaturePack) -> Self {
        Self {
            id: pack.id.to_owned(),
            title: TextComponent::new(pack.id),
            source: PackSource::Feature,
            meta: PackMeta {
                description: TextComponent::translate(pack.description, []),
                formats: (meta::Format::current(), meta::Format::current()),
                features: pack.features.iter().map(|f| (*f).to_owned()).collect(),
                overlays: Vec::new(),
                filter: Vec::new(),
            },
            location: Location::Feature(pack),
            world_features: default_features(),
        }
    }

    /// The feature flags the pack needs that the world does not have.
    pub fn missing_features(&self) -> Vec<&str> {
        self.meta
            .features
            .iter()
            .map(String::as_str)
            .filter(|flag| !self.world_features.iter().any(|f| f == flag))
            .collect()
    }

    /// Whether the pack is a built-in feature pack, which turns on the
    /// world's flags and cannot be disabled once the world has them.
    pub fn provides_features(&self) -> bool {
        self.source == PackSource::Feature && !self.meta.features.is_empty()
    }

    /// Whether a newly found pack is enabled without being asked for.
    fn added_automatically(&self) -> bool {
        self.source != PackSource::Feature && self.missing_features().is_empty()
    }

    /// Whether the pack is always enabled, below every other.
    pub fn required(&self) -> bool {
        self.source == PackSource::BuiltIn
    }

    /// `[id (source)]`, green when enabled and red otherwise, with the
    /// title and description on hover.
    pub fn chat_link(&self, enabled: bool) -> TextComponent {
        let source = match self.source {
            PackSource::BuiltIn => "pack.source.builtin",
            PackSource::Feature => "pack.source.feature",
            PackSource::World => "pack.source.world",
        };
        let name = TextComponent::translate(
            "pack.nameAndSource",
            [
                TextComponent::new(self.id.clone()),
                TextComponent::translate(source, []),
            ],
        )
        .color("gray");
        TextComponent::translate("chat.square_brackets", [name])
            .color(if enabled { "green" } else { "red" })
            .insertion(quote_if_needed(&self.id))
            .hover_text(
                TextComponent::new("")
                    .append(self.title.clone())
                    .append("\n")
                    .append(self.meta.description.clone()),
            )
    }
}

/// `id` as a brigadier string argument would need it typed.
pub fn quote_if_needed(id: &str) -> String {
    if !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
    {
        id.to_owned()
    } else {
        format!("\"{}\"", id.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// The world's saved pack selection (`level.dat` `DataPacks`); a new
/// world's enables `vanilla`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPackConfig {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

impl Default for DataPackConfig {
    fn default() -> Self {
        Self {
            enabled: vec![VANILLA.to_owned()],
            disabled: Vec::new(),
        }
    }
}

/// What packs are laid over: the recipes features registered, how a data
/// recipe becomes one, the loot rules pack loot tables plug into and the
/// features' advancement tree (the built-in one when `None`), and the
/// level functions are compiled at (`function-permission-level`).
#[derive(Clone)]
pub struct Base {
    pub recipes: Arc<RecipeSet>,
    pub data_recipes: Option<Arc<dyn DataRecipes>>,
    pub block_loot: Option<Arc<dyn BlockLoot>>,
    pub advancements: Option<Arc<dyn Advancements>>,
    pub function_permission: PermissionLevel,
}

impl Default for Base {
    fn default() -> Self {
        Self {
            recipes: Arc::default(),
            data_recipes: None,
            block_loot: None,
            advancements: None,
            function_permission: function::PERMISSION,
        }
    }
}

impl Base {
    pub fn new(resolved: &crate::world::Resolved) -> Self {
        Self {
            recipes: resolved.recipe_set.clone(),
            data_recipes: resolved.data_recipes.clone(),
            block_loot: Some(resolved.block_loot.clone()),
            advancements: Some(resolved.advancements.clone()),
            function_permission: function::PERMISSION,
        }
    }
}

/// What the enabled packs provide.
pub struct Contents {
    /// Bumped by every successful load, so `#minecraft:load` runs once per
    /// reload.
    pub generation: u64,
    pub functions: BTreeMap<String, Arc<Function>>,
    pub function_tags: BTreeMap<String, Vec<String>>,
    /// Every registry's tags, the built-in pack's included.
    pub tags: Arc<tags::TagSet>,
    /// The Update Tags body for `tags`.
    pub tags_packet: Bytes,
    /// The server's recipes when packs changed any; `None` keeps the
    /// features' set.
    pub recipes: Option<Arc<RecipeSet>>,
    /// The loot tables in effect.
    pub loot_tables: Arc<loot::LootTables>,
    /// The features' loot rules over `loot_tables` when packs changed any;
    /// `None` keeps the features' own.
    pub block_loot: Option<Arc<dyn BlockLoot>>,
    /// The stored predicates, the built-in pack's included.
    pub predicates: Arc<loot::Predicates>,
    /// The stored item modifiers.
    pub item_modifiers: Arc<loot::ItemModifiers>,
    /// The stored slot sources.
    pub slot_sources: Arc<tuff_datapack::slot_source::SlotSources>,
    /// The advancement tree when packs changed any advancement or tag;
    /// `None` keeps the features' tree.
    pub advancements: Option<Arc<dyn Advancements>>,
    /// The structure template files (gzip NBT) packs provide, by id.
    pub structures: Arc<BTreeMap<String, Arc<[u8]>>>,
    /// The `test_instance` definitions (JSON) packs provide, by id.
    pub test_instances: Arc<BTreeMap<String, Arc<[u8]>>>,
    /// The `test_environment` definitions (JSON) packs provide, by id.
    pub test_environments: Arc<BTreeMap<String, Arc<[u8]>>>,
    /// The data-driven registries, fixed when the server starts.
    pub registries: Arc<tuff_item::registries::Registries>,
    /// The enchantments and providers decoded from `registries`.
    pub enchantments: Arc<tuff_item::enchantment::EnchantmentData>,
}

impl std::fmt::Debug for Contents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contents")
            .field("generation", &self.generation)
            .field("functions", &self.functions.len())
            .field("function_tags", &self.function_tags.len())
            .field("recipes", &self.recipes.is_some())
            .field("loot_tables", &self.loot_tables)
            .finish_non_exhaustive()
    }
}

impl Default for Contents {
    fn default() -> Self {
        let tags = tags::TagSet::vanilla();
        Self {
            generation: 0,
            functions: BTreeMap::new(),
            function_tags: BTreeMap::new(),
            tags_packet: vanilla_tags_packet(),
            tags,
            recipes: None,
            loot_tables: loot::LootTables::vanilla(),
            block_loot: None,
            predicates: loot::Predicates::vanilla(),
            item_modifiers: Arc::default(),
            slot_sources: Arc::default(),
            advancements: None,
            structures: Arc::default(),
            test_instances: Arc::default(),
            test_environments: Arc::default(),
            registries: tuff_item::registries::Registries::vanilla(),
            enchantments: tuff_item::enchantment::vanilla(),
        }
    }
}

fn vanilla_tags_packet() -> Bytes {
    static PACKET: std::sync::OnceLock<Bytes> = std::sync::OnceLock::new();
    PACKET
        .get_or_init(|| tags::TagSet::vanilla().network_body())
        .clone()
}

impl Contents {
    pub fn function(&self, id: &str) -> Option<&Arc<Function>> {
        self.functions.get(id)
    }

    /// The functions of a tag, in order; empty for an unknown tag.
    pub fn tag(&self, id: &str) -> Vec<Arc<Function>> {
        self.function_tags
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|function| self.functions.get(function).cloned())
            .collect()
    }

    pub fn has_tag(&self, id: &str) -> bool {
        self.function_tags.contains_key(id)
    }
}

/// A pack's files, by path relative to the pack root.
trait Files {
    fn names(&self) -> Vec<&str>;
    fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>>;
}

struct DirectoryFiles<'a> {
    backend: &'a dyn WorldBackend,
    prefix: String,
    names: Vec<String>,
}

impl Files for DirectoryFiles<'_> {
    fn names(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }

    fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        self.backend
            .read_extra_file_capped(&format!("{}{name}", self.prefix), max)
    }
}

/// A built-in feature pack's compiled-in files.
struct FeatureFiles(&'static tuff_vanilla_data::version::FeaturePack);

impl Files for FeatureFiles {
    fn names(&self) -> Vec<&str> {
        self.0.files.iter().map(|(path, _)| *path).collect()
    }

    fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        let (_, json) = self
            .0
            .files
            .iter()
            .find(|(path, _)| *path == name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, name.to_owned()))?;
        if json.len() > max {
            return Err(invalid(format!("{name} is over {max} bytes")));
        }
        Ok(json.as_bytes().to_vec())
    }
}

impl Files for zip::Archive<'_> {
    fn names(&self) -> Vec<&str> {
        zip::Archive::names(self)
            .iter()
            .map(String::as_str)
            .collect()
    }

    fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        zip::Archive::read(self, name, max)
    }
}

fn directory_files<'a>(
    backend: &'a dyn WorldBackend,
    prefix: &str,
) -> io::Result<DirectoryFiles<'a>> {
    let names = backend
        .list_extra_files_under(prefix)?
        .into_iter()
        .filter_map(|path| path.strip_prefix(prefix).map(str::to_owned))
        .collect();
    Ok(DirectoryFiles {
        backend,
        prefix: prefix.to_owned(),
        names,
    })
}

fn read_archive(backend: &dyn WorldBackend, path: &str) -> io::Result<Vec<u8>> {
    backend.read_extra_file_capped(path, MAX_ARCHIVE)
}

fn read_meta(files: &dyn Files) -> io::Result<PackMeta> {
    let bytes = files.read(METADATA, MAX_FILE)?;
    PackMeta::parse(&bytes).map_err(invalid)
}

/// Every pack the world offers: `vanilla`, then its own packs by id.
/// Entries that are not packs, or whose metadata cannot be read, are
/// skipped with a warning.
pub fn discover(backend: &dyn WorldBackend) -> Vec<PackInfo> {
    let mut packs = vec![PackInfo::vanilla()];
    let features = tuff_vanilla_data::version::FEATURE_PACKS
        .iter()
        .map(PackInfo::feature);
    let paths = match backend.list_extra_files_under(DIRECTORY) {
        Ok(paths) => paths,
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            packs.extend(features);
            return packs;
        }
        Err(err) => {
            log::warn!("Failed to list packs in {DIRECTORY}: {err}");
            packs.extend(features);
            return packs;
        }
    };
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for path in &paths {
        let Some(rest) = path.strip_prefix(DIRECTORY) else {
            continue;
        };
        let (name, location) = match rest.split_once('/') {
            Some((name, METADATA)) => (name, Location::Directory(format!("{DIRECTORY}{name}/"))),
            Some(_) => continue,
            None if rest.ends_with(".zip") => (rest, Location::Zip(path.clone())),
            None => continue,
        };
        if name.is_empty() || !seen.insert(name.to_owned()) {
            continue;
        }
        found.push((name.to_owned(), location));
    }
    for (name, location) in found {
        let id = format!("file/{name}");
        let meta = match &location {
            Location::Directory(prefix) => {
                directory_files(backend, prefix).and_then(|files| read_meta(&files))
            }
            Location::Zip(path) => read_archive(backend, path).and_then(|bytes| {
                let archive = zip::Archive::open(&bytes, MAX_ENTRIES)?;
                read_meta(&archive)
            }),
            Location::BuiltIn | Location::Feature(_) => continue,
        };
        match meta {
            Ok(meta) => packs.push(PackInfo {
                id,
                title: TextComponent::new(name),
                source: PackSource::World,
                meta,
                location,
                world_features: default_features(),
            }),
            Err(err) => log::warn!("Failed to read pack {id} metadata: {err}"),
        }
    }
    packs[1..].sort_by(|a, b| a.id.cmp(&b.id));
    packs.extend(features);
    packs
}

/// The feature flags of a world that has no others.
fn default_features() -> Arc<[String]> {
    store::WORLD_FEATURES
        .iter()
        .map(|f| (*f).to_owned())
        .collect()
}

/// Every feature flag this server knows: the ones a new world may turn on.
fn known_features() -> Arc<[String]> {
    let mut flags: Vec<String> = default_features().to_vec();
    for pack in tuff_vanilla_data::version::FEATURE_PACKS {
        for flag in pack.features {
            if !flags.iter().any(|f| f == flag) {
                flags.push((*flag).to_owned());
            }
        }
    }
    flags.into()
}

/// Records the world's feature flags on every pack, for
/// [`PackInfo::missing_features`].
fn stamp(packs: &mut [PackInfo], features: &Arc<[String]>) {
    for pack in packs {
        pack.world_features = features.clone();
    }
}

/// Turns on the feature packs that provide a flag the world has but no
/// selected pack asks for, as an experimental world's packs must stay.
fn force_feature_packs(packs: &[PackInfo], selected: &mut Vec<String>, forced: &[String]) {
    let requested = |selected: &[String]| -> Vec<String> {
        selected
            .iter()
            .filter_map(|id| packs.iter().find(|p| &p.id == id))
            .flat_map(|p| p.meta.features.iter().cloned())
            .collect()
    };
    let provided = requested(selected);
    let mut missing: Vec<&String> = forced
        .iter()
        .filter(|flag| !provided.contains(flag) && !store::WORLD_FEATURES.contains(&flag.as_str()))
        .collect();
    for pack in packs.iter().filter(|p| p.source == PackSource::Feature) {
        if missing.is_empty() {
            break;
        }
        let flags = &pack.meta.features;
        if !flags.is_empty()
            && flags.iter().any(|f| missing.contains(&f))
            && flags.iter().all(|f| forced.contains(f))
            && !selected.contains(&pack.id)
        {
            log::info!(
                "Found feature pack ('{}') for requested feature, forcing to enabled",
                pack.id
            );
            selected.push(pack.id.clone());
            missing.retain(|flag| !flags.contains(flag));
        }
    }
}

/// The world's feature flags: those its packs ask for and those it had.
fn world_features(packs: &[PackInfo], selected: &[String], forced: &[String]) -> Arc<[String]> {
    let mut flags: Vec<String> = default_features().to_vec();
    let requested = selected
        .iter()
        .filter_map(|id| packs.iter().find(|p| &p.id == id))
        .flat_map(|p| p.meta.features.iter());
    for flag in requested.chain(forced) {
        if !flags.contains(flag) {
            flags.push(flag.clone());
        }
    }
    flags.sort();
    flags.into()
}

fn available(packs: &[PackInfo], id: &str) -> bool {
    packs.iter().any(|p| p.id == id)
}

/// `ids` with unavailable packs dropped and required ones added at the
/// bottom, as the selection is rebuilt after every change.
pub fn normalise(packs: &[PackInfo], ids: &[String]) -> Vec<String> {
    let mut selected: Vec<String> = Vec::new();
    for id in ids {
        if available(packs, id) && !selected.contains(id) {
            selected.push(id.clone());
        }
    }
    for pack in packs.iter().filter(|p| p.required()) {
        if !selected.contains(&pack.id) {
            selected.insert(0, pack.id.clone());
        }
    }
    selected
}

/// The selection a world starts with: its saved enabled packs that still
/// exist, then every new pack it has not disabled.
pub fn configure(packs: &[PackInfo], config: &DataPackConfig) -> Vec<String> {
    let mut selected: Vec<String> = Vec::new();
    for id in &config.enabled {
        if available(packs, id) {
            if !selected.contains(id) {
                selected.push(id.clone());
            }
        } else {
            log::warn!("Missing data pack {id}");
        }
    }
    for pack in packs {
        if config.disabled.contains(&pack.id) || selected.contains(&pack.id) {
            continue;
        }
        if pack.added_automatically() {
            log::info!("Found new data pack {}, loading it automatically", pack.id);
            selected.push(pack.id.clone());
        } else if pack.source == PackSource::World {
            log::info!(
                "Found new data pack {}, but can't load it due to missing features {}",
                pack.id,
                pack.missing_features().join(", ")
            );
        }
    }
    selected.retain(|id| {
        let Some(pack) = packs.iter().find(|p| &p.id == id) else {
            return true;
        };
        let missing = pack.missing_features();
        if !missing.is_empty() {
            log::warn!(
                "Pack {id} requires features {} that are not enabled for this world, disabling pack.",
                missing.join(", ")
            );
        }
        missing.is_empty()
    });
    if selected.is_empty() {
        selected.push(VANILLA.to_owned());
    }
    normalise(packs, &selected)
}

/// The saved form of `selected`: every other available pack is disabled,
/// and disabled packs this server does not know stay listed.
pub fn saved_config(
    packs: &[PackInfo],
    selected: &[String],
    previous: &DataPackConfig,
) -> DataPackConfig {
    let mut disabled: Vec<String> = packs
        .iter()
        .filter(|p| !selected.contains(&p.id))
        .map(|p| p.id.clone())
        .collect();
    for id in &previous.disabled {
        if !available(packs, id) && !disabled.contains(id) {
            disabled.push(id.clone());
        }
    }
    DataPackConfig {
        enabled: selected.to_vec(),
        disabled,
    }
}

/// What one pack contributes, keyed by path below `data/`.
#[derive(Default)]
struct PackData {
    functions: Vec<(String, Vec<u8>)>,
    function_tags: Vec<(String, Vec<u8>)>,
    /// `(registry, tag, file)`.
    tags: Vec<(&'static str, String, Vec<u8>)>,
    recipes: Vec<(String, Vec<u8>)>,
    loot_tables: Vec<(String, Vec<u8>)>,
    predicates: Vec<(String, Vec<u8>)>,
    item_modifiers: Vec<(String, Vec<u8>)>,
    slot_sources: Vec<(String, Vec<u8>)>,
    advancements: Vec<(String, Vec<u8>)>,
    structures: Vec<(String, Vec<u8>)>,
    test_instances: Vec<(String, Vec<u8>)>,
    test_environments: Vec<(String, Vec<u8>)>,
    /// `(registry, entry, file)` of the data-driven registries.
    registries: Vec<(
        &'static tuff_vanilla_data::data_registry::DataRegistry,
        String,
        Vec<u8>,
    )>,
    unsupported: BTreeMap<String, usize>,
}

enum Kind {
    Function,
    FunctionTag,
    Tag(&'static str),
    Recipe,
    LootTable,
    Predicate,
    ItemModifier,
    SlotSource,
    Advancement,
    Structure,
    TestInstance,
    TestEnvironment,
}

/// What a resource path below a namespace holds, and its id path.
fn classify(path: &str) -> Option<(Kind, &str)> {
    if let Some(function) = path
        .strip_prefix(FUNCTIONS)
        .and_then(|p| p.strip_suffix(".mcfunction"))
    {
        return Some((Kind::Function, function));
    }
    if let Some(template) = path
        .strip_prefix(STRUCTURES)
        .and_then(|p| p.strip_suffix(".nbt"))
    {
        return Some((Kind::Structure, template));
    }
    let json = path.strip_suffix(".json")?;
    if let Some(tag) = json.strip_prefix(FUNCTION_TAGS) {
        return Some((Kind::FunctionTag, tag));
    }
    if let Some(tag) = json.strip_prefix(TAGS) {
        let (registry, tag) = tags::split_registry(tag)?;
        return Some((Kind::Tag(registry), tag));
    }
    [
        (RECIPES, Kind::Recipe),
        (LOOT_TABLES, Kind::LootTable),
        (PREDICATES, Kind::Predicate),
        (ITEM_MODIFIERS, Kind::ItemModifier),
        (SLOT_SOURCES, Kind::SlotSource),
        (ADVANCEMENTS, Kind::Advancement),
        (TEST_INSTANCES, Kind::TestInstance),
        (TEST_ENVIRONMENTS, Kind::TestEnvironment),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| json.strip_prefix(prefix).map(|id| (kind, id)))
}

/// The kind directory of a resource path (`recipe` for
/// `ns/recipe/x.json`, `tags/item` for `ns/tags/item/x.json`).
fn kind_of(path: &str) -> &str {
    let mut parts = path.splitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some("tags"), Some(registry)) => &path[..5 + registry.len()],
        (Some(kind), _) => kind,
        _ => path,
    }
}

/// The filters of the packs above, which hide what this pack offers.
type Hiders<'a> = [&'a [meta::FilterPattern]];

fn hidden(hiders: &Hiders<'_>, namespace: &str, path: &str) -> bool {
    hiders
        .iter()
        .any(|filter| meta::filtered(filter, namespace, path))
}

fn collect(
    id: &str,
    files: &dyn Files,
    meta: &PackMeta,
    hiders: &Hiders<'_>,
    budget: &mut usize,
) -> io::Result<PackData> {
    let mut chosen: BTreeMap<String, String> = BTreeMap::new();
    let roots: Vec<String> = std::iter::once("data/".to_owned())
        .chain(
            meta.active_overlays()
                .iter()
                .map(|dir| format!("{dir}/data/")),
        )
        .collect();
    let names = files.names();
    for root in &roots {
        for name in &names {
            if let Some(resource) = name.strip_prefix(root.as_str()) {
                chosen.insert(resource.to_owned(), (*name).to_owned());
            }
        }
    }
    let mut data = PackData::default();
    for (resource, name) in chosen {
        let Some((namespace, path)) = resource.split_once('/') else {
            continue;
        };
        if !valid_namespace(namespace) {
            log::warn!("Invalid namespace {namespace:?} in pack {id}, ignoring {name}");
            continue;
        }
        if !valid_path(path) {
            log::warn!("Invalid path {path:?} in pack {id}, ignoring {name}");
            continue;
        }
        if hidden(hiders, namespace, path) {
            continue;
        }
        let Some((kind, rest)) = classify(path) else {
            if let Some((registry, entry)) = tuff_item::registries::classify(path) {
                let bytes = files.read(&name, MAX_FILE.min(*budget))?;
                *budget -= bytes.len();
                data.registries
                    .push((registry, format!("{namespace}:{entry}"), bytes));
                continue;
            }
            *data
                .unsupported
                .entry(kind_of(path).to_owned())
                .or_default() += 1;
            continue;
        };
        let rid = format!("{namespace}:{rest}");
        let bytes = files.read(&name, MAX_FILE.min(*budget))?;
        *budget -= bytes.len();
        match kind {
            Kind::Function => data.functions.push((rid, bytes)),
            Kind::FunctionTag => data.function_tags.push((rid, bytes)),
            Kind::Tag(registry) => data.tags.push((registry, rid, bytes)),
            Kind::Recipe => data.recipes.push((rid, bytes)),
            Kind::LootTable => data.loot_tables.push((rid, bytes)),
            Kind::Predicate => data.predicates.push((rid, bytes)),
            Kind::ItemModifier => data.item_modifiers.push((rid, bytes)),
            Kind::SlotSource => data.slot_sources.push((rid, bytes)),
            Kind::Advancement => data.advancements.push((rid, bytes)),
            Kind::Structure => data.structures.push((rid, bytes)),
            Kind::TestInstance | Kind::TestEnvironment => {
                if let Some((registry, _)) = tuff_item::registries::classify(path) {
                    data.registries.push((registry, rid.clone(), bytes.clone()));
                }
                if matches!(kind, Kind::TestInstance) {
                    data.test_instances.push((rid, bytes));
                } else {
                    data.test_environments.push((rid, bytes));
                }
            }
        }
    }
    Ok(data)
}

/// The packs on offer and which are enabled, in order.
#[derive(Debug, Clone)]
pub struct Status {
    pub available: Vec<PackInfo>,
    pub selected: Vec<String>,
}

impl Status {
    pub fn pack(&self, id: &str) -> Option<&PackInfo> {
        self.available.iter().find(|p| p.id == id)
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.iter().any(|s| s == id)
    }
}

/// How a reload picks its packs.
#[derive(Debug, Clone)]
pub enum Change {
    /// The current selection plus every new pack not disabled (`/reload`).
    Discover,
    /// Exactly these packs (`/datapack enable`/`disable`).
    Select(Vec<String>),
}

enum Request {
    #[cfg(test)]
    Panic,
    Status {
        rescan: bool,
        reply: oneshot::Sender<Status>,
    },
    Reload {
        change: Change,
        reply: oneshot::Sender<Result<Status, String>>,
    },
    FunctionPermission {
        level: PermissionLevel,
        reply: oneshot::Sender<Result<Status, String>>,
    },
}

struct State {
    backend: Arc<dyn WorldBackend>,
    commands: Arc<crate::world::commands::Registry>,
    base: Base,
    available: Vec<PackInfo>,
    selected: Vec<String>,
    features: Arc<[String]>,
    config: watch::Sender<DataPackConfig>,
    contents: watch::Sender<Arc<Contents>>,
    known: watch::Sender<Arc<Status>>,
}

impl State {
    fn status(&self) -> Status {
        Status {
            available: self.available.clone(),
            selected: self.selected.clone(),
        }
    }

    async fn rescan(&mut self) {
        let backend = self.backend.clone();
        if let Ok(mut packs) = tokio::task::spawn_blocking(move || discover(&*backend)).await {
            stamp(&mut packs, &self.features);
            self.available = packs;
            self.known.send_replace(Arc::new(self.status()));
        }
    }

    async fn reload(&mut self, change: Change) -> Result<Status, String> {
        self.rescan().await;
        let wanted = match change {
            Change::Discover => {
                let disabled = self.config.borrow().disabled.clone();
                let mut wanted = self.selected.clone();
                for pack in &self.available {
                    if pack.added_automatically()
                        && !disabled.contains(&pack.id)
                        && !wanted.contains(&pack.id)
                    {
                        wanted.push(pack.id.clone());
                    }
                }
                wanted
            }
            Change::Select(ids) => ids,
        };
        let mut wanted: Vec<String> = wanted
            .into_iter()
            .filter(|id| {
                self.available
                    .iter()
                    .find(|p| &p.id == id)
                    .is_none_or(|p| p.missing_features().is_empty())
            })
            .collect();
        force_feature_packs(&self.available, &mut wanted, &self.features);
        let selected = normalise(&self.available, &wanted);
        let backend = self.backend.clone();
        let commands = self.commands.clone();
        let base = self.base.clone();
        let packs = self.available.clone();
        let generation = self.contents.borrow().generation + 1;
        let chosen = selected.clone();
        let registries = self.contents.borrow().registries.clone();
        let contents = tokio::task::spawn_blocking(move || {
            load(
                &*backend,
                commands.roots(),
                &base,
                &packs,
                &chosen,
                generation,
                Some(registries),
            )
        })
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| {
            log::error!("Failed to reload data packs: {err}");
            err.to_string()
        })?;
        log::info!("Data packs reloaded");
        self.selected = selected;
        let config = saved_config(&self.available, &self.selected, &self.config.borrow());
        self.config.send_replace(config);
        self.contents.send_replace(Arc::new(contents));
        self.known.send_replace(Arc::new(self.status()));
        Ok(self.status())
    }

    async fn run(&mut self, requests: &mut mpsc::Receiver<Request>) {
        while let Some(request) = requests.recv().await {
            match request {
                #[cfg(test)]
                Request::Panic => panic!("test panic"),
                Request::Status { rescan, reply } => {
                    if rescan {
                        self.rescan().await;
                    }
                    let _ = reply.send(self.status());
                }
                Request::Reload { change, reply } => {
                    let _ = reply.send(self.reload(change).await);
                }
                Request::FunctionPermission { level, reply } => {
                    self.base.function_permission = level;
                    let selected = self.selected.clone();
                    let _ = reply.send(self.reload(Change::Select(selected)).await);
                }
            }
        }
    }
}

/// Applies the `feature-packs` startup setting to a saved world: the
/// feature packs listed are on and every other feature pack is off, so the
/// world's flags follow the setting rather than what it was created with.
pub fn apply_startup_feature_packs(
    config: DataPackConfig,
    features: Option<Vec<String>>,
    wanted: &[String],
) -> (DataPackConfig, Option<Vec<String>>) {
    let Some(features) = features else {
        return (config, None);
    };
    let packs = tuff_vanilla_data::version::FEATURE_PACKS;
    let is_feature = |id: &str| packs.iter().any(|p| p.id == id);
    let mut config = config;
    config.enabled.retain(|id| !is_feature(id));
    config.disabled.retain(|id| !is_feature(id));
    let mut flags: Vec<String> = features
        .into_iter()
        .filter(|flag| !packs.iter().any(|p| p.features.contains(&flag.as_str())))
        .collect();
    for pack in packs {
        if wanted.iter().any(|id| id == pack.id) {
            config.enabled.push(pack.id.to_owned());
            for flag in pack.features {
                if !flags.iter().any(|f| f == flag) {
                    flags.push((*flag).to_owned());
                }
            }
        } else {
            config.disabled.push(pack.id.to_owned());
        }
    }
    (config, Some(flags))
}

struct Opened {
    packs: Vec<PackInfo>,
    selected: Vec<String>,
    contents: Contents,
    saved: DataPackConfig,
    features: Arc<[String]>,
}

/// The selection a world starts with and its feature flags. `features` are
/// the flags it saved, `None` for a world being created, which may turn
/// on any flag its starting packs ask for.
pub fn configure_world(
    packs: &mut [PackInfo],
    config: &DataPackConfig,
    features: Option<&[String]>,
) -> (Vec<String>, Arc<[String]>) {
    let allowed = match features {
        Some(flags) => world_features(&[], &[], flags),
        None => known_features(),
    };
    stamp(packs, &allowed);
    let mut selected = configure(packs, config);
    let forced = features.unwrap_or_default();
    force_feature_packs(packs, &mut selected, forced);
    let flags = world_features(packs, &selected, forced);
    stamp(packs, &flags);
    (selected, flags)
}

fn initial_load(
    backend: &dyn WorldBackend,
    roots: &Roots,
    base: &Base,
    config: &DataPackConfig,
    features: Option<&[String]>,
) -> Opened {
    let mut packs = discover(backend);
    let (mut selected, features) = configure_world(&mut packs, config, features);
    loop {
        match load(backend, roots, base, &packs, &selected, 1, None) {
            Ok(contents) => {
                let saved = saved_config(&packs, &selected, config);
                return Opened {
                    packs,
                    selected,
                    contents,
                    saved,
                    features,
                };
            }
            Err(err) => {
                log::error!("Failed to load data packs, leaving out {}: {err}", err.pack);
                selected.retain(|id| *id != err.pack);
            }
        }
    }
}

/// The handle every part of the server reaches data packs through.
#[derive(Clone)]
pub struct DataPacks {
    requests: Option<mpsc::Sender<Request>>,
    contents: watch::Receiver<Arc<Contents>>,
    config: watch::Receiver<DataPackConfig>,
    known: watch::Receiver<Arc<Status>>,
    features: Arc<[String]>,
}

impl DataPacks {
    /// No packs and nothing to change, for worlds built without storage.
    pub fn inert() -> Self {
        Self {
            requests: None,
            contents: watch::channel(Arc::new(Contents::default())).1,
            config: watch::channel(DataPackConfig::default()).1,
            known: watch::channel(Arc::new(Status {
                available: vec![PackInfo::vanilla()],
                selected: vec![VANILLA.to_owned()],
            }))
            .1,
            features: default_features(),
        }
    }

    /// Discovers and loads the world's packs as `config` selects them and
    /// starts the actor. A pack that fails to load is left out. `features`
    /// are the world's saved feature flags, `None` for a new world.
    pub async fn open(
        backend: Arc<dyn WorldBackend>,
        commands: Arc<crate::world::commands::Registry>,
        base: Base,
        config: DataPackConfig,
        features: Option<Vec<String>>,
    ) -> Self {
        let opened = {
            let backend = backend.clone();
            let commands = commands.clone();
            let base = base.clone();
            tokio::task::spawn_blocking(move || {
                initial_load(
                    &*backend,
                    commands.roots(),
                    &base,
                    &config,
                    features.as_deref(),
                )
            })
            .await
        };
        let opened = opened.unwrap_or_else(|_| Opened {
            packs: vec![PackInfo::vanilla()],
            selected: vec![VANILLA.to_owned()],
            contents: Contents::default(),
            saved: DataPackConfig::default(),
            features: default_features(),
        });
        Self::start(backend, commands, base, opened)
    }

    /// [`Self::open`], loading on the calling thread.
    pub fn open_now(
        backend: Arc<dyn WorldBackend>,
        commands: Arc<crate::world::commands::Registry>,
        base: Base,
        config: DataPackConfig,
        features: Option<Vec<String>>,
    ) -> Self {
        let opened = initial_load(
            &*backend,
            commands.roots(),
            &base,
            &config,
            features.as_deref(),
        );
        Self::start(backend, commands, base, opened)
    }

    fn start(
        backend: Arc<dyn WorldBackend>,
        commands: Arc<crate::world::commands::Registry>,
        base: Base,
        opened: Opened,
    ) -> Self {
        let (config_tx, config) = watch::channel(opened.saved);
        let (contents_tx, contents) = watch::channel(Arc::new(opened.contents));
        let (known_tx, known) = watch::channel(Arc::new(Status {
            available: opened.packs.clone(),
            selected: opened.selected.clone(),
        }));
        let (requests, receiver) = mpsc::channel(REQUEST_QUEUE);
        let features = opened.features.clone();
        let state = State {
            backend,
            commands,
            base,
            available: opened.packs,
            selected: opened.selected,
            features: opened.features,
            config: config_tx,
            contents: contents_tx,
            known: known_tx,
        };
        tuff_supervise::spawn(
            "the data pack state",
            tuff_supervise::restarting(
                "the data pack state",
                (state, receiver),
                |(state, requests)| Box::pin(state.run(requests)),
            ),
        );
        Self {
            requests: Some(requests),
            contents,
            config,
            known,
            features,
        }
    }

    /// The world's feature flags, fixed once it is open.
    pub fn features(&self) -> Arc<[String]> {
        self.features.clone()
    }

    /// Whether the world has the feature flag `flag` (`namespace:path`).
    pub fn has_feature(&self, flag: &str) -> bool {
        self.features.iter().any(|f| f == flag)
    }

    /// The loaded contents; never waits.
    pub fn contents(&self) -> Arc<Contents> {
        self.contents.borrow().clone()
    }

    /// A receiver that sees every newly loaded set of contents.
    pub fn watch_contents(&self) -> watch::Receiver<Arc<Contents>> {
        self.contents.clone()
    }

    /// The packs as last scanned, without asking the actor.
    pub fn known(&self) -> Arc<Status> {
        self.known.borrow().clone()
    }

    /// The selection as the world saves it.
    pub fn config(&self) -> DataPackConfig {
        self.config.borrow().clone()
    }

    #[cfg(test)]
    async fn panic_for_test(&self) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(Request::Panic).await;
        }
    }

    /// The packs on offer, rescanning the world's pack folder first when
    /// `rescan`. `None` once the actor is gone (or for an inert handle).
    pub async fn status(&self, rescan: bool) -> Option<Status> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .as_ref()?
            .send(Request::Status { rescan, reply })
            .await
            .ok()?;
        answer.await.ok()
    }

    /// Reloads with the packs `change` picks. The error says why nothing
    /// changed.
    pub async fn reload(&self, change: Change) -> Result<Status, String> {
        let (reply, answer) = oneshot::channel();
        let requests = self.requests.as_ref().ok_or("data packs are not loaded")?;
        requests
            .send(Request::Reload { change, reply })
            .await
            .map_err(|_| "the world has stopped".to_owned())?;
        answer
            .await
            .map_err(|_| "the world has stopped".to_owned())?
    }

    /// Recompiles every function with `level` as its permission level
    /// (`function-permission-level`), keeping the selected packs.
    pub async fn set_function_permission(&self, level: PermissionLevel) -> Result<Status, String> {
        let (reply, answer) = oneshot::channel();
        let requests = self.requests.as_ref().ok_or("data packs are not loaded")?;
        requests
            .send(Request::FunctionPermission { level, reply })
            .await
            .map_err(|_| "the world has stopped".to_owned())?;
        answer
            .await
            .map_err(|_| "the world has stopped".to_owned())?
    }
}

#[cfg(test)]
mod tests;

impl tags::HasTags for Contents {
    fn tag_set(&self) -> Arc<tags::TagSet> {
        self.tags.clone()
    }
}
mod loading;
pub use loading::*;
