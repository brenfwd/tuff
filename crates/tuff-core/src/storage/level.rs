//! `level.dat` and the world-wide saved data files that 26.x split out of it.
//! Each file's root is kept whole and only the fields this server manages are
//! overwritten, so vanilla settings (game rules, data packs, ...) survive.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use tuff_vanilla_data::version::{DATA_VERSION, NAME, SERIES, STABLE};

use crate::protocol::play::GameMode;
use crate::world::clock::WorldClocks;
use crate::world::game_rules::GameRules;
use crate::world::player::Difficulty;
use crate::world::weather::Weather;
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::Documents;
use tuff_storage::document::Document;
use tuff_world_model::BlockPos;

/// Constant vanilla writes as `version` (`WorldData.ANVIL_VERSION_ID`).
const ANVIL_VERSION: i32 = 19133;
pub const OVERWORLD: &str = "minecraft:overworld";
pub const THE_NETHER: &str = "minecraft:the_nether";
pub const THE_END: &str = "minecraft:the_end";
const WORLD_GEN_SETTINGS: &str = "minecraft:world_gen_settings";
const WORLD_CLOCKS: &str = "minecraft:world_clocks";
pub(crate) const WORLD_BORDER: &str = "minecraft:world_border";
const GAME_RULES: &str = "minecraft:game_rules";
const WEATHER: &str = "minecraft:weather";
/// Vanilla's default and maximum border diameter.
pub const DEFAULT_BORDER_SIZE: f64 = 59_999_968.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Spawn {
    pub dimension: String,
    pub pos: BlockPos,
    pub yaw: f32,
    pub pitch: f32,
}

/// A dimension's saved `world_border` data. `lerp_time` is the ticks left
/// of a size change toward `lerp_target`; 0 means the size is fixed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldBorder {
    pub center: [f64; 2],
    pub size: f64,
    pub damage_per_block: f64,
    pub safe_zone: f64,
    pub warning_blocks: i32,
    /// In ticks.
    pub warning_time: i32,
    pub lerp_time: i64,
    pub lerp_target: f64,
}

impl Default for WorldBorder {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0],
            size: DEFAULT_BORDER_SIZE,
            damage_per_block: 0.2,
            safe_zone: 5.0,
            warning_blocks: 5,
            warning_time: 300,
            lerp_time: 0,
            lerp_target: 0.0,
        }
    }
}

impl WorldBorder {
    /// A dimension's border from its own saved data, for dimensions that
    /// are not listed in the level's world generation settings.
    pub fn read(docs: &dyn Documents, dimension: &str) -> io::Result<Self> {
        let root = docs.read(Document::DimensionData {
            dimension,
            id: WORLD_BORDER,
        })?;
        Ok(Self::load(&inner(root)))
    }

    pub fn write(&self, docs: &dyn Documents, dimension: &str) -> io::Result<()> {
        let mut border = Compound::new();
        self.save_into(&mut border);
        docs.write(
            Document::DimensionData {
                dimension,
                id: WORLD_BORDER,
            },
            saved(border),
        )
    }

    fn load(raw: &Compound) -> Self {
        let default = Self::default();
        Self {
            center: [
                raw.get_double("center_x").unwrap_or(default.center[0]),
                raw.get_double("center_z").unwrap_or(default.center[1]),
            ],
            size: raw.get_double("size").unwrap_or(default.size),
            damage_per_block: raw
                .get_double("damage_per_block")
                .unwrap_or(default.damage_per_block),
            safe_zone: raw.get_double("safe_zone").unwrap_or(default.safe_zone),
            warning_blocks: raw
                .get_int("warning_blocks")
                .unwrap_or(default.warning_blocks),
            warning_time: raw.get_int("warning_time").unwrap_or(default.warning_time),
            lerp_time: raw.get_long("lerp_time").unwrap_or(default.lerp_time),
            lerp_target: raw.get_double("lerp_target").unwrap_or(default.lerp_target),
        }
    }

    fn save_into(&self, border: &mut Compound) {
        border.insert("center_x", Tag::Double(self.center[0]));
        border.insert("center_z", Tag::Double(self.center[1]));
        border.insert("damage_per_block", Tag::Double(self.damage_per_block));
        border.insert("safe_zone", Tag::Double(self.safe_zone));
        border.insert("warning_blocks", Tag::Int(self.warning_blocks));
        border.insert("warning_time", Tag::Int(self.warning_time));
        border.insert("size", Tag::Double(self.size));
        border.insert("lerp_time", Tag::Long(self.lerp_time));
        border.insert("lerp_target", Tag::Double(self.lerp_target));
    }
}

