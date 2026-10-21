//! Mob spawner blocks (dungeons, fortress blaze spawners) and the spawn
//! lists structures impose on natural spawning (blazes in fortresses).
//!
//! Spawners stay saved block entity NBT; a region finds them among its
//! chunks' raw block entities every second and ticks them every tick:
//! while a player is in range they count down and then spawn a few of
//! their mob around themselves, up to a crowd limit.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use rand::Rng;
use serde_json::Value;
use tuff_vanilla_data::biome::Spawner;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::registry::EntityType;

use super::spawning::rule_holds;
use crate::physics;
use crate::spawning::{self, SpawnReason};
use tuff_core::world::messages::RegionInternal;
use tuff_core::world::player::Difficulty;
use tuff_core::world::region_api::{RegionTask, RegionWorld};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const SPAWNER_ID: &str = "minecraft:mob_spawner";
const SPAWNER_PARTICLES: i32 = 2004;
/// Spawner blocks are looked for again this often.
const RESCAN_TICKS: u64 = 20;

/// Where a spawner's settings live: a block entity, or a spawner
/// minecart's saved data.
#[derive(Clone, Copy)]
enum Source {
    Block(BlockPos),
    Cart(i32),
}

impl Source {
    fn pos(self, world: &dyn RegionWorld) -> Option<BlockPos> {
        match self {
            Self::Block(pos) => Some(pos),
            Self::Cart(id) => world
                .entity(id)
                .map(|t| physics::block_pos(t.entity.base().position)),
        }
    }

    fn nbt(self, world: &mut dyn RegionWorld) -> Option<&mut Compound> {
        match self {
            Self::Block(pos) => spawner_nbt(world, pos),
            Self::Cart(id) => world.entity_mut(id).map(|t| &mut t.entity.base_mut().extra),
        }
    }
}

/// Spawners among a region's loaded chunks and entities, found every
/// second.
#[derive(Default)]
struct Spawners(Vec<Source>);

/// A spawner's settings, with the game's defaults for missing ones.
struct Settings {
    delay: i32,
    min_delay: i32,
    max_delay: i32,
    count: i32,
    max_nearby: i32,
    player_range: i32,
    range: i32,
}

fn int(nbt: &Compound, key: &str, default: i32) -> i32 {
    match nbt.get(key) {
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Int(v)) => *v,
        Some(Tag::Byte(v)) => i32::from(*v),
        _ => default,
    }
}

impl Settings {
    fn read(nbt: &Compound) -> Self {
        Self {
            delay: int(nbt, "Delay", 20),
            min_delay: int(nbt, "MinSpawnDelay", 200),
            max_delay: int(nbt, "MaxSpawnDelay", 800),
            count: int(nbt, "SpawnCount", 4),
            max_nearby: int(nbt, "MaxNearbyEntities", 6),
            player_range: int(nbt, "RequiredPlayerRange", 16),
            range: int(nbt, "SpawnRange", 4),
        }
    }
}

fn is_spawner(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(SPAWNER_ID)
}

fn spawner_nbt(world: &mut dyn RegionWorld, pos: BlockPos) -> Option<&mut Compound> {
    world
        .raw_block_entity_mut(pos)
        .filter(|nbt| is_spawner(nbt))
}

/// A spawn egg used on the spawner block at `pos`: its next spawn becomes
/// `kind`, or the egg's whole entity data when an operator used it.
/// Returns whether `pos` holds a spawner.
pub fn retarget(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    kind: EntityType,
    entity_data: Option<&Compound>,
) -> bool {
    let id = format!("minecraft:{}", kind.name());
    let Some(nbt) = spawner_nbt(world, pos) else {
        return false;
    };
    let mut spawn_data = nbt.get_compound("SpawnData").cloned().unwrap_or_default();
    let entity = match entity_data {
        Some(data) => {
            let mut data = data.clone();
            data.insert("id", id);
            data
        }
        None => {
            let mut entity = spawn_data
                .get_compound("entity")
                .cloned()
                .unwrap_or_default();
            entity.insert("id", id);
            entity
        }
    };
    spawn_data.insert("entity", entity);
    nbt.insert("SpawnData", spawn_data);
    world.block_entity_changed(pos);
    true
}

