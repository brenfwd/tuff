//! The dimensions of a world, built from the `dimensions` of its world gen
//! settings: every one the data names exists unless a setting switches a
//! vanilla one off ([`ENABLED_KEYS`]), with the vanilla ones getting their
//! placeholder terrain and the rest an empty void. Actors refer to a
//! dimension by [`DimensionId`], its index in [`Dimensions`], which never
//! changes while the world runs. Plugin dimensions
//! ([`crate::world::custom_dimension`]) follow the level's own, and
//! dimensions made while the world runs ([`runtime`]) follow those.

mod runtime;

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::watch;
use tuff_vanilla_data::data_registry::DIMENSION_TYPE;
use tuff_vanilla_data::dimension_type::{self, DimensionType};

use crate::storage::level::{DimensionSettings, Spawn, WorldBorder};
use crate::world::border::Border;
use crate::world::clock::{ClockId, clock_id};
use crate::world::custom_dimension::{CustomDimensions, PackTypes};
use crate::world::generator::{self, ChunkGenerator};
use tuff_world_model::BlockPos;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::ChunkShape;

pub use runtime::{CreateError, MAX_RUNTIME};

pub const OVERWORLD: &str = "minecraft:overworld";
pub const THE_NETHER: &str = "minecraft:the_nether";
pub const THE_END: &str = "minecraft:the_end";

/// The setting that switches each vanilla dimension on or off, by key.
pub const ENABLED_KEYS: [(&str, &str); 3] = [
    (OVERWORLD, "tuff.world.dimensions.overworld.enabled"),
    (THE_NETHER, "tuff.world.dimensions.nether.enabled"),
    (THE_END, "tuff.world.dimensions.end.enabled"),
];
/// The settings that make players above a height take void damage, by
/// vanilla dimension key: (height, amount).
const CEILING_KEYS: [(&str, &str, &str); 3] = [
    (
        OVERWORLD,
        "tuff.world.dimensions.overworld.ceilingDamage.height",
        "tuff.world.dimensions.overworld.ceilingDamage.amount",
    ),
    (
        THE_NETHER,
        "tuff.world.dimensions.nether.ceilingDamage.height",
        "tuff.world.dimensions.nether.ceilingDamage.amount",
    ),
    (
        THE_END,
        "tuff.world.dimensions.end.ceilingDamage.height",
        "tuff.world.dimensions.end.ceilingDamage.amount",
    ),
];
pub const CEILING_DEFAULT_AMOUNT: i64 = 4;

/// The height at or above which survival players in `key` are hurt and the
/// damage per hurt tick, or None when the ceiling is off.
pub fn ceiling_damage(settings: &crate::config::ConfigStore, key: &str) -> Option<(f64, f32)> {
    let (_, height, amount) = CEILING_KEYS
        .iter()
        .find(|(dimension, ..)| *dimension == key)?;
    let height = settings.get_int(height).filter(|height| *height > 0)?;
    let amount = settings.get_int(amount).unwrap_or(CEILING_DEFAULT_AMOUNT);
    Some((height as f64, amount as f32))
}

/// The operator's spawn dimension, over any feature's claim.
pub const SPAWN_DIMENSION_KEY: &str = "tuff.world.spawnDimension";

pub fn config_keys() -> Vec<crate::config::Key> {
    use crate::config::{Key, Value};
    let mut keys: Vec<Key> = ENABLED_KEYS
        .iter()
        .map(|(dimension, key)| {
            Key::new(
                key,
                Value::Bool(true),
                &format!(
                    "Whether the world has {dimension}; when off it is not loaded, ticked or sent and portals into it do nothing, but its saved data is kept (applies on restart)"
                ),
            )
        })
        .collect();
    for (dimension, height, amount) in CEILING_KEYS {
        keys.push(
            Key::new(
                height,
                Value::Int(0),
                &format!(
                    "Players in {dimension} at or above this y take void damage while in survival or adventure; 0 is off (for example 128 for a nether roof)"
                ),
            )
            .validated(|value| match value {
                Value::Int(n) if *n >= 0 => Ok(()),
                _ => Err("must be 0 or a height".to_owned()),
            }),
        );
        keys.push(
            Key::new(
                amount,
                Value::Int(CEILING_DEFAULT_AMOUNT),
                &format!(
                    "Health points of damage per hurt tick above the {dimension} ceiling height"
                ),
            )
            .validated(|value| match value {
                Value::Int(n) if *n >= 1 => Ok(()),
                _ => Err("must be at least 1".to_owned()),
            }),
        );
    }
    keys.push(
        Key::new(
            SPAWN_DIMENSION_KEY,
            Value::String(String::new()),
            "The dimension key new players spawn in and respawns fall back to, over any plugin's claim; empty for a plugin's claim, else the overworld (applies on restart)",
        )
        .validated(|value| match value {
            crate::config::Value::String(key) if key.is_empty() || valid_key(key) => Ok(()),
            _ => Err("must be empty or a dimension key like minecraft:the_nether".to_owned()),
        }),
    );
    keys
}

