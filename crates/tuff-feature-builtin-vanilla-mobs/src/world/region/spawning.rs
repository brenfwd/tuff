//! The natural spawn cycle, run by each region over its own chunks near
//! players, and the creatures a freshly generated chunk starts with.
//!
//! The game counts mobs against its caps level-wide. Each region counts
//! its own mobs and spawnable chunks and adds what the other regions of
//! the dimension last reported to the spawn census
//! (`world::spawn_census`), so the caps, and each player's own cap, hold
//! across region borders without a region ever waiting on another.

use std::sync::Arc;

use rand::Rng;
use rand::seq::SliceRandom;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::registry::EntityType;

use super::spawners::spawn_list;
use crate::spawning::{self, Potential, SpawnReason};
use crate::world::spawn_census::{self, Counts, Others, Report, SpawnCensus};
pub use tuff_core::world::entity::rule_holds;
use tuff_core::world::player::Difficulty;
use tuff_core::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// Players spawn mobs in chunks whose center is closer than 128 blocks.
const SPAWN_DISTANCE_SQ: f64 = 128.0 * 128.0;
/// Nothing spawns within 24 blocks of a player or the world spawn.
const MIN_DISTANCE_SQ: f64 = 24.0 * 24.0;
/// Chunks within this many of a player count towards the caps.
const CAP_CHUNK_RADIUS: i32 = 8;
/// The caps are per this many chunks, a 17x17 square.
const CAP_CHUNKS: i32 = 289;
/// Persistent categories (animals) only spawn every this many ticks.
const PERSISTENT_PERIOD: u64 = 400;
const GENERATION_ATTEMPTS: u32 = 4;
/// A region with no player near counts its mobs for the census this often.
const IDLE_CENSUS_PERIOD: u64 = 20;

/// The config key of each category's natural spawn cap, by category id.
const CAP_KEYS: [(MobCategory, &str); 7] = [
    (MobCategory::Monster, "tuff.mobs.spawnCap.monster"),
    (MobCategory::Creature, "tuff.mobs.spawnCap.creature"),
    (MobCategory::Ambient, "tuff.mobs.spawnCap.ambient"),
    (MobCategory::Axolotls, "tuff.mobs.spawnCap.axolotls"),
    (
        MobCategory::UndergroundWaterCreature,
        "tuff.mobs.spawnCap.undergroundWaterCreature",
    ),
    (
        MobCategory::WaterCreature,
        "tuff.mobs.spawnCap.waterCreature",
    ),
    (MobCategory::WaterAmbient, "tuff.mobs.spawnCap.waterAmbient"),
];

const PER_PLAYER_KEY: &str = "tuff.mobs.spawnCap.perPlayer";

/// The config keys for the natural spawn caps, defaulting to vanilla's.
pub fn cap_keys() -> Vec<tuff_core::config::Key> {
    let per_player = tuff_core::config::Key::new(
        PER_PLAYER_KEY,
        tuff_core::config::Value::Bool(false),
        "Cap natural spawns per player (mobs within 128 blocks) instead of by the loaded chunks around all players",
    );
    CAP_KEYS
        .iter()
        .map(|(category, name)| {
            tuff_core::config::Key::new(
                name,
                tuff_core::config::Value::Int(i64::from(category.info().max_per_chunk)),
                "Mobs of this category that spawn naturally per 17x17 chunks around players",
            )
            .validated(|value| match value {
                tuff_core::config::Value::Int(n) if (0..=i64::from(i32::MAX)).contains(n) => Ok(()),
                _ => Err("must be between 0 and 2147483647".to_owned()),
            })
        })
        .chain(std::iter::once(per_player))
        .collect()
}

/// Whether the dimension-wide cap leaves room for a category: always when
/// the caps are per player, which `local_room` then decides.
fn category_open(
    cap: i32,
    counts: i32,
    others: i32,
    dimension_chunks: i32,
    per_player: bool,
) -> bool {
    cap > 0 && (per_player || counts + others < cap * dimension_chunks / CAP_CHUNKS)
}