pub fn tick_spawners(world: &mut dyn RegionWorld) {
    if !world
        .shared()
        .game_rules
        .borrow()
        .bool("spawner_blocks_work")
    {
        return;
    }
    if world.tick().is_multiple_of(RESCAN_TICKS) {
        let mut blocks = world.raw_block_entities_where(&is_spawner);
        blocks.sort_unstable_by_key(|p| (p.y, p.z, p.x));
        let mut carts: Vec<i32> = world
            .entity_ids()
            .into_iter()
            .filter(|id| {
                world
                    .entity(*id)
                    .is_some_and(|t| t.entity.base().kind == EntityType::SpawnerMinecart)
            })
            .collect();
        carts.sort_unstable();
        let found = blocks
            .into_iter()
            .map(Source::Block)
            .chain(carts.into_iter().map(Source::Cart))
            .collect();
        world.state().get::<Spawners>().0 = found;
    }
    let positions = std::mem::take(&mut world.state().get::<Spawners>().0);
    for &source in &positions {
        tick_spawner(world, source);
    }
    world.state().get::<Spawners>().0 = positions;
}

fn tick_spawner(world: &mut dyn RegionWorld, source: Source) {
    let (Some(pos), Some(nbt)) = (source.pos(world), source.nbt(world)) else {
        return;
    };
    let settings = Settings::read(nbt);
    let center = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    ];
    let range = f64::from(settings.player_range);
    let near = world
        .seen_player_positions()
        .iter()
        .any(|p| tuff_core::world::distance_sq(*p, center) < range * range);
    if !near {
        return;
    }
    if settings.delay == -1 {
        reset_spawner(world, source, &settings);
        return;
    }
    if settings.delay > 0 {
        if let Some(nbt) = source.nbt(world) {
            nbt.insert("Delay", (settings.delay - 1) as i16);
        }
        return;
    }
    let mut spawned = false;
    for _ in 0..settings.count {
        match spawner_attempt(world, source, &settings) {
            Attempt::Spawned => spawned = true,
            Attempt::Crowded => {
                reset_spawner(world, source, &settings);
                return;
            }
            Attempt::Failed => {}
        }
    }
    if spawned {
        reset_spawner(world, source, &settings);
    }
}