fn valid_key(key: &str) -> bool {
    let Some((namespace, path)) = key.split_once(':') else {
        return false;
    };
    let allowed = |c: char| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.');
    !namespace.is_empty()
        && !path.is_empty()
        && namespace.chars().all(allowed)
        && path.chars().all(|c| allowed(c) || c == '/')
}

/// Whether `settings` leave the dimension `key` in the world; only the
/// vanilla dimensions can be switched off.
pub fn enabled(settings: &crate::config::ConfigStore, key: &str) -> bool {
    ENABLED_KEYS
        .iter()
        .find(|(dimension, _)| *dimension == key)
        .and_then(|(_, setting)| settings.get_bool(setting))
        .unwrap_or(true)
}

/// The level's dimensions that `settings` leave in the world.
pub fn enabled_settings(
    all: &[DimensionSettings],
    settings: &crate::config::ConfigStore,
) -> Vec<DimensionSettings> {
    all.iter()
        .filter(|dimension| enabled(settings, &dimension.key))
        .cloned()
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DimensionId(u16);

impl DimensionId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn for_tests(index: u16) -> Self {
        Self(index)
    }
}

pub struct Dimension {
    pub id: DimensionId,
    /// The `minecraft:dimension` key, e.g. `minecraft:the_nether`.
    pub key: String,
    pub kind: &'static DimensionType,
    environment: &'static tuff_vanilla_data::world::Environment,
    type_id: i32,
    pub shape: ChunkShape,
    pub generator: Arc<dyn ChunkGenerator>,
    border: watch::Receiver<Border>,
    pub default_clock: Option<ClockId>,
    /// Where nether portals lit here lead, if anywhere.
    pub portal_link: Option<DimensionId>,
    /// Whether mobs spawn here on their own.
    pub natural_spawning: bool,
}

impl Dimension {
    pub fn sea_level(&self) -> i32 {
        self.generator.sea_level()
    }

    pub fn is_debug(&self) -> bool {
        self.generator.is_debug()
    }

    pub fn is_flat(&self) -> bool {
        self.generator.is_flat()
    }

    /// The `minecraft:dimension_type` network ID.
    pub fn type_id(&self) -> i32 {
        self.type_id
    }

    /// The dimension's gameplay rules (beds, water, sky light).
    pub fn environment(&self) -> &'static tuff_vanilla_data::world::Environment {
        self.environment
    }

    /// Whether the spawners the game runs only in the overworld (phantoms,
    /// patrols, wandering traders) run here.
    pub fn special_spawners(&self) -> bool {
        self.natural_spawning && self.key == OVERWORLD
    }

    pub fn has_sky_light(&self) -> bool {
        self.kind.has_skylight
    }

    /// Below this y the void hurts.
    pub fn void_y(&self) -> f64 {
        (self.shape.min_y - 64) as f64
    }

    /// One past the highest y a portal may occupy.
    pub fn logical_max_y(&self) -> i32 {
        (self.shape.min_y + self.kind.logical_height as i32).min(self.shape.max_y())
    }

    /// The border as it is now; the coordinator owns and moves it.
    pub fn border(&self) -> Border {
        *self.border.borrow()
    }

    pub fn watch_border(&self) -> watch::Receiver<Border> {
        self.border.clone()
    }

    /// How far the block column `(x, z)` is from the nearest border edge.
    pub fn distance_to_border(&self, x: i32, z: i32) -> f64 {
        self.border().distance(f64::from(x), f64::from(z))
    }

    /// The nearest block inside the border, as the game clamps portal
    /// targets.
    pub fn clamp_to_border(&self, x: f64, y: i32, z: f64) -> BlockPos {
        self.border().clamp_block(x, y, z)
    }

    /// Whether the block's corner lies inside the border, as pistons and
    /// block interaction ask.
    pub fn block_within_border(&self, pos: BlockPos) -> bool {
        self.border().contains_block(pos)
    }

    pub fn contains_block(&self, pos: BlockPos) -> bool {
        self.block_within_border(pos)
    }
}