/// Each category's natural spawn cap: vanilla's, unless configured.
fn caps(config: &tuff_core::config::ConfigStore) -> Counts {
    let mut caps = vanilla_caps();
    for (category, name) in CAP_KEYS {
        if let Some(cap) = config.get_int(name) {
            caps[category.id() as usize] = cap.clamp(0, i64::from(i32::MAX)) as i32;
        }
    }
    caps
}

fn vanilla_caps() -> Counts {
    let mut caps = Counts::default();
    for category in MobCategory::ALL {
        caps[category.id() as usize] = category.info().max_per_chunk;
    }
    caps
}

#[derive(Default)]
struct SpawnState {
    caps: Counts,
    counts: Counts,
    players: Vec<[f64; 3]>,
    local: Vec<Counts>,
    /// Per player, the mobs other regions count for that player's cap.
    remote: Vec<Counts>,
    potential: Potential,
}

impl SpawnState {
    fn players_near(&self, chunk: ChunkPos) -> impl Iterator<Item = usize> + '_ {
        let center = [chunk.x as f64 * 16.0 + 8.0, chunk.z as f64 * 16.0 + 8.0];
        self.players.iter().enumerate().filter_map(move |(i, p)| {
            let (dx, dz) = (p[0] - center[0], p[2] - center[1]);
            (dx * dx + dz * dz < SPAWN_DISTANCE_SQ).then_some(i)
        })
    }

    fn add(&mut self, chunk: ChunkPos, category: MobCategory, local: bool) {
        self.counts[category.id() as usize] += 1;
        if local {
            let near: Vec<usize> = self.players_near(chunk).collect();
            for player in near {
                self.local[player][category.id() as usize] += 1;
            }
        }
    }

    fn local_room(&self, chunk: ChunkPos, category: MobCategory) -> bool {
        let id = category.id() as usize;
        let max = self.caps[id];
        self.players_near(chunk)
            .any(|p| self.local[p][id] + self.remote[p][id] < max)
    }
}

fn spawn_charge(world: &dyn RegionWorld, kind: EntityType, pos: BlockPos) -> f64 {
    let overlay = world.dimension_info().environment().natural_spawns;
    let biome = world
        .biome_name(pos)
        .and_then(tuff_vanilla_data::biome::Biome::get);
    let cost = match biome {
        Some(b) => b.natural_spawns.cost_under(overlay, kind),
        None => overlay.and_then(|over| over.cost(kind)),
    };
    cost.map_or(0.0, |cost| cost.charge)
}

/// Counts the mobs that count towards the caps (every non-misc entity but
/// those marked to stay) into `state`, giving their positions and types.
fn census(world: &dyn RegionWorld, state: &mut SpawnState) -> Vec<(BlockPos, EntityType)> {
    let mut counted = Vec::new();
    for id in world.entity_ids() {
        let Some(tracked) = world.entity(id) else {
            continue;
        };
        let entity = &tracked.entity;
        let base = entity.base();
        let category = base.kind.category();
        if category == MobCategory::Misc || entity.persistence_required() {
            continue;
        }
        counted.push((crate::physics::block_pos(base.position), base.kind));
        state.add(base.chunk(), category, entity.living().is_some());
    }
    counted
}

/// Hands this region's counts to the census when they changed or are due
/// a refresh, and answers what the other regions of the dimension hold.
fn take_census(world: &mut dyn RegionWorld, report: Report) -> Others {
    #[derive(Default)]
    struct Reported(Option<(u64, Arc<Report>)>);

    let shared = world.shared().clone();
    let Some(census) = shared.services.get::<SpawnCensus>() else {
        return Others::default();
    };
    let (dimension, region, now) = (world.dimension_id(), world.region_pos(), world.tick());
    let reported = world.state().get::<Reported>();
    let fresh = reported.0.as_ref().is_some_and(|(at, last)| {
        **last == report && now.saturating_sub(*at) < spawn_census::REFRESH_TICKS
    });
    if !fresh {
        let report = Arc::new(report);
        if census.report(dimension, region, now, report.clone()) {
            reported.0 = Some((now, report));
        }
    }
    census.others(dimension, region)
}