fn spawner_attempt(world: &mut dyn RegionWorld, source: Source, settings: &Settings) -> Attempt {
    let Some(pos) = source.pos(world) else {
        return Attempt::Failed;
    };
    let Some((data, rules, equipment)) = source.nbt(world).and_then(|nbt| {
        let spawn_data = nbt.get("SpawnData")?.as_compound()?;
        Some((
            spawn_data.get("entity")?.as_compound()?.clone(),
            spawn_data.get_compound("custom_spawn_rules").cloned(),
            spawn_data.get_compound("equipment").cloned(),
        ))
    }) else {
        return Attempt::Failed;
    };
    let Some(kind) = data.get_str("id").and_then(EntityType::from_name) else {
        return Attempt::Failed;
    };
    let range = f64::from(settings.range);
    let at = fixed_position(&data).unwrap_or_else(|| {
        let rng = world.rng();
        let x = f64::from(pos.x) + (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * range + 0.5;
        let y = f64::from(pos.y + rng.gen_range(0..3) - 1);
        let z = f64::from(pos.z) + (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * range + 0.5;
        [x, y, z]
    });
    if !physics::no_collision(spawning::spawn_box(kind, at), &|p| world.block(p)) {
        return Attempt::Failed;
    }
    let shared = world.shared().clone();
    let peaceful = *shared.difficulty.borrow() == Difficulty::Peaceful;
    if peaceful && !kind.category().info().friendly {
        return Attempt::Failed;
    }
    let block_pos = physics::block_pos(at);
    match &rules {
        Some(rules) => {
            if !light_rules_hold(world, rules, block_pos) {
                return Attempt::Failed;
            }
        }
        None => {
            if let Some(rule) = shared.entity_types.spawn_rule(kind) {
                let seed = world.rng().r#gen();
                if !rule_holds(world, seed, kind, block_pos, rule, SpawnReason::Spawner) {
                    return Attempt::Failed;
                }
            }
        }
    }
    let area = [
        f64::from(pos.x) - range,
        f64::from(pos.y) - range,
        f64::from(pos.z) - range,
        f64::from(pos.x) + 1.0 + range,
        f64::from(pos.y) + 1.0 + range,
        f64::from(pos.z) + 1.0 + range,
    ];
    let nearby = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .filter(|t| t.entity.base().kind == kind && physics::intersects(t.entity.bounds(), area))
        .count() as i32;
    if nearby >= settings.max_nearby {
        return Attempt::Crowded;
    }
    let fresh = data.len() == 1;
    let yaw = world.rng().r#gen::<f32>() * 360.0;
    let mut saved = data;
    saved.insert("Pos", Tag::List(at.map(Tag::Double).to_vec()));
    saved.insert(
        "Rotation",
        Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
    );
    if let Some(equipment) = &equipment {
        tuff_vanilla_common::spawn_equipment::equip(world, equipment, &mut saved);
    }
    let id = shared.next_entity_id();
    let Some(mut entity) = shared.entity_types.load(&saved, id) else {
        return Attempt::Failed;
    };
    if !spawning::unobstructed_for(entity.base().kind, entity.bounds(), &|p| world.block(p)) {
        return Attempt::Failed;
    }
    if fresh {
        world.finalize_spawn(&mut entity, 0);
    }
    world.add_entity(entity);
    world.level_event(SPAWNER_PARTICLES, pos, 0);
    Attempt::Spawned
}

/// The `Pos` a spawn entry pins its mob to, in place of a random spot.
fn fixed_position(data: &Compound) -> Option<[f64; 3]> {
    let list = data.get("Pos")?.as_list()?;
    let mut at = [0.0; 3];
    if list.len() != 3 {
        return None;
    }
    for (slot, tag) in at.iter_mut().zip(list) {
        *slot = match tag {
            Tag::Double(v) => *v,
            Tag::Float(v) => f64::from(*v),
            Tag::Int(v) => f64::from(*v),
            _ => return None,
        };
    }
    Some(at)
}

/// Whether the light at `pos` is within a spawn entry's
/// `custom_spawn_rules` (`block_light_limit`, `sky_light_limit`: an exact
/// level or an inclusive range), which stand in for the mob's own rules.
pub fn light_rules_hold(world: &dyn RegionWorld, rules: &Compound, pos: BlockPos) -> bool {
    let within = |tag: Option<&Tag>, value: i32| match tag {
        Some(Tag::Compound(range)) => {
            let low = range.get_int("min_inclusive").unwrap_or(0);
            let high = range.get_int("max_inclusive").unwrap_or(15);
            (low..=high).contains(&value)
        }
        Some(Tag::Int(exact)) => *exact == value,
        _ => true,
    };
    let (sky, block) = world.engine_light(pos).unwrap_or((0, 0));
    let darken = tuff_core::world::sky_darken(world.shared(), world.dimension_info());
    let sky = (i32::from(sky) - i32::from(darken)).max(0);
    within(rules.get("block_light_limit"), i32::from(block))
        && within(rules.get("sky_light_limit"), sky)
}

/// A new countdown and the next mob to spawn, weighted from the spawner's
/// potentials.
fn reset_spawner(world: &mut dyn RegionWorld, source: Source, settings: &Settings) {
    let delay = if settings.max_delay <= settings.min_delay {
        settings.min_delay
    } else {
        settings.min_delay
            + world
                .rng()
                .gen_range(0..settings.max_delay - settings.min_delay)
    };
    let roll = world.rng().r#gen::<u32>();
    let Some(nbt) = source.nbt(world) else {
        return;
    };
    nbt.insert("Delay", delay as i16);
    let potentials: Vec<(i32, Compound)> = nbt
        .get("SpawnPotentials")
        .and_then(Tag::as_list)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.as_compound()?;
            Some((
                int(entry, "weight", 1),
                entry.get("data")?.as_compound()?.clone(),
            ))
        })
        .collect();
    let total: i32 = potentials.iter().map(|(w, _)| (*w).max(0)).sum();
    let mut changed = false;
    if total > 0 {
        let mut pick = (roll % total as u32) as i32;
        for (weight, data) in potentials {
            pick -= weight.max(0);
            if pick < 0 {
                changed = nbt.get_compound("SpawnData") != Some(&data);
                nbt.insert("SpawnData", data);
                break;
            }
        }
    }
    match source {
        Source::Block(pos) => {
            if changed {
                world.block_entity_changed(pos);
            }
            world.block_event(pos, 1, 0);
        }
        Source::Cart(id) => {
            let Some(position) = world.entity(id).map(|t| t.entity.base().position) else {
                return;
            };
            let event = tuff_core::world::framed(&tuff_core::protocol::play::EntityEvent {
                entity_id: id,
                status: 1,
            });
            world.send_to_chunk(
                tuff_world_model::pos::ChunkPos::of_position(position),
                None,
                tuff_core::world::packets([event]),
            );
        }
    }
}