/// The layers and biome of a `minecraft:flat` generator, bottom layer
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatSettings {
    pub biome: String,
    pub layers: Vec<(String, u32)>,
    /// Whether the biome's decoration features are placed.
    pub features: bool,
    /// Whether the underground and surface lava lakes are placed.
    pub lakes: bool,
    /// The structure sets that generate; `None` allows every set.
    pub structure_overrides: Option<Vec<String>>,
}

impl Default for FlatSettings {
    fn default() -> Self {
        Self {
            biome: "minecraft:plains".into(),
            layers: vec![
                ("minecraft:bedrock".into(), 1),
                ("minecraft:dirt".into(), 2),
                ("minecraft:grass_block".into(), 1),
            ],
            features: false,
            lakes: false,
            structure_overrides: Some(vec![
                "minecraft:villages".into(),
                "minecraft:strongholds".into(),
            ]),
        }
    }
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

impl FlatSettings {
    const MAX_HEIGHT: u32 = 384;

    /// Parses the `generator-settings` JSON; `None` when a layer names an
    /// unknown block, a layer height is outside `0..=384` or the heights
    /// sum past 384. An unknown biome falls back to plains. Missing keys
    /// take the default preset's values.
    pub fn from_json(json: &serde_json::Value) -> Option<Self> {
        let default = Self::default();
        let biome = match json.get("biome").and_then(|b| b.as_str()) {
            Some(b) => qualified(b),
            None => default.biome,
        };
        let biome = if tuff_world_model::chunk::biome_id(&biome).is_some() {
            biome
        } else {
            "minecraft:plains".to_owned()
        };
        let layers = match json.get("layers").and_then(|l| l.as_array()) {
            Some(list) => list
                .iter()
                .map(|layer| {
                    let block = qualified(layer.get("block")?.as_str()?);
                    tuff_vanilla_data::Block::from_name(&block)?;
                    let height = u32::try_from(layer.get("height")?.as_u64()?).ok()?;
                    (height <= Self::MAX_HEIGHT).then_some((block, height))
                })
                .collect::<Option<Vec<_>>>()
                .filter(|layers| layers.iter().map(|(_, h)| h).sum::<u32>() <= Self::MAX_HEIGHT)?,
            None => default.layers,
        };
        let flag = |key: &str| json.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let structure_overrides = match json.get("structure_overrides") {
            Some(serde_json::Value::String(set)) => Some(vec![qualified(set)]),
            Some(serde_json::Value::Array(sets)) => Some(
                sets.iter()
                    .filter_map(|s| s.as_str().map(qualified))
                    .collect(),
            ),
            _ => None,
        };
        Some(Self {
            biome,
            layers,
            features: flag("features"),
            lakes: flag("lakes"),
            structure_overrides,
        })
    }

