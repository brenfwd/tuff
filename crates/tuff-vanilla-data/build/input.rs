//! Parses the data generator reports and the data pack embedded in the server
//! jar into plain structures for the code generators.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use zip::ZipArchive;

use crate::fetch::Vanilla;

pub struct Input {
    pub version: VersionInfo,
    /// state -> direction -> packet name -> ID
    pub packets: BTreeMap<String, BTreeMap<String, BTreeMap<String, ProtocolId>>>,
    pub registries: BTreeMap<String, RegistryReport>,
    pub blocks: BTreeMap<String, BlockReport>,
    /// Item name -> default data components.
    pub item_components: BTreeMap<String, Map<String, Value>>,
    pub datapack: DatapackReport,
    pub commands: CommandNode,
    /// Every file path under `data/` in the server jar, e.g.
    /// `minecraft/tags/block/logs.json`.
    pub data_files: Vec<String>,
    /// The first 8 bytes (big-endian) of the SHA-1 of every JSON file under
    /// `data/`, by the same path as `data_files`.
    pub data_hashes: BTreeMap<String, u64>,
    /// Parsed contents of the data files the generators read.
    pub data_json: BTreeMap<String, Value>,
    /// The JSON files of the built-in feature packs
    /// (`minecraft/datapacks/<pack>/...`), kept apart from `data_json` so
    /// no generator mistakes them for the base game's.
    pub feature_pack_json: BTreeMap<String, Value>,
    /// `LightProperties.java` output (see `codegen/light.rs`).
    pub light: String,
    /// `BlockProperties.java` output (see `codegen/block_properties.rs`).
    pub block_properties: String,
    /// `ItemProperties.java` output (see `codegen/item_properties.rs`).
    pub item_properties: String,
    /// `GameRuleProperties.java` output (see `codegen/game_rules.rs`).
    pub game_rules: String,
    /// `WorldProperties.java` output (see `codegen/world.rs`).
    pub world_properties: String,
    /// `EntityProperties.java` output (see `codegen/entities.rs`).
    pub entity_properties: String,
    /// `AttributeProperties.java` output (see `codegen/attributes.rs`).
    pub attribute_properties: String,
    /// `BlockEntityProperties.java` output (see `codegen/block_entities.rs`).
    pub block_entity_properties: String,
    pub fluid_properties: String,
    /// `DragonProperties.java` output (see `codegen/dragon.rs`).
    pub dragon_properties: String,
    /// `GrowthProperties.java` output (see `codegen/growth.rs`).
    pub growth_properties: String,
    /// `PotionProperties.java` output (see `codegen/potions.rs`).
    pub potion_properties: String,
    /// `VillagerProperties.java` output (see `codegen/villagers.rs`).
    pub villager_properties: String,
    /// `RedstoneProperties.java` output (see `codegen/redstone.rs`).
    pub redstone_properties: String,
    /// `MapProperties.java` output (see `codegen/maps.rs`).
    pub map_properties: String,
    /// `NetworkRegistries.java` output (see `codegen/data_registries.rs`).
    pub network_registries: String,
    /// The multi-noise parameter list reports (`reports/biome_parameters`),
    /// by preset name.
    pub biome_parameters: BTreeMap<String, Value>,
    /// Raw bytes of the structure templates (`data/<ns>/structure/**.nbt`),
    /// keyed by their data path.
    pub structure_templates: BTreeMap<String, Vec<u8>>,
    /// The `en_us` client language file (`assets/minecraft/lang/en_us.json`):
    /// translation key -> format string.
    pub lang: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandNode {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub children: BTreeMap<String, CommandNode>,
    pub redirect: Option<Vec<String>>,
    pub parser: Option<String>,
    pub properties: Option<Value>,
    pub permissions: Option<Value>,
}

#[derive(Deserialize)]
pub struct VersionInfo {
    pub id: String,
    pub name: String,
    pub build_time: String,
    pub world_version: i32,
    pub series_id: String,
    pub stable: bool,
    pub protocol_version: i32,
    pub pack_version: PackVersion,
}

#[derive(Deserialize)]
pub struct PackVersion {
    pub resource_major: i32,
    pub resource_minor: i32,
    pub data_major: i32,
    pub data_minor: i32,
}

#[derive(Deserialize)]
pub struct ProtocolId {
    pub protocol_id: u32,
}

#[derive(Deserialize)]
pub struct RegistryReport {
    pub protocol_id: u32,
    pub default: Option<String>,
    pub entries: BTreeMap<String, ProtocolId>,
}

#[derive(Deserialize)]
pub struct BlockReport {
    #[serde(default)]
    pub properties: BTreeMap<String, Vec<String>>,
    pub states: Vec<BlockStateReport>,
}

#[derive(Deserialize)]
pub struct BlockStateReport {
    pub id: u32,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub struct DatapackReport {
    pub registries: BTreeMap<String, DatapackRegistry>,
}

#[derive(Deserialize)]
pub struct DatapackRegistry {
    pub elements: bool,
    pub tags: bool,
    pub stable: bool,
}

impl Input {
    pub fn load(version: &str, vanilla: &Vanilla) -> Result<Self> {
        let reports = &vanilla.reports;
        let mut outer = ZipArchive::new(File::open(&vanilla.server_jar)?)?;
        let version_info: VersionInfo =
            serde_json::from_slice(&read_entry(&mut outer, "version.json")?)?;
        ensure!(
            version_info.name == version,
            "server jar reports version {}, expected {version}",
            version_info.name
        );

        let mut input = Self {
            version: version_info,
            packets: read_json(&reports.join("packets.json"))?,
            registries: read_json(&reports.join("registries.json"))?,
            blocks: read_json(&reports.join("blocks.json"))?,
            item_components: read_item_components(reports)?,
            datapack: read_json(&reports.join("datapack.json"))?,
            commands: read_json(&reports.join("commands.json"))?,
            data_files: Vec::new(),
            data_hashes: BTreeMap::new(),
            data_json: BTreeMap::new(),
            feature_pack_json: BTreeMap::new(),
            light: fs::read_to_string(&vanilla.light)
                .with_context(|| format!("failed to read {}", vanilla.light.display()))?,
            block_properties: fs::read_to_string(&vanilla.block_properties).with_context(|| {
                format!("failed to read {}", vanilla.block_properties.display())
            })?,
            item_properties: fs::read_to_string(&vanilla.item_properties)
                .with_context(|| format!("failed to read {}", vanilla.item_properties.display()))?,
            game_rules: fs::read_to_string(&vanilla.game_rules)
                .with_context(|| format!("failed to read {}", vanilla.game_rules.display()))?,
            world_properties: fs::read_to_string(&vanilla.world_properties).with_context(|| {
                format!("failed to read {}", vanilla.world_properties.display())
            })?,
            entity_properties: fs::read_to_string(&vanilla.entity_properties).with_context(
                || format!("failed to read {}", vanilla.entity_properties.display()),
            )?,
            attribute_properties: fs::read_to_string(&vanilla.attribute_properties).with_context(
                || format!("failed to read {}", vanilla.attribute_properties.display()),
            )?,
            block_entity_properties: fs::read_to_string(&vanilla.block_entity_properties)
                .with_context(|| {
                    format!(
                        "failed to read {}",
                        vanilla.block_entity_properties.display()
                    )
                })?,
            fluid_properties: fs::read_to_string(&vanilla.fluid_properties).with_context(|| {
                format!("failed to read {}", vanilla.fluid_properties.display())
            })?,
            dragon_properties: fs::read_to_string(&vanilla.dragon_properties).with_context(
                || format!("failed to read {}", vanilla.dragon_properties.display()),
            )?,
            growth_properties: fs::read_to_string(&vanilla.growth_properties).with_context(
                || format!("failed to read {}", vanilla.growth_properties.display()),
            )?,
            potion_properties: fs::read_to_string(&vanilla.potion_properties).with_context(
                || format!("failed to read {}", vanilla.potion_properties.display()),
            )?,
            villager_properties: fs::read_to_string(&vanilla.villager_properties).with_context(
                || format!("failed to read {}", vanilla.villager_properties.display()),
            )?,
            redstone_properties: fs::read_to_string(&vanilla.redstone_properties).with_context(
                || format!("failed to read {}", vanilla.redstone_properties.display()),
            )?,
            map_properties: fs::read_to_string(&vanilla.map_properties)
                .with_context(|| format!("failed to read {}", vanilla.map_properties.display()))?,
            network_registries: fs::read_to_string(&vanilla.network_registries).with_context(
                || format!("failed to read {}", vanilla.network_registries.display()),
            )?,
            biome_parameters: read_biome_parameters(reports)?,
            structure_templates: BTreeMap::new(),
            lang: BTreeMap::new(),
        };
        input.read_data_pack(&mut outer)?;
        Ok(input)
    }