/// Every dimension of the world, in the order of their IDs.
pub struct Dimensions {
    list: Vec<Dimension>,
    border_writers: Vec<watch::Sender<Border>>,
    custom: Arc<CustomDimensions>,
    spawn: DimensionId,
    /// Keys runtime dimensions may not take besides the dimensions here.
    reserved: HashSet<String>,
    runtime: runtime::Runtime,
}

impl Dimensions {
    /// Builds the dimensions the level data lists. Unknown dimension types
    /// take the overworld's shape and unknown generators produce a void,
    /// each reported once.
    pub fn from_settings(
        settings: &[DimensionSettings],
        seed: i64,
        generators: &dyn generator::WorldGenerators,
    ) -> Self {
        Self::from_settings_typed(settings, seed, generators, &PackTypes::vanilla())
    }

    /// Like [`Dimensions::from_settings`] with the dimension types the
    /// data packs left.
    pub fn from_settings_typed(
        settings: &[DimensionSettings],
        seed: i64,
        generators: &dyn generator::WorldGenerators,
        types: &PackTypes,
    ) -> Self {
        let make = |settings: &DimensionSettings, seed| {
            generators.generator(settings, seed).unwrap_or_else(|| {
                log::warn!(
                    "dimension {} uses generator {} which this server cannot run; it will be empty",
                    settings.key,
                    settings.generator
                );
                Arc::new(generator::Void)
            })
        };
        Self::from_settings_in(settings, seed, make, types)
    }

    /// Like [`Dimensions::from_settings`] with generators from `make`.
    pub fn from_settings_with(
        settings: &[DimensionSettings],
        seed: i64,
        make: impl Fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
    ) -> Self {
        Self::from_settings_in(settings, seed, make, &PackTypes::vanilla())
    }

    /// Like [`Dimensions::from_settings_with`] with the dimension types the
    /// data packs left.
    pub fn from_settings_in(
        settings: &[DimensionSettings],
        seed: i64,
        make: impl Fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
        types: &PackTypes,
    ) -> Self {
        let mut border_writers = Vec::with_capacity(settings.len());
        let mut list: Vec<Dimension> = settings
            .iter()
            .enumerate()
            .map(|(index, settings)| {
                let (kind, type_id) = types.get(&settings.kind).unwrap_or_else(|| {
                    log::warn!(
                        "dimension {} has unknown type {}; using the overworld's",
                        settings.key,
                        settings.kind
                    );
                    (
                        &dimension_type::OVERWORLD,
                        DIMENSION_TYPE
                            .entries
                            .binary_search(&OVERWORLD)
                            .map_or(0, |id| id as i32),
                    )
                });
                let (writer, border) = watch::channel(Border::from_saved(&settings.border));
                border_writers.push(writer);
                Dimension {
                    id: DimensionId(index as u16),
                    key: settings.key.clone(),
                    kind,
                    environment: tuff_vanilla_data::world::Environment::of(kind.name),
                    type_id,
                    shape: ChunkShape::new(kind.min_y, kind.height as i32)
                        .with_overworld(settings.key == "minecraft:overworld"),
                    generator: make(settings, seed),
                    border,
                    default_clock: kind.default_clock.and_then(clock_id),
                    portal_link: None,
                    natural_spawning: true,
                }
            })
            .collect();
        let find = |key: &str| list.iter().position(|d| d.key == key);
        let (overworld, nether) = (find(OVERWORLD), find(THE_NETHER));
        if let (Some(overworld), Some(nether)) = (overworld, nether) {
            list[overworld].portal_link = Some(DimensionId(nether as u16));
            list[nether].portal_link = Some(DimensionId(overworld as u16));
        }
        let spawn = DimensionId(overworld.unwrap_or(0) as u16);
        Self {
            list,
            border_writers,
            custom: Arc::default(),
            spawn,
            reserved: HashSet::new(),
            runtime: runtime::Runtime::default(),
        }
    }