/// The spawn list at `pos`: the fortress's monsters on its nether bricks
/// anywhere in its box, else a structure's override for `category` when
/// `pos` lies in one (in a piece of it, or its whole box), else the
/// biome's.
pub fn spawn_list(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    biome: &str,
    category: MobCategory,
) -> &'static [Spawner] {
    if let Some(list) = structure_spawns(world, pos, category) {
        return list;
    }
    let overlay = world.dimension_info().environment().natural_spawns;
    if let Some(over) = overlay.filter(|over| over.defines(category)) {
        return over.spawners(category);
    }
    if let Some(list) = world.shared().dimensions.biomes().spawners(biome, category) {
        return list;
    }
    match tuff_vanilla_data::biome::Biome::get(biome) {
        Some(b) => b.natural_spawns.spawners_under(overlay, category),
        None => overlay.map_or(&[], |over| over.spawners(category)),
    }
}

const FORTRESS: &str = "minecraft:fortress";
/// A neighbour asked for a structure start that it had not loaded is
/// asked again after this long.
const START_RETRY_TICKS: u64 = 100;
const MAX_REMOTE_STARTS: usize = 1024;

/// Pieces of structure starts in neighbour regions, as they answered.
/// Starts never change once generated, so answers are kept.
#[derive(Default)]
struct RemoteStarts(HashMap<(ChunkPos, String), RemoteStart>);

enum RemoteStart {
    Asked(u64),
    Known(Arc<[[i32; 6]]>),
}

fn structure_spawns(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    category: MobCategory,
) -> Option<&'static [Spawner]> {
    let references: Vec<(String, Vec<ChunkPos>)> = world
        .chunk_extra(ChunkPos::of_block(pos))?
        .get("structures")?
        .as_compound()?
        .get("References")?
        .as_compound()?
        .iter()
        .filter_map(|(name, starts)| match starts {
            Tag::LongArray(values) => Some((
                name.to_owned(),
                values
                    .iter()
                    .map(|packed| ChunkPos::new(*packed as i32, (*packed >> 32) as i32))
                    .collect(),
            )),
            _ => None,
        })
        .collect();
    if category == MobCategory::Monster
        && world
            .block(pos.offset(tuff_world_model::Face::Down))
            .is_some_and(|below| below.block() == tuff_vanilla_data::Block::NetherBricks)
        && let Some((_, list)) = overrides(FORTRESS, category)
        && let Some((_, starts)) = references.iter().find(|(name, _)| name == FORTRESS)
        && starts
            .iter()
            .any(|start| inside_start(world, *start, FORTRESS, pos, false))
    {
        return Some(list);
    }
    for (name, starts) in &references {
        let Some((by_piece, list)) = overrides(name, category) else {
            continue;
        };
        for start in starts {
            if inside_start(world, *start, name, pos, by_piece) {
                return Some(list);
            }
        }
    }
    None
}

/// The piece boxes of the start of structure `name` in chunk `start`,
/// empty when that chunk has no such start.
fn start_pieces(world: &dyn RegionWorld, start: ChunkPos, name: &str) -> Option<Vec<[i32; 6]>> {
    let structures = world
        .chunk_extra(start)?
        .get("structures")
        .and_then(Tag::as_compound);
    let Some(start) =
        structures.and_then(|s| s.get("starts")?.as_compound()?.get(name)?.as_compound())
    else {
        return Some(Vec::new());
    };
    Some(
        start
            .get("Children")
            .and_then(Tag::as_list)
            .into_iter()
            .flatten()
            .filter_map(|piece| match piece.as_compound()?.get("BB")? {
                Tag::IntArray(bb) if bb.len() == 6 => {
                    Some([bb[0], bb[1], bb[2], bb[3], bb[4], bb[5]])
                }
                _ => None,
            })
            .collect(),
    )
}