    fn read_data_pack(&mut self, outer: &mut ZipArchive<File>) -> Result<()> {
        let versions = String::from_utf8(read_entry(outer, "META-INF/versions.list")?)?;
        let inner_path = versions
            .lines()
            .find_map(|line| line.split('\t').nth(2))
            .context("META-INF/versions.list is empty")?;
        let inner = read_entry(outer, &format!("META-INF/versions/{inner_path}"))?;
        let mut jar = ZipArchive::new(Cursor::new(inner))?;
        let lang_bytes = read_entry(&mut jar, "assets/minecraft/lang/en_us.json")?;
        self.lang =
            serde_json::from_slice(&lang_bytes).context("assets/minecraft/lang/en_us.json")?;
        if let Ok(bytes) = read_entry(&mut jar, "assets/minecraft/lang/deprecated.json") {
            let deprecated: DeprecatedLang =
                serde_json::from_slice(&bytes).context("assets/minecraft/lang/deprecated.json")?;
            deprecated.apply(&mut self.lang);
        }
        for index in 0..jar.len() {
            let mut file = jar.by_index(index)?;
            let Some(path) = file.name().strip_prefix("data/").map(str::to_owned) else {
                continue;
            };
            if file.is_dir() {
                continue;
            }
            let is_template = path.ends_with(".nbt")
                && path
                    .split_once('/')
                    .is_some_and(|(_, r)| r.starts_with("structure/"));
            let is_json = path.ends_with(".json");
            if is_template || is_json || is_feature_pack_meta(&path) {
                let mut bytes = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut bytes)?;
                if is_json {
                    self.data_hashes.insert(path.clone(), short_sha1(&bytes));
                }
                if is_template {
                    self.structure_templates.insert(path.clone(), bytes);
                } else if (is_json && wants_contents(&path)) || is_feature_pack_meta(&path) {
                    let value =
                        serde_json::from_slice(&bytes).with_context(|| format!("data/{path}"))?;
                    self.data_json.insert(path.clone(), value);
                } else if is_json && path.starts_with(FEATURE_PACKS) {
                    let value =
                        serde_json::from_slice(&bytes).with_context(|| format!("data/{path}"))?;
                    self.feature_pack_json.insert(path.clone(), value);
                }
            }
            self.data_files.push(path);
        }
        self.data_files.sort();
        Ok(())
    }