    /// Points each dimension at the timelines the loaded data packs leave
    /// its type following. A type whose timelines fail to load keeps the
    /// ones it had, with a warning. Then binds each generator to the
    /// registries.
    pub fn apply_data_packs(
        &mut self,
        registries: &tuff_item::registries::Registries,
        tags: &tuff_datapack::tags::TagSet,
        settings: &crate::config::ConfigStore,
    ) {
        super::generator::bind_all(self, registries, settings);
        let types = registries.get("minecraft:dimension_type");
        for dimension in &mut self.list {
            let name = dimension.kind.name;
            let pack_json =
                types
                    .and_then(|types| types.entry(name))
                    .and_then(|entry| match entry {
                        tuff_item::registries::Entry::Loaded(raw) => raw.json.clone(),
                        tuff_item::registries::Entry::Builtin(_) => None,
                    });
            let plugin_json = self
                .custom
                .types()
                .iter()
                .find(|t| t.kind.name == name)
                .and_then(|t| t.json());
            let json = pack_json.as_deref().or(plugin_json.as_ref());
            match crate::world::custom_dimension::data_pack_environment(
                name,
                json,
                dimension.environment,
                registries,
                tags,
            ) {
                Ok(Some(environment)) => {
                    dimension.environment = Box::leak(Box::new(environment));
                }
                Ok(None) => {}
                Err(err) => log::warn!("dimension {} keeps its timelines: {err}", dimension.key),
            }
        }
    }

    /// Appends the plugin dimensions in `custom`, which must not share a
    /// key with any dimension here (see [`CustomDimensions::collect`]).
    /// Their borders start at the defaults.
    pub fn with_custom(mut self, custom: CustomDimensions, seed: i64) -> Self {
        let custom = Arc::new(custom);
        for resolved in custom.dimensions() {
            debug_assert!(self.by_key(&resolved.key).is_none());
            let (writer, border) = watch::channel(Border::from_saved(&WorldBorder::default()));
            self.border_writers.push(writer);
            let kind = resolved.kind;
            self.list.push(Dimension {
                id: DimensionId(self.list.len() as u16),
                key: resolved.key.clone(),
                kind,
                environment: resolved.environment,
                type_id: resolved.type_id,
                shape: ChunkShape::new(kind.min_y, kind.height as i32)
                    .with_overworld(resolved.key == "minecraft:overworld"),
                generator: resolved.generator(seed, custom.biomes()),
                border,
                default_clock: kind.default_clock.and_then(clock_id),
                portal_link: None,
                natural_spawning: resolved.natural_spawning,
            });
        }
        self.custom = custom;
        self
    }

    /// The plugin dimensions with their custom dimension types and biomes.
    pub fn custom(&self) -> &CustomDimensions {
        &self.custom
    }

    /// Every biome by id, custom ones included.
    pub fn biomes(&self) -> &Arc<Biomes> {
        self.custom.biomes()
    }

    /// Replaces the border of the dimension `key` with its saved state.
    pub fn restore_border(&mut self, key: &str, saved: &WorldBorder) {
        let Some(dimension) = self.by_key(key) else {
            return;
        };
        if let Some(writer) = self.border_writers.get(dimension.id.index()) {
            writer.send_modify(|border| *border = Border::from_saved(saved));
        }
    }

    /// Keeps every border's edges within `size` blocks of the origin.
    pub fn limit_world_size(&self, size: i32) {
        for writer in &self.border_writers {
            writer.send_modify(|border| border.absolute_max = size);
        }
    }

    /// Hands out the writing ends of the borders, once, to their owner.
    pub fn take_border_writers(&mut self) -> Vec<watch::Sender<Border>> {
        std::mem::take(&mut self.border_writers)
    }

    /// The dimension `id`, which may be a runtime dimension discarded
    /// since. Panics for an id this world never handed out.
    pub fn get(&self, id: DimensionId) -> &Dimension {
        match id.index().checked_sub(self.list.len()) {
            None => &self.list[id.index()],
            Some(slot) => self
                .runtime
                .get(slot)
                .unwrap_or_else(|| panic!("no dimension has id {}", id.index())),
        }
    }

    /// The dimension called `key`, runtime dimensions included while they
    /// are not discarded.
    pub fn by_key(&self, key: &str) -> Option<&Dimension> {
        self.list.iter().find(|d| d.key == key).or_else(|| {
            let slot = self.runtime.by_key(key)?;
            self.runtime.get(slot)
        })
    }