/// The piece boxes of a start, read here when this region owns its chunk
/// and otherwise from what the owning neighbour answered; a start not
/// known yet is asked for and reads as no pieces until the answer lands.
fn pieces(world: &mut dyn RegionWorld, start: ChunkPos, name: &str) -> Arc<[[i32; 6]]> {
    if world.owns_chunk(start) {
        return start_pieces(world, start, name).unwrap_or_default().into();
    }
    let now = world.tick();
    let key = (start, name.to_owned());
    match world.state().get::<RemoteStarts>().0.get(&key) {
        Some(RemoteStart::Known(boxes)) => return boxes.clone(),
        Some(RemoteStart::Asked(at)) if now.saturating_sub(*at) < START_RETRY_TICKS => {
            return Arc::from([]);
        }
        _ => {}
    }
    let Some(owner) = world.neighbor(start.region()) else {
        return Arc::from([]);
    };
    let home = world.handle();
    let remote = world.state().get::<RemoteStarts>();
    if remote.0.len() >= MAX_REMOTE_STARTS {
        remote.0.clear();
    }
    remote.0.insert(key.clone(), RemoteStart::Asked(now));
    owner.internal(RegionInternal::Run(RegionTask::new(move |other| {
        let Some(boxes) = start_pieces(other, start, &key.1) else {
            return;
        };
        home.internal(RegionInternal::Run(RegionTask::new(move |here| {
            here.state()
                .get::<RemoteStarts>()
                .0
                .insert(key, RemoteStart::Known(boxes.into()));
        })));
    })));
    Arc::from([])
}

/// Whether `pos` is in the start of structure `name` at chunk `start`: in
/// one of its pieces, or anywhere in its box.
fn inside_start(
    world: &mut dyn RegionWorld,
    start: ChunkPos,
    name: &str,
    pos: BlockPos,
    by_piece: bool,
) -> bool {
    let boxes = pieces(world, start, name);
    let inside = |b: &[i32; 6]| {
        (b[0]..=b[3]).contains(&pos.x)
            && (b[1]..=b[4]).contains(&pos.y)
            && (b[2]..=b[5]).contains(&pos.z)
    };
    if by_piece {
        return boxes.iter().any(inside);
    }
    let Some(first) = boxes.first() else {
        return false;
    };
    let whole = boxes.iter().fold(*first, |a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].min(b[2]),
            a[3].max(b[3]),
            a[4].max(b[4]),
            a[5].max(b[5]),
        ]
    });
    inside(&whole)
}