/// One tick of natural spawning in this region's chunks near players.
pub fn spawn_mobs(world: &mut dyn RegionWorld) {
    let shared = world.shared().clone();
    if !shared.game_rules.borrow().bool("spawn_mobs")
        || !world.dimension_info().natural_spawning
        || !world.lighting_available()
    {
        return;
    }
    let seen = world.seen_player_details();
    if seen.is_empty() {
        if world.tick().is_multiple_of(IDLE_CENSUS_PERIOD) {
            let mut state = SpawnState::default();
            census(world, &mut state);
            take_census(
                world,
                Report {
                    chunks: 0,
                    counts: state.counts,
                    near: Vec::new(),
                },
            );
        }
        return;
    }
    let players: Vec<[f64; 3]> = seen.iter().map(|p| p.position).collect();
    let peaceful = *shared.difficulty.borrow() == Difficulty::Peaceful;
    let spawn_enemies = !peaceful && shared.game_rules.borrow().bool("spawn_monsters");
    let spawn_persistent = world.tick().is_multiple_of(PERSISTENT_PERIOD);
    let player_chunks: Vec<ChunkPos> = players.iter().map(|p| ChunkPos::of_position(*p)).collect();
    let loaded = world.loaded_chunks();
    let cap_chunks = loaded
        .iter()
        .filter(|c| {
            player_chunks.iter().any(|p| {
                (p.x - c.x).abs() <= CAP_CHUNK_RADIUS && (p.z - c.z).abs() <= CAP_CHUNK_RADIUS
            })
        })
        .count() as i32;
    let per_player = shared.config.get_bool(PER_PLAYER_KEY) == Some(true);
    let mut state = SpawnState {
        caps: caps(&shared.config),
        local: vec![Counts::default(); players.len()],
        players,
        ..SpawnState::default()
    };
    let counted = census(world, &mut state);
    let mut near: Vec<(u128, Counts)> = seen
        .iter()
        .zip(&state.local)
        .filter(|(_, counts)| counts.iter().any(|c| *c != 0))
        .map(|(player, counts)| (player.uuid, *counts))
        .collect();
    near.sort_unstable_by_key(|(uuid, _)| *uuid);
    let others = take_census(
        world,
        Report {
            chunks: cap_chunks,
            counts: state.counts,
            near,
        },
    );
    state.remote = seen.iter().map(|p| others.near(p.uuid)).collect();
    for (pos, kind) in counted {
        let charge = spawn_charge(world, kind, pos);
        state.potential.add(pos, charge);
    }
    let dimension_chunks = cap_chunks + others.chunks;
    let categories: Vec<MobCategory> = MobCategory::ALL
        .into_iter()
        .filter(|category| {
            let info = category.info();
            let id = category.id() as usize;
            *category != MobCategory::Misc
                && (spawn_enemies || info.friendly)
                && (spawn_persistent || !info.persistent)
                && category_open(
                    state.caps[id],
                    state.counts[id],
                    others.counts[id],
                    dimension_chunks,
                    per_player,
                )
        })
        .collect();
    if categories.is_empty() {
        return;
    }
    let border = world.dimension_info().border();
    let mut chunks: Vec<ChunkPos> = loaded
        .into_iter()
        .filter(|c| {
            let (x, z) = (f64::from(c.x * 16), f64::from(c.z * 16));
            border.contains_box([x, z], [x + 16.0, z + 16.0])
                && world.chunk_lit(*c)
                && state.players_near(*c).next().is_some()
        })
        .collect();
    chunks.shuffle(world.rng());
    for chunk in chunks {
        for category in &categories {
            if state.local_room(chunk, *category) {
                spawn_category(world, *category, chunk, &mut state);
            }
        }
    }
}

fn spawn_category(
    world: &mut dyn RegionWorld,
    category: MobCategory,
    chunk: ChunkPos,
    state: &mut SpawnState,
) {
    let x = chunk.x * 16 + world.rng().gen_range(0..16);
    let z = chunk.z * 16 + world.rng().gen_range(0..16);
    let Some(surface) = world.world_surface(x, z) else {
        return;
    };
    let min_y = world.min_y();
    let y = world.rng().gen_range(min_y..=(surface + 1).max(min_y));
    if y < min_y + 1 {
        return;
    }
    spawn_at(world, category, BlockPos::new(x, y, z), state, true);
}