    /// The condition stored at `data/<namespace>/predicate/<path>.json`, the
    /// file a bare resource location in a loot table `condition` refers to.
    pub fn predicate(&self, name: &str) -> Result<&Value> {
        let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
        let key = format!("{namespace}/predicate/{path}.json");
        self.data_json
            .get(&key)
            .with_context(|| format!("no predicate named {name}"))
    }
}

/// Where the built-in feature packs live below `data/`.
pub const FEATURE_PACKS: &str = "minecraft/datapacks/";

/// `minecraft/datapacks/<name>/pack.mcmeta`: a built-in feature pack.
fn is_feature_pack_meta(path: &str) -> bool {
    path.strip_prefix("minecraft/datapacks/")
        .and_then(|rest| rest.strip_suffix("/pack.mcmeta"))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// The first 8 bytes of the SHA-1 of `bytes`, big-endian.
pub fn short_sha1(bytes: &[u8]) -> u64 {
    use sha1::Digest;
    let digest = sha1::Sha1::digest(bytes);
    u64::from_be_bytes(digest[..8].try_into().expect("20-byte digest"))
}

fn wants_contents(path: &str) -> bool {
    let Some((_namespace, rest)) = path.split_once('/') else {
        return false;
    };
    const PREFIXES: &[&str] = &[
        "tags/",
        "dimension_type/",
        "damage_type/",
        "worldgen/",
        "recipe/",
        "loot_table/",
        "predicate/",
        "timeline/",
        "advancement/",
        "context_int_provider/",
        "context_float_provider/",
        "enchantment/",
        "block_transformer/",
        "cow_variant/",
        "pig_variant/",
        "chicken_variant/",
        "cow_sound_variant/",
        "pig_sound_variant/",
        "chicken_sound_variant/",
        "wolf_variant/",
        "wolf_sound_variant/",
        "cat_variant/",
        "cat_sound_variant/",
        "frog_variant/",
        "villager_trade/",
        "trade_set/",
        "jukebox_song/",
        "trial_spawner/",
        "sulfur_cube_archetype/",
        "chat_type/",
        "trim_pattern/",
        "trim_material/",
        "zombie_nautilus_variant/",
        "painting_variant/",
        "banner_pattern/",
        "instrument/",
        "test_environment/",
        "test_instance/",
        "dialog/",
        "world_clock/",
        "decorated_pot_pattern/",
    ];
    PREFIXES.iter().any(|prefix| rest.starts_with(prefix))
}

fn read_biome_parameters(reports: &Path) -> Result<BTreeMap<String, Value>> {
    let mut lists = BTreeMap::new();
    let root = reports.join("biome_parameters");
    for namespace in fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))? {
        let namespace = namespace?;
        let ns = namespace.file_name().to_string_lossy().into_owned();
        for file in fs::read_dir(namespace.path())? {
            let path = file?.path();
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            lists.insert(format!("{ns}:{name}"), read_json(&path)?);
        }
    }
    Ok(lists)
}