    pub fn from_nbt(settings: &Compound) -> Option<Self> {
        let layers = match settings.get("layers")? {
            Tag::List(list) => list
                .iter()
                .map(|layer| {
                    let Tag::Compound(layer) = layer else {
                        return None;
                    };
                    Some((
                        layer.get_str("block")?.to_owned(),
                        u32::try_from(layer.get_int("height")?).ok()?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        let flag = |key: &str| settings.get_byte(key).is_some_and(|b| b != 0);
        let structure_overrides = match settings.get("structure_overrides") {
            Some(Tag::List(list)) => Some(
                list.iter()
                    .filter_map(|t| match t {
                        Tag::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect(),
            ),
            Some(Tag::String(set)) => Some(vec![set.to_string()]),
            _ => None,
        };
        Some(Self {
            biome: settings.get_str("biome")?.to_owned(),
            layers,
            features: flag("features"),
            lakes: flag("lakes"),
            structure_overrides,
        })
    }

    pub fn to_nbt(&self) -> Compound {
        let mut out = Compound::new()
            .with("biome", self.biome.as_str())
            .with("features", self.features)
            .with("lakes", self.lakes)
            .with(
                "layers",
                Tag::List(
                    self.layers
                        .iter()
                        .map(|(block, height)| {
                            Tag::Compound(
                                Compound::new()
                                    .with("block", block.as_str())
                                    .with("height", *height as i32),
                            )
                        })
                        .collect(),
                ),
            );
        if let Some(sets) = &self.structure_overrides {
            out.insert(
                "structure_overrides",
                Tag::List(
                    sets.iter()
                        .map(|s| Tag::String(s.as_str().into()))
                        .collect(),
                ),
            );
        }
        out
    }
}

/// One entry of a multi-noise source's inline `biomes` list: the biome and
/// its climate ranges (temperature, humidity, continentalness, erosion,
/// depth, weirdness) and offset, unquantized.
#[derive(Debug, Clone, PartialEq)]
pub struct BiomePoint {
    pub biome: String,
    pub ranges: [(f32, f32); 6],
    pub offset: f32,
}

/// One entry of the world gen settings' `dimensions`, with the dimension's
/// own saved data.
#[derive(Debug, Clone, PartialEq)]
pub struct DimensionSettings {
    /// The `minecraft:dimension` key.
    pub key: String,
    /// The `minecraft:dimension_type` key.
    pub kind: String,
    /// The generator type, e.g. `minecraft:noise`.
    pub generator: String,
    /// The generator's `settings` when it names a noise settings preset.
    pub settings: Option<String>,
    /// The generator's `biome_source`: its type, and the `preset` or
    /// `biome` it names.
    pub biome_source: Option<String>,
    pub biome_preset: Option<String>,
    pub biome: Option<String>,
    /// A checkerboard source's `biomes` (ids and `#tags`) and `scale`.
    pub biome_list: Vec<String>,
    pub biome_scale: Option<i32>,
    /// A multi-noise source's inline `biomes` list, empty when it names a
    /// `preset` or the list is malformed.
    pub biome_points: Vec<BiomePoint>,
    /// The `minecraft:flat` generator's settings.
    pub flat: Option<FlatSettings>,
    pub border: WorldBorder,
    /// The world's `generate_structures` option: without it no structure
    /// starts and nothing is located.
    pub generate_structures: bool,
    border_raw: Compound,
}

impl DimensionSettings {
    pub fn new(
        key: String,
        kind: String,
        generator: String,
        settings: Option<String>,
        border: WorldBorder,
    ) -> Self {
        Self {
            key,
            kind,
            generator,
            settings,
            biome_source: None,
            biome_preset: None,
            biome: None,
            biome_list: Vec::new(),
            biome_scale: None,
            biome_points: Vec::new(),
            flat: None,
            border,
            generate_structures: true,
            border_raw: Compound::new(),
        }
    }

    /// The three dimensions of a new vanilla world, sorted by key.
    pub fn vanilla() -> Vec<Self> {
        parse_dimensions(&default_dimensions(), |_| Compound::new())
    }
}

fn biome_list(source: &Compound) -> Vec<String> {
    match source.get("biomes") {
        Some(Tag::String(one)) => vec![one.clone()],
        Some(Tag::List(items)) => items
            .iter()
            .filter_map(|t| match t {
                Tag::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn climate_range(tag: &Tag) -> Option<(f32, f32)> {
    let (min, max) = match tag {
        Tag::List(pair) => match pair.as_slice() {
            [a, b] => (a.as_float()?, b.as_float()?),
            _ => return None,
        },
        Tag::Compound(c) => (c.get_float("min")?, c.get_float("max")?),
        other => {
            let v = other.as_float()?;
            (v, v)
        }
    };
    ((-2.0..=2.0).contains(&min) && (-2.0..=2.0).contains(&max) && min <= max).then_some((min, max))
}

/// The inline `biomes` of a multi-noise source; empty unless every entry
/// is valid.
fn biome_points(source: &Compound) -> Vec<BiomePoint> {
    let Some(Tag::List(entries)) = source.get("biomes") else {
        return Vec::new();
    };
    let point = |entry: &Tag| -> Option<BiomePoint> {
        let entry = entry.as_compound()?;
        let parameters = entry.get_compound("parameters")?;
        let range = |key: &str| climate_range(parameters.get(key)?);
        let offset = parameters.get_float("offset")?;
        (0.0..=1.0).contains(&offset).then_some(())?;
        Some(BiomePoint {
            biome: entry.get_str("biome")?.to_owned(),
            ranges: [
                range("temperature")?,
                range("humidity")?,
                range("continentalness")?,
                range("erosion")?,
                range("depth")?,
                range("weirdness")?,
            ],
            offset,
        })
    };
    entries
        .iter()
        .map(point)
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// Reads `dimensions` in key order, so IDs derived from the order are
/// stable across runs.
pub(crate) fn parse_dimensions(
    dimensions: &Compound,
    mut border_raw: impl FnMut(&str) -> Compound,
) -> Vec<DimensionSettings> {
    let mut keys: Vec<&str> = dimensions.keys().collect();
    keys.sort_unstable();
    keys.into_iter()
        .filter_map(|key| {
            let entry = dimensions.get_compound(key)?;
            let generator = entry.get_compound("generator");
            let biome_source = generator.and_then(|g| g.get_compound("biome_source"));
            let raw = border_raw(key);
            Some(DimensionSettings {
                key: key.to_owned(),
                kind: entry.get_str("type").unwrap_or(key).to_owned(),
                generator: generator
                    .and_then(|g| g.get_str("type"))
                    .unwrap_or("")
                    .to_owned(),
                settings: generator
                    .and_then(|g| g.get_str("settings"))
                    .map(str::to_owned),
                biome_source: biome_source
                    .and_then(|b| b.get_str("type"))
                    .map(str::to_owned),
                biome_preset: biome_source
                    .and_then(|b| b.get_str("preset"))
                    .map(str::to_owned),
                biome: biome_source
                    .and_then(|b| b.get_str("biome"))
                    .map(str::to_owned),
                biome_list: biome_source.map(biome_list).unwrap_or_default(),
                biome_scale: biome_source.and_then(|b| b.get_int("scale")),
                biome_points: biome_source.map(biome_points).unwrap_or_default(),
                flat: generator
                    .and_then(|g| g.get_compound("settings"))
                    .and_then(FlatSettings::from_nbt),
                border: WorldBorder::load(&raw),
                generate_structures: true,
                border_raw: raw,
            })
        })
        .collect()
}

/// Everything loaded from a world's top-level files at startup.
#[derive(Debug, Clone)]
pub struct LevelData {
    data: Compound,
    world_gen: Compound,
    clocks_raw: Compound,
    pub seed: i64,
    pub game_time: i64,
    pub clocks: WorldClocks,
    pub spawn: Spawn,
    pub difficulty: Difficulty,
    pub hardcore: bool,
    pub difficulty_locked: bool,
    /// The game mode of players joining for the first time.
    pub default_game_mode: GameMode,
    /// Every dimension of the world, sorted by key.
    pub dimensions: Vec<DimensionSettings>,
    pub game_rules: GameRules,
    pub weather: Weather,
    pub new_world: bool,
    /// A never-started world whose settings ask for the spawn bonus chest.
    pub bonus_chest_pending: bool,
}

fn difficulty_name(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Peaceful => "peaceful",
        Difficulty::Easy => "easy",
        Difficulty::Normal => "normal",
        Difficulty::Hard => "hard",
    }
}

fn parse_difficulty(name: &str) -> Option<Difficulty> {
    Some(match name {
        "peaceful" => Difficulty::Peaceful,
        "easy" => Difficulty::Easy,
        "normal" => Difficulty::Normal,
        "hard" => Difficulty::Hard,
        _ => return None,
    })
}

pub fn saved(data: Compound) -> Compound {
    Compound::new()
        .with("data", data)
        .with("DataVersion", DATA_VERSION)
}

pub fn inner(root: Option<Compound>) -> Compound {
    root.and_then(|mut root| match root.remove("data") {
        Some(Tag::Compound(data)) => Some(data),
        _ => None,
    })
    .unwrap_or_default()
}

fn default_dimensions() -> Compound {
    let noise = |settings: &str, biome_source: Compound| {
        Compound::new()
            .with("settings", settings)
            .with("biome_source", biome_source)
            .with("type", "minecraft:noise")
    };
    let multi_noise = |preset: &str| {
        Compound::new()
            .with("preset", preset)
            .with("type", "minecraft:multi_noise")
    };
    let dimension = |generator: Compound, kind: &str| {
        Compound::new()
            .with("generator", generator)
            .with("type", kind)
    };
    Compound::new()
        .with(
            OVERWORLD,
            dimension(noise(OVERWORLD, multi_noise(OVERWORLD)), OVERWORLD),
        )
        .with(
            THE_NETHER,
            dimension(
                noise("minecraft:nether", multi_noise("minecraft:nether")),
                THE_NETHER,
            ),
        )
        .with(
            THE_END,
            dimension(
                noise(
                    "minecraft:end",
                    Compound::new().with("type", "minecraft:the_end"),
                ),
                THE_END,
            ),
        )
}

impl LevelData {
    /// Applies the `generate-structures` property to a world that has just
    /// been created; existing worlds keep their saved option.
    pub fn set_new_world_structures(&mut self, generate: bool) {
        if !self.new_world {
            return;
        }
        self.world_gen.insert("generate_structures", generate);
        for dimension in &mut self.dimensions {
            dimension.generate_structures = generate;
        }
    }

    /// Applies the `level-type` and `generator-settings` properties to a
    /// world that has just been created, by replacing the overworld's
    /// generator. Returns false for a type this server cannot generate,
    /// which leaves the normal world.
    pub fn set_new_world_type(&mut self, level_type: &str, generator_settings: &str) -> bool {
        let settings = serde_json::from_str::<serde_json::Value>(generator_settings).ok();
        let biome = settings
            .as_ref()
            .and_then(|s| s.get("biome"))
            .and_then(|b| b.as_str())
            .map(|b| {
                if b.contains(':') {
                    b.to_owned()
                } else {
                    format!("minecraft:{b}")
                }
            })
            .filter(|b| tuff_world_model::chunk::biome_id(b).is_some());
        let noise = |preset: &str, biome_source: Compound| {
            Compound::new()
                .with("settings", preset)
                .with("biome_source", biome_source)
                .with("type", "minecraft:noise")
        };
        let multi_noise = Compound::new()
            .with("preset", OVERWORLD)
            .with("type", "minecraft:multi_noise");
        let generator = match level_type {
            "normal" => return true,
            "amplified" => noise("minecraft:amplified", multi_noise),
            "large_biomes" => noise("minecraft:large_biomes", multi_noise),
            "single_biome_surface" => noise(
                OVERWORLD,
                Compound::new()
                    .with("biome", biome.unwrap_or_else(|| "minecraft:plains".into()))
                    .with("type", "minecraft:fixed"),
            ),
            "flat" => {
                let flat = settings
                    .as_ref()
                    .and_then(FlatSettings::from_json)
                    .unwrap_or_default();
                Compound::new()
                    .with("settings", flat.to_nbt())
                    .with("type", "minecraft:flat")
            }
            "debug_all_block_states" => Compound::new().with("type", "minecraft:debug"),
            "flat_all_dimensions" => {
                if self.new_world {
                    for (key, biome, block, height) in [
                        (OVERWORLD, "minecraft:desert", "minecraft:sandstone", 67),
                        (THE_NETHER, "minecraft:basalt_deltas", "minecraft:basalt", 3),
                        (THE_END, "minecraft:the_end", "minecraft:end_stone", 3),
                    ] {
                        let flat = FlatSettings {
                            biome: biome.into(),
                            layers: vec![("minecraft:bedrock".into(), 1), (block.into(), height)],
                            ..FlatSettings::default()
                        };
                        self.set_dimension_generator(
                            key,
                            Compound::new()
                                .with("settings", flat.to_nbt())
                                .with("type", "minecraft:flat"),
                        );
                    }
                }
                return true;
            }
            _ => return false,
        };
        if self.new_world {
            self.set_dimension_generator(OVERWORLD, generator);
        }
        true
    }

    /// Replaces a just-created world's dimensions with a world preset's
    /// `dimensions` (key to `type` and `generator`). Existing worlds and an
    /// empty preset keep theirs.
    pub fn set_new_world_dimensions(&mut self, dimensions: Compound) {
        if !self.new_world || dimensions.is_empty() {
            return;
        }
        let generate = self.world_gen.get_bool("generate_structures");
        let mut parsed = parse_dimensions(&dimensions, |_| Compound::new());
        for dimension in &mut parsed {
            dimension.generate_structures = generate.unwrap_or(true);
        }
        self.world_gen.insert("dimensions", dimensions);
        self.dimensions = parsed;
    }

    fn set_dimension_generator(&mut self, key: &str, generator: Compound) {
        let Some(entry) = self
            .world_gen
            .get_compound_mut("dimensions")
            .and_then(|d| d.get_compound_mut(key))
        else {
            return;
        };
        entry.insert("generator", generator);
        let entry = entry.clone();
        let parsed = parse_dimensions(&Compound::new().with(key, entry), |_| Compound::new());
        if let (Some(new), Some(old)) = (
            parsed.into_iter().next(),
            self.dimensions.iter_mut().find(|d| d.key == key),
        ) {
            old.generator = new.generator;
            old.settings = new.settings;
            old.biome_source = new.biome_source;
            old.biome_preset = new.biome_preset;
            old.biome = new.biome;
            old.biome_list = new.biome_list;
            old.biome_scale = new.biome_scale;
            old.biome_points = new.biome_points;
            old.flat = new.flat;
        }
    }

    /// Applies the gameplay properties to a world that has just been
    /// created; existing worlds keep their saved values.
    pub fn set_new_world_gameplay(
        &mut self,
        difficulty: Difficulty,
        game_mode: GameMode,
        hardcore: bool,
        pvp: bool,
    ) {
        if !self.new_world {
            return;
        }
        self.difficulty = if hardcore {
            Difficulty::Hard
        } else {
            difficulty
        };
        self.default_game_mode = if hardcore {
            GameMode::Survival
        } else {
            game_mode
        };
        self.data.insert("GameType", self.default_game_mode.id());
        self.data.insert(
            "difficulty_settings",
            Compound::new()
                .with("difficulty", difficulty_name(self.difficulty))
                .with("hardcore", hardcore)
                .with("locked", false),
        );
        let _ = self
            .game_rules
            .set("pvp", crate::world::game_rules::RuleValue::Bool(pvp));
        self.hardcore = hardcore;
        self.difficulty_locked = hardcore;
        self.record_applied_gameplay(hardcore, pvp);
        self.record_applied_difficulty(difficulty);
        self.record_applied("gamemode", Tag::Int(game_mode.id()));
    }

    /// Hardcore forces hard difficulty and locks it; turning it off only
    /// unlocks, leaving the difficulty where it is.
    pub fn set_hardcore(&mut self, hardcore: bool) {
        self.hardcore = hardcore;
        self.difficulty_locked = hardcore;
        if hardcore {
            self.difficulty = Difficulty::Hard;
        }
    }

    /// Applies the hardcore and pvp properties to an existing world when
    /// they differ from what was last applied (a world never launched with
    /// them applies both), so `/gamerule pvp` survives restarts.
    pub fn sync_existing_world_gameplay(&mut self, hardcore: bool, pvp: bool) {
        if self.new_world {
            return;
        }
        let applied = self.data.get_compound("tuff_applied_properties");
        let last_hardcore = applied
            .and_then(|a| tuff_storage::datafix::get_bool(a, "hardcore"))
            .unwrap_or(self.hardcore);
        let last_pvp = applied.and_then(|a| tuff_storage::datafix::get_bool(a, "pvp"));
        if hardcore != last_hardcore || hardcore != self.hardcore {
            self.set_hardcore(hardcore);
        }
        if last_pvp != Some(pvp) {
            let _ = self
                .game_rules
                .set("pvp", crate::world::game_rules::RuleValue::Bool(pvp));
        }
        self.record_applied_gameplay(hardcore, pvp);
    }

    pub fn record_applied_gameplay(&mut self, hardcore: bool, pvp: bool) {
        self.record_applied("hardcore", Tag::from(hardcore));
        self.record_applied("pvp", Tag::from(pvp));
    }

    /// Notes that a server.properties value was applied to this world, so
    /// a later start applies the file's value only when it changed.
    pub fn record_applied(&mut self, key: &str, value: Tag) {
        if self.data.get_compound("tuff_applied_properties").is_none() {
            self.data.insert("tuff_applied_properties", Compound::new());
        }
        if let Some(applied) = self.data.get_compound_mut("tuff_applied_properties") {
            applied.insert(key, value);
        }
    }

    /// Applies the difficulty and gamemode properties to an existing world
    /// when they differ from what was last applied (a world never launched
    /// with them applies both), so `/difficulty` survives restarts. A
    /// locked difficulty stays.
    pub fn sync_existing_world_defaults(&mut self, difficulty: Difficulty, game_mode: GameMode) {
        if self.new_world {
            return;
        }
        let name = difficulty_name(difficulty);
        let applied = self.data.get_compound("tuff_applied_properties");
        let last_difficulty = applied.and_then(|a| a.get_str("difficulty"));
        let last_game_mode = applied.and_then(|a| a.get_int("gamemode"));
        if last_difficulty != Some(name) {
            if !self.difficulty_locked {
                self.difficulty = difficulty;
            }
            self.record_applied("difficulty", Tag::from(name));
        }
        if last_game_mode != Some(game_mode.id()) {
            self.set_default_game_mode(game_mode);
        }
    }

    /// The game mode new players start in; recorded as applied.
    pub fn set_default_game_mode(&mut self, game_mode: GameMode) {
        self.default_game_mode = game_mode;
        self.data.insert("GameType", game_mode.id());
        self.record_applied("gamemode", Tag::Int(game_mode.id()));
    }

    /// Records the difficulty property as applied.
    pub fn record_applied_difficulty(&mut self, difficulty: Difficulty) {
        self.record_applied("difficulty", Tag::from(difficulty_name(difficulty)));
    }

    /// The saved data pack selection; a new world enables `vanilla`.
    pub fn data_packs(&self) -> crate::datapack::DataPackConfig {
        let Some(packs) = self.data.get_compound("DataPacks") else {
            return crate::datapack::DataPackConfig::default();
        };
        let names = |key: &str| -> Vec<String> {
            packs
                .get_list(key)
                .map(|list| {
                    list.iter()
                        .filter_map(|tag| tag.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        crate::datapack::DataPackConfig {
            enabled: names("Enabled"),
            disabled: names("Disabled"),
        }
    }

    /// The world's feature flags (`enabled_features`); `None` for a world
    /// being created, whose flags follow the packs it starts with.
    pub fn enabled_features(&self) -> Option<Vec<String>> {
        if self.new_world {
            return None;
        }
        let flags = self.data.get_list("enabled_features").map(|list| {
            list.iter()
                .filter_map(|tag| tag.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        });
        Some(flags.unwrap_or_else(|| {
            crate::datapack::store::WORLD_FEATURES
                .iter()
                .map(|flag| (*flag).to_owned())
                .collect()
        }))
    }

    pub fn set_enabled_features(&mut self, flags: &[String]) {
        self.data.insert(
            "enabled_features",
            Tag::List(flags.iter().map(|f| Tag::from(f.as_str())).collect()),
        );
    }

    pub fn set_data_packs(&mut self, config: &crate::datapack::DataPackConfig) {
        let list =
            |names: &[String]| Tag::List(names.iter().map(|n| Tag::from(n.as_str())).collect());
        self.data.insert(
            "DataPacks",
            Compound::new()
                .with("Enabled", list(&config.enabled))
                .with("Disabled", list(&config.disabled)),
        );
    }

    /// Loads the world, or describes a new one. `new_seed` is only called for
    /// a new world and `spawn_for` whenever no spawn point is stored.
    pub fn load(
        docs: &dyn Documents,
        new_seed: impl FnOnce() -> i64,
        spawn_for: impl FnOnce(i64) -> BlockPos,
    ) -> io::Result<Self> {
        let data = docs
            .read(Document::Level)?
            .and_then(|mut root| match root.remove("Data") {
                Some(Tag::Compound(data)) => Some(data),
                _ => None,
            })
            .unwrap_or_default();
        let version = data.get_int("DataVersion").unwrap_or(0);
        if version > DATA_VERSION {
            return Err(io::Error::other(format!(
                "world was saved by a newer version (data version {version}, this server supports {DATA_VERSION})"
            )));
        }
        let mut world_gen = inner(docs.read(Document::SavedData(WORLD_GEN_SETTINGS))?);
        let mut new_world = false;
        let seed = match world_gen.get_long("seed") {
            Some(seed) => seed,
            None => {
                new_world = true;
                let seed = new_seed();
                world_gen.insert("seed", seed);
                world_gen.insert("generate_structures", true);
                world_gen.insert("bonus_chest", false);
                world_gen.insert("dimensions", default_dimensions());
                seed
            }
        };
        let bonus_chest_pending = world_gen.get_bool("bonus_chest").unwrap_or(false)
            && !data.get_bool("initialized").unwrap_or(true);
        let clocks_raw = inner(docs.read(Document::SavedData(WORLD_CLOCKS))?);
        let game_time = data.get_long("Time").unwrap_or(0);
        let clocks = WorldClocks::load(&clocks_raw, data.get_long("DayTime").unwrap_or(game_time));
        if world_gen
            .get_compound("dimensions")
            .is_none_or(Compound::is_empty)
        {
            world_gen.insert("dimensions", default_dimensions());
        }
        let mut border_error = None;
        let mut dimensions = parse_dimensions(
            world_gen.get_compound("dimensions").expect("dimensions"),
            |key| match docs.read(Document::DimensionData {
                dimension: key,
                id: WORLD_BORDER,
            }) {
                Ok(root) => inner(root),
                Err(err) => {
                    border_error.get_or_insert(err);
                    Compound::new()
                }
            },
        );
        if let Some(err) = border_error {
            return Err(err);
        }
        let generate_structures = world_gen.get_bool("generate_structures").unwrap_or(true);
        for dimension in &mut dimensions {
            dimension.generate_structures = generate_structures;
        }
        let spawn = data
            .get_compound("spawn")
            .and_then(|spawn| {
                let &[x, y, z] = spawn.get_int_array("pos")? else {
                    return None;
                };
                Some(Spawn {
                    dimension: spawn.get_str("dimension").unwrap_or(OVERWORLD).to_owned(),
                    pos: BlockPos::new(x, y, z),
                    yaw: spawn.get_float("yaw").unwrap_or(0.0),
                    pitch: spawn.get_float("pitch").unwrap_or(0.0),
                })
            })
            .unwrap_or_else(|| Spawn {
                dimension: OVERWORLD.into(),
                pos: spawn_for(seed),
                yaw: 0.0,
                pitch: 0.0,
            });
        let difficulty = data
            .get_compound("difficulty_settings")
            .and_then(|settings| settings.get_str("difficulty"))
            .and_then(parse_difficulty)
            .unwrap_or(Difficulty::Normal);
        let settings = data.get_compound("difficulty_settings");
        let hardcore = settings
            .and_then(|s| tuff_storage::datafix::get_bool(s, "hardcore"))
            .unwrap_or(false);
        let difficulty_locked = settings
            .and_then(|s| tuff_storage::datafix::get_bool(s, "locked"))
            .unwrap_or(false);
        let default_game_mode = data
            .get_int("GameType")
            .and_then(GameMode::from_id)
            .unwrap_or(GameMode::Survival);
        let game_rules = GameRules::load(&inner(docs.read(Document::SavedData(GAME_RULES))?));
        let weather = Weather::load(&inner(docs.read(Document::SavedData(WEATHER))?));
        Ok(Self {
            data,
            world_gen,
            new_world,
            bonus_chest_pending,
            clocks_raw,
            seed,
            game_time,
            clocks,
            spawn,
            difficulty,
            hardcore,
            difficulty_locked,
            default_game_mode,
            dimensions,
            game_rules,
            weather,
        })
    }

    pub fn dimension(&self, key: &str) -> Option<&DimensionSettings> {
        self.dimensions.iter().find(|d| d.key == key)
    }

    /// Writes `level.dat` and the saved data files this server manages.
    pub fn save(&self, docs: &dyn Documents, level_name: &str, brand: &str) -> io::Result<()> {
        let mut data = self.data.clone();
        let mut history: Vec<Tag> = data.get_list("version_history").unwrap_or(&[]).to_vec();
        if history.last().and_then(Tag::as_int) != Some(DATA_VERSION) {
            history.push(Tag::Int(DATA_VERSION));
        }
        let mut brands: Vec<Tag> = data.get_list("ServerBrands").unwrap_or(&[]).to_vec();
        if !brands.iter().any(|b| b.as_str() == Some(brand)) {
            brands.push(Tag::from(brand));
        }
        let last_played = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let defaults = [
            ("LevelName", Tag::from(level_name)),
            ("GameType", Tag::Int(0)),
            ("allowCommands", Tag::from(false)),
            (
                "difficulty_settings",
                Tag::Compound(
                    Compound::new()
                        .with("difficulty", difficulty_name(self.difficulty))
                        .with("hardcore", false)
                        .with("locked", false),
                ),
            ),
            (
                "DataPacks",
                Tag::Compound(
                    Compound::new()
                        .with("Enabled", Tag::List(vec![Tag::from("vanilla")]))
                        .with("Disabled", Tag::List(Vec::new())),
                ),
            ),
        ];
        for (key, value) in defaults {
            if !data.contains_key(key) {
                data.insert(key, value);
            }
        }
        if let Some(settings) = data.get_compound_mut("difficulty_settings") {
            settings.insert("difficulty", difficulty_name(self.difficulty));
            settings.insert("hardcore", self.hardcore);
            settings.insert("locked", self.difficulty_locked);
        }
        data.insert("DataVersion", DATA_VERSION);
        data.insert("version", ANVIL_VERSION);
        data.insert(
            "Version",
            Compound::new()
                .with("Snapshot", !STABLE)
                .with("Series", SERIES)
                .with("Id", DATA_VERSION)
                .with("Name", NAME),
        );
        data.insert("version_history", Tag::List(history));
        data.insert("ServerBrands", Tag::List(brands));
        data.insert("WasModded", true);
        data.insert("Time", self.game_time);
        data.insert("LastPlayed", last_played);
        data.insert("initialized", true);
        data.insert(
            "spawn",
            Compound::new()
                .with(
                    "pos",
                    vec![self.spawn.pos.x, self.spawn.pos.y, self.spawn.pos.z],
                )
                .with("pitch", self.spawn.pitch)
                .with("dimension", self.spawn.dimension.as_str())
                .with("yaw", self.spawn.yaw),
        );
        docs.write(Document::Level, Compound::new().with("Data", data))?;
        docs.write(
            Document::SavedData(WORLD_GEN_SETTINGS),
            saved(self.world_gen.clone()),
        )?;

        let mut clocks = self.clocks_raw.clone();
        self.clocks.save_into(&mut clocks);
        docs.write(Document::SavedData(WORLD_CLOCKS), saved(clocks))?;
        docs.write(
            Document::SavedData(GAME_RULES),
            saved(self.game_rules.save()),
        )?;
        docs.write(Document::SavedData(WEATHER), saved(self.weather.save()))?;

        for dimension in &self.dimensions {
            let mut border = dimension.border_raw.clone();
            dimension.border.save_into(&mut border);
            docs.write(
                Document::DimensionData {
                    dimension: &dimension.key,
                    id: WORLD_BORDER,
                },
                saved(border),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "level_tests.rs"]
mod tests;