    /// The dimensions the world started with, without runtime ones.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Dimension> {
        self.list.iter()
    }

    /// The keys of the dimensions the world started with.
    pub fn keys(&self) -> Vec<&str> {
        self.list.iter().map(|d| d.key.as_str()).collect()
    }

    /// Keeps runtime dimensions from taking `keys`, such as dimensions the
    /// level lists but a setting switched off.
    pub fn reserve<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) {
        self.reserved.extend(keys.into_iter().map(str::to_owned));
    }

    /// Whether `key` belongs to the world itself: a dimension it started
    /// with, a reserved key, or the `minecraft` namespace.
    pub fn is_reserved(&self, key: &str) -> bool {
        key.starts_with("minecraft:")
            || self.reserved.contains(key)
            || self.list.iter().any(|d| d.key == key)
    }

    /// Makes a dimension called `key` with `base`'s type, height,
    /// environment and generator, owned by `owner`. Usable at once; it has
    /// no portal link and a default border, and is never in
    /// [`Self::iter`]. The caller keeps its chunks out of the world's
    /// storage.
    pub fn create(
        &self,
        key: &str,
        base: DimensionId,
        owner: &str,
    ) -> Result<DimensionId, CreateError> {
        if self.is_reserved(key) {
            return Err(CreateError::Reserved);
        }
        self.runtime
            .create(self.list.len(), key, self.get(base), owner)
    }

    fn runtime_slot(&self, id: DimensionId) -> Option<usize> {
        id.index().checked_sub(self.list.len())
    }

    /// Whether `id` is a runtime dimension, live or discarded.
    pub fn is_runtime(&self, id: DimensionId) -> bool {
        self.runtime_slot(id).is_some()
    }

    /// Whether `id` is a runtime dimension that was discarded.
    pub fn is_discarded(&self, id: DimensionId) -> bool {
        self.runtime_slot(id)
            .is_some_and(|slot| self.runtime.is_discarded(slot))
    }

    /// Who made runtime dimension `id`.
    pub fn owner(&self, id: DimensionId) -> Option<&str> {
        self.runtime.owner(self.runtime_slot(id)?)
    }

    /// Takes runtime dimension `id` out of [`Self::by_key`]; its key stays
    /// taken until [`Self::forget`]. False when it is not a live runtime
    /// dimension.
    pub fn discard(&self, id: DimensionId) -> bool {
        self.runtime_slot(id)
            .is_some_and(|slot| self.runtime.discard(slot))
    }

    /// Frees the key and id of a discarded runtime dimension, once nothing
    /// of it is loaded or stored.
    pub fn forget(&self, id: DimensionId) {
        if let Some(slot) = self.runtime_slot(id) {
            self.runtime.forget(slot);
        }
    }

    /// Whether a runtime dimension, live or being discarded, has `key`.
    pub fn runtime_taken(&self, key: &str) -> bool {
        self.runtime.taken(key)
    }

    /// Every runtime dimension not yet forgotten, discarded ones included.
    pub fn runtime(&self) -> Vec<&Dimension> {
        self.runtime
            .slots()
            .into_iter()
            .filter_map(|slot| self.runtime.get(slot))
            .collect()
    }

    /// The spawn dimension: the one [`Self::choose_spawn`] picked, else
    /// the overworld, else the first dimension.
    pub fn default_id(&self) -> DimensionId {
        self.spawn
    }

    /// Picks the spawn dimension: `configured` (the operator's setting)
    /// when it names a dimension here, else the first plugin dimension
    /// claiming the spawn. Says whether either picked one; otherwise the
    /// overworld (or the first dimension) stays.
    pub fn choose_spawn(&mut self, configured: &str) -> bool {
        let configured = configured.trim();
        if !configured.is_empty() {
            if let Some(dimension) = self.by_key(configured) {
                self.spawn = dimension.id;
                return true;
            }
            log::error!(
                "{SPAWN_DIMENSION_KEY} names {configured}, which this world does not have; ignoring it"
            );
        }
        let mut claims = self.custom.dimensions().iter().filter(|d| d.claims_spawn);
        let Some(first) = claims.next() else {
            return false;
        };
        for other in claims {
            log::warn!(
                "plugin dimension {} also claims the spawn; {} keeps it",
                other.key,
                first.key
            );
        }
        let Some(id) = self.by_key(&first.key).map(|d| d.id) else {
            return false;
        };
        self.spawn = id;
        true
    }

    /// Moves `spawn` to the spawn dimension's generator spawn point when
    /// its dimension is not in the world, or when `chosen` (a setting or
    /// claim picked the spawn dimension) and it lies in another one. Says
    /// whether it moved.
    pub fn place_spawn(&self, chosen: bool, spawn: &mut Spawn) -> bool {
        let target = self.get(self.spawn);
        let present = self.by_key(&spawn.dimension).is_some();
        if present && (!chosen || spawn.dimension == target.key) {
            return false;
        }
        log::info!(
            "the world spawn moves from {} to {}",
            spawn.dimension,
            target.key
        );
        *spawn = Spawn {
            dimension: target.key.clone(),
            pos: tuff_datapack::tags::built_in_scope(|| target.generator.spawn()),
            yaw: 0.0,
            pitch: 0.0,
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::level::{DEFAULT_BORDER_SIZE, WorldBorder};

    fn settings(
        key: &str,
        kind: &str,
        generator: &str,
        settings: Option<&str>,
    ) -> DimensionSettings {
        DimensionSettings::new(
            key.into(),
            kind.into(),
            generator.into(),
            settings.map(str::to_owned),
            WorldBorder {
                size: DEFAULT_BORDER_SIZE,
                ..WorldBorder::default()
            },
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn without_generators_every_dimension_is_void() {
        let dimensions =
            Dimensions::from_settings(&DimensionSettings::vanilla(), 1, &generator::NoGenerators);
        for dimension in dimensions.iter() {
            let chunk = dimension
                .generator
                .generate(tuff_world_model::pos::ChunkPos::new(0, 0), dimension.shape);
            assert!(chunk.sections().all(|section| section.has_only_air()));
            assert!(
                dimension
                    .generator
                    .clone()
                    .pipeline(generator::PipelineContext {
                        shape: dimension.shape,
                        clock: tokio::sync::watch::channel(0).1,
                        storage: None,
                        tags: Default::default(),
                    })
                    .is_none()
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_dimensions_get_their_shapes_and_links() {
        let dimensions =
            Dimensions::from_settings(&DimensionSettings::vanilla(), 1, &generator::NoGenerators);
        assert_eq!(dimensions.iter().len(), 3);
        let overworld = dimensions.by_key(OVERWORLD).unwrap();
        let nether = dimensions.by_key(THE_NETHER).unwrap();
        let end = dimensions.by_key(THE_END).unwrap();
        assert_eq!(overworld.shape, ChunkShape::OVERWORLD);
        assert_eq!(nether.shape, ChunkShape::new(0, 256));
        assert_eq!(end.shape, ChunkShape::new(0, 256));
        assert!(!nether.has_sky_light());
        assert_eq!(overworld.portal_link, Some(nether.id));
        assert_eq!(nether.portal_link, Some(overworld.id));
        assert_eq!(end.portal_link, None);
        assert_eq!(nether.logical_max_y(), 128);
        assert_eq!(dimensions.default_id(), overworld.id);
        assert!(nether.default_clock.is_none());
        assert!(overworld.default_clock.is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_dimensions_still_exist() {
        let mut all = DimensionSettings::vanilla();
        all.push(settings("pack:sky", "pack:sky_type", "pack:islands", None));
        let dimensions = Dimensions::from_settings(&all, 1, &generator::NoGenerators);
        let sky = dimensions.by_key("pack:sky").unwrap();
        assert_eq!(sky.shape, ChunkShape::new(-64, 384));
        assert!(!sky.shape.overworld);
        assert_eq!(sky.id.index(), 3);
        assert!(
            sky.generator
                .generate(tuff_world_model::pos::ChunkPos::new(0, 0), sky.shape)
                .sections()
                .all(|s| s.has_only_air())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn border_clamping_keeps_blocks_inside() {
        let mut all = DimensionSettings::vanilla();
        all[0] = settings(OVERWORLD, OVERWORLD, "minecraft:noise", Some(OVERWORLD));
        all[0].border.size = 100.0;
        let mut dimensions = Dimensions::from_settings(&all, 1, &generator::NoGenerators);
        let writers = dimensions.take_border_writers();
        assert_eq!(writers.len(), 3);
        assert!(dimensions.take_border_writers().is_empty());
        let overworld = dimensions.by_key(OVERWORLD).unwrap();
        assert_eq!(
            overworld.clamp_to_border(1000.0, 5, -1000.0),
            BlockPos::new(49, 5, -50)
        );
        assert!(overworld.contains_block(BlockPos::new(49, 0, -50)));
        assert!(!overworld.contains_block(BlockPos::new(50, 0, 0)));
        assert!(overworld.block_within_border(BlockPos::new(-50, 0, 49)));
        assert!(!overworld.block_within_border(BlockPos::new(50, 0, 0)));
        assert!(!overworld.block_within_border(BlockPos::new(0, 0, -51)));
        writers[overworld.id.index()].send_replace(Border::fixed([0.0, 0.0], 10.0));
        assert!(!overworld.contains_block(BlockPos::new(49, 0, -50)));
        assert_eq!(overworld.border().size(), 10.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn ceiling_damage_keys_default_to_off_per_dimension() {
        use crate::config::Value;
        let keys = config_keys();
        for dimension in ["overworld", "nether", "end"] {
            let height = format!("tuff.world.dimensions.{dimension}.ceilingDamage.height");
            let amount = format!("tuff.world.dimensions.{dimension}.ceilingDamage.amount");
            let find = |name: &str| keys.iter().find(|key| key.name == name).unwrap();
            assert_eq!(find(&height).default, Value::Int(0));
            assert_eq!(find(&amount).default, Value::Int(CEILING_DEFAULT_AMOUNT));
        }
    }

    struct Claims(Vec<(&'static str, bool)>);

    impl crate::world::custom_dimension::DimensionProvider for Claims {
        fn dimensions(&self) -> Vec<crate::world::custom_dimension::DimensionDefinition> {
            self.0
                .iter()
                .map(
                    |(key, claims_spawn)| crate::world::custom_dimension::DimensionDefinition {
                        key: (*key).into(),
                        dimension_type: OVERWORLD.into(),
                        type_json: String::new(),
                        biomes: Vec::new(),
                        natural_spawning: false,
                        claims_spawn: *claims_spawn,
                    },
                )
                .collect()
        }

        fn generator(
            &self,
            _: &str,
            _: i64,
            _: &Arc<tuff_world_model::biomes::Biomes>,
        ) -> Arc<dyn ChunkGenerator> {
            Arc::new(generator::Void)
        }
    }

    fn with_plugins(
        settings: &[DimensionSettings],
        claims: Vec<(&'static str, bool)>,
    ) -> Dimensions {
        let provider: Arc<dyn crate::world::custom_dimension::DimensionProvider> =
            Arc::new(Claims(claims));
        let custom = CustomDimensions::collect(&[provider], &[OVERWORLD, THE_NETHER, THE_END]);
        Dimensions::from_settings(settings, 1, &generator::NoGenerators).with_custom(custom, 1)
    }

    fn spawn_in(dimension: &str) -> Spawn {
        Spawn {
            dimension: dimension.into(),
            pos: BlockPos::new(10, 70, -10),
            yaw: 90.0,
            pitch: 0.0,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn switched_off_vanilla_dimensions_are_left_out() {
        let config = crate::config::ConfigStore::new();
        assert_eq!(
            enabled_settings(&DimensionSettings::vanilla(), &config).len(),
            3
        );
        config
            .set_text_for_run("tuff.world.dimensions.overworld.enabled", "false")
            .unwrap();
        assert!(!enabled(&config, OVERWORLD));
        assert!(enabled(&config, "example:sky"));
        let left = enabled_settings(&DimensionSettings::vanilla(), &config);
        let dimensions = Dimensions::from_settings(&left, 1, &generator::NoGenerators);
        assert_eq!(dimensions.keys(), [THE_END, THE_NETHER]);
        assert_eq!(dimensions.by_key(THE_NETHER).unwrap().portal_link, None);
        assert_eq!(
            dimensions.default_id(),
            dimensions.by_key(THE_END).unwrap().id
        );
        assert!(
            config
                .set_text_for_run(SPAWN_DIMENSION_KEY, "not a key")
                .is_err()
        );
        config
            .set_text_for_run(SPAWN_DIMENSION_KEY, "bedwars:bedwars")
            .unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_operator_setting_beats_the_first_plugin_claim() {
        let vanilla = DimensionSettings::vanilla();
        let claims = vec![("ex:a", false), ("ex:b", true), ("ex:c", true)];
        let mut dimensions = with_plugins(&vanilla, claims.clone());
        assert!(dimensions.choose_spawn(""));
        assert_eq!(dimensions.get(dimensions.default_id()).key, "ex:b");

        let mut dimensions = with_plugins(&vanilla, claims.clone());
        assert!(dimensions.choose_spawn(" minecraft:the_nether "));
        assert_eq!(dimensions.get(dimensions.default_id()).key, THE_NETHER);

        let mut dimensions = with_plugins(&vanilla, claims);
        assert!(dimensions.choose_spawn("ex:missing"));
        assert_eq!(dimensions.get(dimensions.default_id()).key, "ex:b");

        let mut dimensions = with_plugins(&vanilla, vec![("ex:a", false)]);
        assert!(!dimensions.choose_spawn(""));
        assert_eq!(dimensions.get(dimensions.default_id()).key, OVERWORLD);

        let mut dimensions = with_plugins(&[], vec![("ex:a", false), ("ex:b", false)]);
        assert!(!dimensions.choose_spawn(""));
        assert_eq!(dimensions.get(dimensions.default_id()).key, "ex:a");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_spawn_moves_only_when_chosen_elsewhere_or_gone() {
        let vanilla = DimensionSettings::vanilla();
        let mut dimensions = with_plugins(&vanilla, vec![("ex:lobby", true)]);
        let mut spawn = spawn_in(THE_NETHER);
        assert!(!dimensions.place_spawn(false, &mut spawn));
        assert_eq!(spawn, spawn_in(THE_NETHER));

        let chosen = dimensions.choose_spawn("");
        let mut lobby = spawn_in("ex:lobby");
        assert!(!dimensions.place_spawn(chosen, &mut lobby));
        assert_eq!(lobby, spawn_in("ex:lobby"));
        assert!(dimensions.place_spawn(chosen, &mut spawn));
        assert_eq!(
            spawn,
            Spawn {
                dimension: "ex:lobby".into(),
                pos: BlockPos::new(0, 64, 0),
                yaw: 0.0,
                pitch: 0.0,
            }
        );

        let dimensions = with_plugins(&vanilla[1..], vec![]);
        let mut gone = spawn_in(OVERWORLD);
        assert!(dimensions.place_spawn(false, &mut gone));
        assert_eq!(gone.dimension, THE_END);
    }

    #[tuff_core::watchdog]
    #[test]
    fn runtime_dimensions_copy_their_base_and_free_their_key_when_forgotten() {
        let mut dimensions =
            Dimensions::from_settings(&DimensionSettings::vanilla(), 1, &generator::NoGenerators);
        dimensions.reserve(["game:kept"]);
        let nether = dimensions.by_key(THE_NETHER).unwrap();
        let (nether_id, nether_type, nether_shape) = (nether.id, nether.type_id(), nether.shape);
        assert_eq!(
            dimensions.create("minecraft:arena", nether_id, "game"),
            Err(CreateError::Reserved)
        );
        assert_eq!(
            dimensions.create("game:kept", nether_id, "game"),
            Err(CreateError::Reserved)
        );
        let arena = dimensions.create("game:arena", nether_id, "game").unwrap();
        assert_eq!(arena.index(), 3);
        assert!(dimensions.is_runtime(arena) && !dimensions.is_runtime(nether_id));
        let made = dimensions.by_key("game:arena").unwrap();
        assert_eq!(made.id, arena);
        assert_eq!(made.type_id(), nether_type);
        assert_eq!(made.shape, nether_shape);
        assert!(made.portal_link.is_none());
        assert_eq!(dimensions.owner(arena), Some("game"));
        assert_eq!(dimensions.iter().len(), 3);
        assert_eq!(
            dimensions.create("game:arena", nether_id, "game"),
            Err(CreateError::Exists)
        );

        assert!(dimensions.discard(arena));
        assert!(!dimensions.discard(arena));
        assert!(dimensions.by_key("game:arena").is_none());
        assert!(dimensions.is_discarded(arena));
        assert_eq!(dimensions.get(arena).key, "game:arena");
        assert_eq!(
            dimensions.create("game:arena", nether_id, "game"),
            Err(CreateError::Exists)
        );
        dimensions.forget(arena);
        assert!(dimensions.runtime().is_empty());
        let again = dimensions.create("game:arena", nether_id, "game").unwrap();
        assert_ne!(again, arena, "freed ids are reused last");
        assert_eq!(dimensions.get(arena).key, "game:arena");
        assert!(dimensions.is_discarded(arena));
        assert!(!dimensions.is_discarded(again));
    }

    #[tuff_core::watchdog]
    #[test]
    fn runtime_dimensions_stop_at_the_cap() {
        let dimensions =
            Dimensions::from_settings(&DimensionSettings::vanilla(), 1, &generator::NoGenerators);
        let base = dimensions.default_id();
        for n in 0..MAX_RUNTIME {
            dimensions
                .create(&format!("game:a{n}"), base, "game")
                .unwrap();
        }
        assert_eq!(
            dimensions.create("game:over", base, "game"),
            Err(CreateError::Full)
        );
        let first = dimensions.by_key("game:a0").unwrap().id;
        assert!(dimensions.discard(first));
        dimensions.forget(first);
        assert_eq!(dimensions.create("game:over", base, "game"), Ok(first));
        assert_eq!(dimensions.runtime().len(), MAX_RUNTIME);
    }
}