fn read_item_components(reports: &Path) -> Result<BTreeMap<String, Map<String, Value>>> {
    #[derive(Deserialize)]
    struct Report {
        components: Map<String, Value>,
    }

    let mut items = BTreeMap::new();
    for namespace in fs::read_dir(reports)? {
        let namespace = namespace?;
        let dir = namespace.path().join("components").join("item");
        if !dir.is_dir() {
            continue;
        }
        let namespace = namespace.file_name().to_string_lossy().into_owned();
        for file in fs::read_dir(&dir)? {
            let path = file?.path();
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let report: Report = read_json(&path)?;
            items.insert(format!("{namespace}:{name}"), report.components);
        }
    }
    Ok(items)
}

#[derive(Deserialize)]
struct DeprecatedLang {
    removed: Vec<String>,
    renamed: BTreeMap<String, String>,
}

impl DeprecatedLang {
    fn apply(&self, lang: &mut BTreeMap<String, String>) {
        for key in &self.removed {
            lang.remove(key);
        }
        for (from, to) in &self.renamed {
            match lang.remove(from) {
                Some(value) => {
                    lang.insert(to.clone(), value);
                }
                None => {
                    lang.remove(to);
                }
            }
        }
    }
}

fn read_entry<R: Read + Seek>(archive: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .with_context(|| format!("{name} not found in server jar"))?;
    let mut bytes = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}