/// `/debugmobspawning`: one spawn attempt for `category` from `start`,
/// as the cycle would make it, but outside the caps and the spawn
/// potential. `false` when `start` is not loaded here.
pub fn debug_spawn(world: &mut dyn RegionWorld, category: MobCategory, start: BlockPos) -> bool {
    if world.block(start).is_none() {
        return false;
    }
    let players: Vec<[f64; 3]> = world
        .seen_player_details()
        .iter()
        .map(|p| p.position)
        .collect();
    let mut state = SpawnState {
        caps: caps(&world.shared().config),
        local: vec![Counts::default(); players.len()],
        players,
        ..SpawnState::default()
    };
    spawn_at(world, category, start, &mut state, false);
    true
}

fn spawn_at(
    world: &mut dyn RegionWorld,
    category: MobCategory,
    start: BlockPos,
    state: &mut SpawnState,
    budgeted: bool,
) {
    let y = start.y;
    if world.block(start).is_none_or(|s| s.is_redstone_conductor()) {
        return;
    }
    let shared = world.shared().clone();
    let spawn = shared.spawn();
    let world_spawn = (shared.spawn_dimension() == world.dimension_id()).then_some(spawn.pos);
    let mut cluster = 0;
    for _ in 0..3 {
        let (mut x, mut z) = (start.x, start.z);
        let mut spawner: Option<&'static tuff_vanilla_data::biome::Spawner> = None;
        let mut max = (world.rng().r#gen::<f32>() * 4.0).ceil() as u32;
        let mut group = 0;
        let mut attempt = 0;
        while attempt < max {
            attempt += 1;
            x += world.rng().gen_range(0..6) - world.rng().gen_range(0..6);
            z += world.rng().gen_range(0..6) - world.rng().gen_range(0..6);
            let pos = BlockPos::new(x, y, z);
            let center = [x as f64 + 0.5, y as f64, z as f64 + 0.5];
            let Some(distance) = state
                .players
                .iter()
                .map(|p| tuff_core::world::distance_sq(*p, center))
                .reduce(f64::min)
            else {
                continue;
            };
            let near_spawn = world_spawn.is_some_and(|s| {
                tuff_core::world::distance_sq(s.center(), center) < MIN_DISTANCE_SQ
            });
            if distance <= MIN_DISTANCE_SQ
                || near_spawn
                || !world.dimension_info().border().contains_block(pos)
                || !world.chunk_lit(ChunkPos::of_block(pos))
            {
                continue;
            }
            let biome = world.biome_name(pos).unwrap_or("minecraft:plains");
            let list = spawn_list(world, pos, biome, category);
            let entry = match spawner {
                Some(entry) => entry,
                None => {
                    if spawning::reduces_water_ambient(category, biome, world.rng()) {
                        break;
                    }
                    let Some(entry) = spawning::pick(list, world.rng()) else {
                        break;
                    };
                    max = spawning::group_size(entry, world.rng());
                    spawner = Some(entry);
                    entry
                }
            };
            let kind = entry.kind;
            let Some(rule) = shared.entity_types.spawn_rule(kind) else {
                return;
            };
            let despawn = kind.category().info().despawn_distance as f64;
            if !kind.mob().spawns_far_from_players && distance > despawn * despawn {
                continue;
            }
            if !kind.mob().summonable || !list.contains(entry) {
                continue;
            }
            if !position_ok(world, kind, pos, center, rule, SpawnReason::Natural) {
                continue;
            }
            let charge = spawn_charge(world, kind, pos);
            let overlay = world.dimension_info().environment().natural_spawns;
            let cost = match tuff_vanilla_data::biome::Biome::get(biome) {
                Some(b) => b.natural_spawns.cost_under(overlay, kind),
                None => overlay.and_then(|over| over.cost(kind)),
            };
            if budgeted
                && let Some(cost) = cost
                && state.potential.energy_change(pos, cost.charge) > cost.energy_budget
            {
                continue;
            }
            let yaw = world.rng().r#gen::<f32>() * 360.0;
            if !spawn_fresh(world, kind, center, yaw, group, SpawnReason::Natural) {
                continue;
            }
            state.potential.add(pos, charge);
            state.add(ChunkPos::of_block(pos), category, true);
            cluster += 1;
            group += 1;
            if cluster >= spawning::max_spawn_cluster_size(kind) {
                return;
            }
            if world.state().get::<spawning::SpawnGroup>().full {
                break;
            }
        }
    }
}

/// Placement, the type's rule and a free box at `center`.
pub(super) fn position_ok(
    world: &mut dyn RegionWorld,
    kind: EntityType,
    pos: BlockPos,
    center: [f64; 3],
    rule: spawning::SpawnRule,
    reason: SpawnReason,
) -> bool {
    let seed = world.rng().r#gen();
    if !spawning::placement_ok(kind, pos, &|p| world.block(p)) {
        return false;
    }
    let rule_pos = crate::physics::block_pos(center);
    rule_holds(world, seed, kind, rule_pos, rule, reason)
        && crate::physics::no_collision(spawning::spawn_box(kind, center), &|p| world.block(p))
}

/// Creates, sets up and adds a mob the spawn cycle or chunk generation
/// (`reason`) chose, unless its box is obstructed. `group` is how many of
/// its group came before it.
pub fn spawn_fresh(
    world: &mut dyn RegionWorld,
    kind: EntityType,
    center: [f64; 3],
    yaw: f32,
    group: u32,
    reason: SpawnReason,
) -> bool {
    let id = world.shared().next_entity_id();
    let Some(mut entity) = world.shared().entity_types.create(kind, id, center, yaw) else {
        return false;
    };
    let bounds = entity.bounds();
    if !spawning::unobstructed_for(kind, bounds, &|p| world.block(p))
        || blocked_by_entity(world, bounds)
    {
        return false;
    }
    world.state().get::<spawning::SettingUp>().0 = Some(reason);
    world.finalize_spawn(&mut entity, group);
    world.state().get::<spawning::SettingUp>().0 = None;
    world.add_entity(entity);
    true
}

/// Entities a new mob cannot spawn inside: anything living, vehicles, end
/// crystals, falling blocks and primed TNT.
fn blocks_spawning(kind: EntityType, living: bool) -> bool {
    let name = kind.name();
    living
        || name.ends_with("_boat")
        || name.ends_with("_raft")
        || name.contains("minecart")
        || matches!(
            kind,
            EntityType::EndCrystal | EntityType::FallingBlock | EntityType::Tnt
        )
}

fn intersects(a: [f64; 6], b: [f64; 6]) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

fn blocked_by_entity(world: &dyn RegionWorld, bounds: [f64; 6]) -> bool {
    let players = world.present_players().into_iter().any(|p| {
        let [x, y, z] = p.position;
        let half = tuff_core::world::PLAYER_HALF_WIDTH;
        intersects(
            bounds,
            [x - half, y, z - half, x + half, y + p.height, z + half],
        )
    });
    players
        || world.entity_ids().into_iter().any(|id| {
            world.entity(id).is_some_and(|tracked| {
                let entity = &tracked.entity;
                blocks_spawning(entity.base().kind, entity.living().is_some())
                    && intersects(bounds, entity.bounds())
            })
        })
}

/// Spawns a mob of `kind` from a spawn egg used on the block space `at`,
/// standing on whatever is there (reaching a block further down when
/// placed on top of one, `lower`).
pub fn spawn_from_egg<M: tuff_item::item::Mode>(
    world: &mut dyn RegionWorld,
    kind: EntityType,
    at: BlockPos,
    lower: bool,
    components: &tuff_item::item::Components<M>,
) -> bool {
    if !world.owns_chunk(ChunkPos::of_block(at)) {
        return false;
    }
    let size = kind.size();
    let half = size.width as f64 / 2.0;
    let (x, z) = (at.x as f64 + 0.5, at.z as f64 + 0.5);
    let top = at.y as f64 + 1.0;
    let bounds = [
        x - half,
        top,
        z - half,
        x + half,
        top + size.height as f64,
        z + half,
    ];
    let reach = if lower { -2.0 } else { -1.0 };
    let area = crate::physics::expand_towards(bounds, [0.0, reach, 0.0]);
    let shapes = crate::physics::colliders(area, &|p| world.block(p));
    let drop = crate::physics::collide_with_shapes([0.0, reach, 0.0], bounds, &shapes);
    let position = [x, top + drop[1], z];
    let id = world.shared().next_entity_id();
    let yaw = crate::control::wrap_degrees(world.rng().r#gen::<f32>() * 360.0);
    let Some(mut entity) = world.shared().entity_types.create(kind, id, position, yaw) else {
        return false;
    };
    crate::apply_egg_components(&mut entity, components);
    world.finalize_spawn(&mut entity, 0);
    world.add_entity(entity);
    true
}

/// The creatures a freshly generated chunk starts with: groups from its
/// biome's creature list while a roll stays under the biome's creature
/// probability. World generation calls for this through
/// `ChunkGenerator::spawns_creatures`. The groups, their sizes and
/// places come from the chunk's decoration random, so they are the same
/// in every world with the seed; the spawn rules still roll on the
/// region's random.
pub fn spawn_generation_mobs(world: &mut dyn RegionWorld, chunk: ChunkPos) {
    use tuff_item::random::{RandomSource, WorldgenRandom};
    let shared = world.shared().clone();
    if !shared.game_rules.borrow().bool("spawn_mobs") || !world.dimension_info().natural_spawning {
        return;
    }
    let (xo, zo) = (chunk.x * 16, chunk.z * 16);
    let (min_y, top) = (world.min_y(), world.max_y());
    let source = BlockPos::new(xo, top - 1, zo);
    let Some(biome) = world
        .biome_name(source)
        .and_then(tuff_vanilla_data::biome::Biome::get)
    else {
        return;
    };
    let list = biome.natural_spawns.spawners(MobCategory::Creature);
    if list.is_empty() {
        return;
    }
    let probability = biome.natural_spawns.creature_probability();
    let mut random = WorldgenRandom::legacy(0);
    random.set_decoration_seed(shared.seed, xo, zo);
    let total: u32 = list.iter().map(|s| s.weight).sum();
    while random.next_float() < probability {
        if total == 0 {
            continue;
        }
        let mut roll = random.next_int_bounded(total as i32) as u32;
        let Some(entry) = list.iter().find(|s| {
            if roll < s.weight {
                return true;
            }
            roll -= s.weight;
            false
        }) else {
            continue;
        };
        let kind = entry.kind;
        let count = if entry.min_count == entry.max_count {
            entry.min_count
        } else {
            random.next_int_between_inclusive(entry.min_count as i32, entry.max_count as i32) as u32
        };
        let start_x = xo + random.next_int_bounded(16);
        let start_z = zo + random.next_int_bounded(16);
        let (mut x, mut z) = (start_x, start_z);
        let rule = shared.entity_types.spawn_rule(kind);
        let mut group = 0;
        for _ in 0..count {
            let mut success = false;
            let mut attempts = 0;
            while !success && attempts < GENERATION_ATTEMPTS {
                attempts += 1;
                let y = spawning::heightmap_top(kind.mob().heightmap, x, z, top, min_y, &|p| {
                    world.block(p)
                });
                let pos = BlockPos::new(x, y, z);
                if kind.mob().summonable && spawning::placement_ok(kind, pos, &|p| world.block(p)) {
                    let width = kind.size().width as f64;
                    let fx = (x as f64).clamp(xo as f64 + width, xo as f64 + 16.0 - width);
                    let fz = (z as f64).clamp(zo as f64 + width, zo as f64 + 16.0 - width);
                    let center = [fx, y as f64, fz];
                    let fits = rule.is_some_and(|rule| {
                        position_ok(world, kind, pos, center, rule, SpawnReason::ChunkGeneration)
                    });
                    if !fits {
                        continue;
                    }
                    let yaw = random.next_float() * 360.0;
                    success = spawn_fresh(
                        world,
                        kind,
                        center,
                        yaw,
                        group,
                        SpawnReason::ChunkGeneration,
                    );
                    if success {
                        group += 1;
                    }
                }
                x += random.next_int_bounded(5) - random.next_int_bounded(5);
                z += random.next_int_bounded(5) - random.next_int_bounded(5);
                while x < xo || x >= xo + 16 || z < zo || z >= zo + 16 {
                    x = start_x + random.next_int_bounded(5) - random.next_int_bounded(5);
                    z = start_z + random.next_int_bounded(5) - random.next_int_bounded(5);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn living_things_and_solid_entities_block_spawning() {
        for kind in [
            EntityType::OakBoat,
            EntityType::OakChestBoat,
            EntityType::BambooRaft,
            EntityType::Minecart,
            EntityType::HopperMinecart,
            EntityType::EndCrystal,
            EntityType::FallingBlock,
            EntityType::Tnt,
        ] {
            assert!(blocks_spawning(kind, false), "{kind:?}");
        }
        assert!(blocks_spawning(EntityType::Cow, true));
        for kind in [
            EntityType::Item,
            EntityType::ExperienceOrb,
            EntityType::Arrow,
        ] {
            assert!(!blocks_spawning(kind, false), "{kind:?}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn touching_boxes_do_not_intersect() {
        let unit = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        assert!(intersects(unit, [0.5, 0.5, 0.5, 1.5, 1.5, 1.5]));
        assert!(!intersects(unit, [1.0, 0.0, 0.0, 2.0, 1.0, 1.0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_player_cap_counts_the_mobs_other_regions_hold_for_it() {
        let monster = MobCategory::Monster.id() as usize;
        let mut remote = Counts::default();
        remote[monster] = 69;
        let mut state = SpawnState {
            caps: vanilla_caps(),
            players: vec![[8.0, 64.0, 8.0], [5000.0, 64.0, 5000.0]],
            local: vec![Counts::default(); 2],
            remote: vec![remote, Counts::default()],
            ..SpawnState::default()
        };
        let chunk = ChunkPos::new(0, 0);
        assert!(state.local_room(chunk, MobCategory::Monster));
        state.add(chunk, MobCategory::Monster, true);
        assert!(!state.local_room(chunk, MobCategory::Monster));
        assert!(state.local_room(ChunkPos::new(312, 312), MobCategory::Monster));
    }

    #[tuff_core::watchdog]
    #[test]
    fn per_player_caps_skip_only_the_dimension_gate() {
        assert!(!category_open(10, 2, 0, 81, false), "81 chunks allow 2");
        assert!(category_open(10, 1, 0, 81, false));
        assert!(category_open(10, 500, 500, 81, true));
        assert!(!category_open(0, 0, 0, 81, true), "cap 0 still disables");
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawn_caps_default_to_vanilla() {
        let config = tuff_core::config::ConfigStore::default();
        for key in cap_keys() {
            config.register(key).unwrap();
        }
        assert_eq!(config.get_bool(PER_PLAYER_KEY), Some(false));
        let caps = caps(&config);
        let expected = [
            (MobCategory::Monster, 70),
            (MobCategory::Creature, 10),
            (MobCategory::Ambient, 15),
            (MobCategory::Axolotls, 5),
            (MobCategory::UndergroundWaterCreature, 5),
            (MobCategory::WaterCreature, 5),
            (MobCategory::WaterAmbient, 20),
        ];
        for (category, cap) in expected {
            assert_eq!(caps[category.id() as usize], cap, "{category:?}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_configured_cap_limits_local_room() {
        let config = tuff_core::config::ConfigStore::default();
        for key in cap_keys() {
            config.register(key).unwrap();
        }
        config
            .set(
                "tuff.mobs.spawnCap.monster",
                tuff_core::config::Value::Int(1),
            )
            .unwrap();
        let mut state = SpawnState {
            caps: caps(&config),
            players: vec![[8.0, 64.0, 8.0]],
            local: vec![Counts::default()],
            remote: vec![Counts::default()],
            ..SpawnState::default()
        };
        let chunk = ChunkPos::new(0, 0);
        assert!(state.local_room(chunk, MobCategory::Monster));
        state.add(chunk, MobCategory::Monster, true);
        assert!(!state.local_room(chunk, MobCategory::Monster));
        assert!(state.local_room(chunk, MobCategory::Creature));
    }
}