/// Whether `pos` lies inside a piece of a structure `name` in this
/// chunk's structure references.
pub fn in_structure_piece(world: &mut dyn RegionWorld, pos: BlockPos, name: &str) -> bool {
    let starts: Vec<ChunkPos> = world
        .chunk_extra(ChunkPos::of_block(pos))
        .and_then(|extra| {
            extra
                .get("structures")?
                .as_compound()?
                .get("References")?
                .as_compound()?
                .get(name)
                .cloned()
        })
        .and_then(|tag| match tag {
            Tag::LongArray(values) => Some(
                values
                    .iter()
                    .map(|packed| ChunkPos::new(*packed as i32, (*packed >> 32) as i32))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    starts
        .into_iter()
        .any(|start| inside_start(world, start, name, pos, true))
}

enum Attempt {
    Spawned,
    Crowded,
    Failed,
}

type Overrides = Vec<(String, MobCategory, bool, &'static [Spawner])>;

fn read_overrides<'a>(structures: impl Iterator<Item = (&'a str, &'a str)>) -> Overrides {
    let mut all = Vec::new();
    for (name, spawn_overrides) in structures {
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(spawn_overrides) else {
            continue;
        };
        for (category_name, entry) in map {
            let Some(category) = MobCategory::from_name(&category_name) else {
                continue;
            };
            let by_piece = entry["bounding_box"] == "piece";
            let spawns: Vec<Spawner> = entry["spawns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(spawner)
                .collect();
            all.push((
                name.to_owned(),
                category,
                by_piece,
                &*Box::leak(spawns.into_boxed_slice()),
            ));
        }
    }
    all
}

/// Structures' spawn overrides, read once from their data (data pack
/// structures of the region's registries over the built-in ones): whether
/// the override applies by piece, and its spawn list.
fn overrides(name: &str, category: MobCategory) -> Option<(bool, &'static [Spawner])> {
    type Packed = Vec<(
        std::sync::Weak<tuff_item::registries::Registries>,
        Arc<Overrides>,
    )>;
    static ALL: OnceLock<Arc<Overrides>> = OnceLock::new();
    static PACKED: OnceLock<std::sync::Mutex<Packed>> = OnceLock::new();
    let builtin = || {
        ALL.get_or_init(|| {
            Arc::new(read_overrides(
                tuff_vanilla_data::structure::STRUCTURES
                    .iter()
                    .map(|(_, s)| (s.name, s.spawn_overrides)),
            ))
        })
        .clone()
    };
    let registries = tuff_item::registries::current();
    let all = match registries
        .get("minecraft:worldgen/structure")
        .filter(|r| r.modified())
    {
        None => builtin(),
        Some(packs) => {
            let mut packed = PACKED
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            packed.retain(|(owner, _)| owner.strong_count() > 0);
            match packed
                .iter()
                .find(|(owner, _)| std::ptr::eq(owner.as_ptr(), Arc::as_ptr(&registries)))
            {
                Some((_, found)) => found.clone(),
                None => {
                    let texts: Vec<(String, String)> = packs
                        .iter()
                        .filter_map(|(id, entry)| {
                            let json = entry.value().json.as_ref()?;
                            Some((
                                id.to_owned(),
                                json.get("spawn_overrides")
                                    .map_or_else(|| "{}".to_owned(), Value::to_string),
                            ))
                        })
                        .collect();
                    let mut all =
                        read_overrides(texts.iter().map(|(n, t)| (n.as_str(), t.as_str())));
                    all.extend(
                        builtin()
                            .iter()
                            .filter(|(n, ..)| !texts.iter().any(|(pack, _)| pack == n))
                            .cloned(),
                    );
                    let all = Arc::new(all);
                    packed.push((Arc::downgrade(&registries), all.clone()));
                    all
                }
            }
        }
    };
    all.iter()
        .find(|(n, c, ..)| n == name && *c == category)
        .map(|(_, _, by_piece, list)| (*by_piece, *list))
}

fn spawner(value: &Value) -> Option<Spawner> {
    let kind = EntityType::from_name(value["type"].as_str()?)?;
    let weight = value["weight"].as_u64()? as u32;
    let (min_count, max_count) = match &value["count"] {
        Value::Number(n) => (n.as_u64()? as u32, n.as_u64()? as u32),
        count => (
            count["min_inclusive"].as_u64()? as u32,
            count["max_inclusive"].as_u64()? as u32,
        ),
    };
    Some(Spawner {
        kind,
        weight,
        min_count,
        max_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn fortresses_override_monster_spawns_with_blazes() {
        assert_eq!(
            MobCategory::from_name("monster"),
            Some(MobCategory::Monster)
        );
        let (by_piece, list) = overrides("minecraft:fortress", MobCategory::Monster).unwrap();
        assert!(by_piece);
        let blaze = list.iter().find(|s| s.kind == EntityType::Blaze).unwrap();
        assert_eq!((blaze.weight, blaze.min_count, blaze.max_count), (10, 2, 3));
        assert!(overrides("minecraft:fortress", MobCategory::Creature).is_none());
        assert!(overrides("minecraft:village_plains", MobCategory::Monster).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_spawn_entry_pos_pins_the_spawn_spot() {
        let mut data = Compound::new();
        assert_eq!(fixed_position(&data), None);
        data.insert(
            "Pos",
            Tag::List(vec![Tag::Double(1.5), Tag::Double(70.0), Tag::Double(-3.5)]),
        );
        assert_eq!(fixed_position(&data), Some([1.5, 70.0, -3.5]));
        data.insert("Pos", Tag::List(vec![Tag::Double(1.5)]));
        assert_eq!(fixed_position(&data), None);
    }
}
