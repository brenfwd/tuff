//! Natural spawning rules: which spawn list applies where, whether a type
//! may stand at a position (placement), its own spawn rules (animals need
//! grass and light), and the spawn potential some types charge. The spawn
//! cycle itself runs in each region over its own chunks; see
//! `region/spawning.rs`.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::biome::{Biome, Spawner};
use tuff_vanilla_data::entity::{Heightmap, MobCategory, SpawnPlacement};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{BlockKind, BlockState};

use super::physics::BlockLookup;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub use tuff_core::world::entity::{SettingUp, SpawnLevel, SpawnReason, SpawnRule};
pub use tuff_core::world::physics::{spawn_box, unobstructed, unobstructed_for};

/// Whether the mob being set up spawns naturally (not from chunk
/// generation, a spawner, an egg or a command).
pub fn spawning_naturally(cx: &mut tuff_core::world::entity::TickContext) -> bool {
    cx.state.get::<SettingUp>().0 == Some(SpawnReason::Natural)
}

/// Whether the mob being set up was placed by structure generation.
pub fn spawning_from_structure(cx: &mut tuff_core::world::entity::TickContext) -> bool {
    cx.state.get::<SettingUp>().0 == Some(SpawnReason::Structure)
}

/// The group a mob being set up spawns in, kept in the region's state: its
/// first member leaves data for the rest (a school's leader and look).
#[derive(Debug, Default)]
pub struct SpawnGroup {
    /// How many of the group came before the mob spawning now.
    pub index: u32,
    pub data: Option<GroupData>,
    /// The group takes no more members (a lone tropical fish).
    pub full: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GroupData {
    /// Fish follow `leader`; tropical fish share its `variant`.
    School { leader: i32, variant: Option<i32> },
    /// Axolotls pick one of two colors.
    Axolotls([i32; 2]),
}

/// The most of one type a natural spawn cycle places around one spot.
pub fn max_spawn_cluster_size(kind: EntityType) -> u32 {
    match kind {
        EntityType::Salmon => 5,
        EntityType::Cod | EntityType::TropicalFish | EntityType::Pufferfish | EntityType::Wolf => 8,
        EntityType::Horse
        | EntityType::Donkey
        | EntityType::Mule
        | EntityType::SkeletonHorse
        | EntityType::ZombieHorse
        | EntityType::Camel
        | EntityType::CamelHusk
        | EntityType::Llama
        | EntityType::TraderLlama => 6,
        EntityType::Ghast | EntityType::HappyGhast | EntityType::Pillager => 1,
        _ => 4,
    }
}

/// The game's default mob rule: something valid to stand on.
pub fn mob_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    reason.is_spawner() || (level.block)(below(pos)).is_some_and(|state| state.is_valid_spawn(kind))
}

/// Bats: below the world surface, half the time, in the dark (light no
/// more than a roll of 0 to 3), on the blocks bats spawn on.
pub fn bat_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    let top = level.min_y + level.dimension.height as i32;
    let surface = heightmap_top(
        Heightmap::WorldSurface,
        pos.x,
        pos.z,
        top,
        level.min_y,
        level.block,
    );
    if pos.y >= surface
        || rng.gen_bool(0.5)
        || i32::from(level.raw_brightness(pos, level.sky_darken)) > rng.gen_range(0..4)
        || !(level.block)(below(pos)).is_some_and(|state| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::BATS_SPAWNABLE_ON,
                state.block(),
            )
        })
    {
        return false;
    }
    mob_rule(kind, level, pos, reason, rng)
}

/// Silverfish and endermites: on valid ground, and not within 5 blocks of a
/// player unless from a spawner.
pub fn no_player_close_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    if !mob_rule(kind, level, pos, reason, rng) {
        return false;
    }
    reason == SpawnReason::Spawner
        || !(level.player_within)(
            [
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            ],
            5.0,
        )
}

fn on_ground_in_light(
    level: &SpawnLevel,
    pos: BlockPos,
    ground: tuff_vanilla_data::tag::Key<tuff_vanilla_data::Block>,
) -> bool {
    (level.block)(below(pos)).is_some_and(|state| tuff_datapack::tags::is(ground, state.block()))
        && level.raw_brightness(pos, 0) > 8
}

/// Animals: on a block animals spawn on, in light brighter than 8.
pub fn animal_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    on_ground_in_light(
        level,
        pos,
        tuff_vanilla_data::tag::block::key::ANIMALS_SPAWNABLE_ON,
    )
}

/// Turtles: on sand in the light, at most three blocks above sea level.
pub fn turtle_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    pos.y < level.sea_level + 4
        && on_ground_in_light(level, pos, tuff_vanilla_data::tag::block::key::SAND)
}

/// Mooshrooms: like animals, but on mycelium.
pub fn mooshroom_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    on_ground_in_light(
        level,
        pos,
        tuff_vanilla_data::tag::block::key::MOOSHROOMS_SPAWNABLE_ON,
    )
}

/// Goats: like animals, on the mountain blocks goats spawn on.
pub fn goat_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    on_ground_in_light(
        level,
        pos,
        tuff_vanilla_data::tag::block::key::GOATS_SPAWNABLE_ON,
    )
}

/// Rabbits: like animals, on the blocks rabbits spawn on.
pub fn rabbit_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    on_ground_in_light(
        level,
        pos,
        tuff_vanilla_data::tag::block::key::RABBITS_SPAWNABLE_ON,
    )
}

/// Wolves: like animals, on the blocks wolves spawn on.
pub fn wolf_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    on_ground_in_light(
        level,
        pos,
        tuff_vanilla_data::tag::block::key::WOLVES_SPAWNABLE_ON,
    )
}

/// Parrots: like animals, on the blocks parrots spawn on.
pub fn parrot_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    on_ground_in_light(
        level,
        pos,
        tuff_vanilla_data::tag::block::key::PARROTS_SPAWNABLE_ON,
    )
}

/// Whether it is dark enough at `pos` for monsters: sky light under a
/// random bar, block light within the dimension's limit, and the light a
/// player would see there at or under the dimension's random spawn level.
pub fn is_dark_enough(level: &SpawnLevel, pos: BlockPos, rng: &mut StdRng) -> bool {
    let (sky, block) = (level.light)(pos).unwrap_or((0, 0));
    if sky as i32 > rng.gen_range(0..32) {
        return false;
    }
    let limit = level.dimension.monster_spawn_block_light_limit;
    if limit < 15 && block > limit {
        return false;
    }
    let darken = if level.thundering {
        10
    } else {
        level.sky_darken
    };
    let (low, high) = level.dimension.monster_spawn_light_level;
    level.raw_brightness(pos, darken) <= rng.gen_range(low..=high)
}

/// Monsters: dark enough, on something valid to stand on.
pub fn monster_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    (reason.ignores_light() || is_dark_enough(level, pos, rng))
        && mob_rule(kind, level, pos, reason, rng)
}

/// Monsters that need the open sky (strays, husks): dark enough, on
/// something to stand on, and under the sky unless from a spawner. Powder
/// snow above does not hide the sky.
pub fn monster_under_sky_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    if !monster_rule(kind, level, pos, reason, rng) {
        return false;
    }
    if reason.is_spawner() {
        return true;
    }
    let mut sky = BlockPos::new(pos.x, pos.y + 1, pos.z);
    while (level.block)(sky).is_some_and(|s| s.block() == tuff_vanilla_data::Block::PowderSnow) {
        sky.y += 1;
    }
    sees_sky(level, BlockPos::new(sky.x, sky.y - 1, sky.z))
}

/// Drowned: dark enough, water below the feet (or a spawner), and water at
/// the feet too unless from a spawner; rarer than a plain monster, and
/// away from rivers only more than five blocks under sea level.
pub fn drowned_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    let spawner = reason.is_spawner();
    let is_water = |p: BlockPos| {
        (level.block)(p).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        })
    };
    if !spawner && !is_water(below(pos)) {
        return false;
    }
    if !(reason.ignores_light() || is_dark_enough(level, pos, rng)) || !(spawner || is_water(pos)) {
        return false;
    }
    if spawner || reason == SpawnReason::Reinforcement {
        return true;
    }
    let frequent = (level.biome)(pos).is_some_and(|biome| {
        tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::MORE_FREQUENT_DROWNED_SPAWNS,
            biome,
        )
    });
    if frequent {
        return rng.gen_range(0..15) == 0;
    }
    rng.gen_range(0..40) == 0 && pos.y < level.sea_level - 5
}

fn water_fluid_at(level: &SpawnLevel, pos: BlockPos) -> bool {
    (level.block)(pos).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::WATER,
            s.fluid_state().fluid(),
        )
    })
}

fn water_block_at(level: &SpawnLevel, pos: BlockPos) -> bool {
    (level.block)(pos).is_some_and(|s| s.block() == tuff_vanilla_data::Block::Water)
}

/// Water below and a water block above `pos`.
fn in_open_water(level: &SpawnLevel, pos: BlockPos) -> bool {
    water_fluid_at(level, below(pos))
        && water_block_at(level, BlockPos::new(pos.x, pos.y + 1, pos.z))
}

/// Squid, dolphins and most fish: open water from 13 blocks under sea
/// level up to it.
pub fn surface_water_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    (level.sea_level - 13..=level.sea_level).contains(&pos.y) && in_open_water(level, pos)
}

/// Tropical fish: open water near the surface, or at any depth in biomes
/// that allow it.
pub fn tropical_fish_rule(
    kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    in_open_water(level, pos)
        && ((level.biome)(pos).is_some_and(|biome| {
            tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::ALLOWS_TROPICAL_FISH_SPAWNS_AT_ANY_HEIGHT,
            biome,
        )
        }) || surface_water_rule(kind, level, pos, reason, rng))
}

/// Glow squid: pitch dark water at least 33 blocks under sea level.
pub fn glow_squid_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    pos.y <= level.sea_level - 33 && level.raw_brightness(pos, 0) == 0 && water_block_at(level, pos)
}

/// Axolotls: on a block they spawn on (clay).
pub fn axolotl_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    (level.block)(below(pos)).is_some_and(|state| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::AXOLOTLS_SPAWNABLE_ON,
            state.block(),
        )
    })
}

/// Nautiluses: open water from 25 to 5 blocks under sea level.
pub fn nautilus_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    _reason: SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    (level.sea_level - 25..=level.sea_level - 5).contains(&pos.y) && in_open_water(level, pos)
}

/// Guardians: in water, mostly where the sky cannot be seen through it.
pub fn guardian_rule(
    _kind: EntityType,
    level: &SpawnLevel,
    pos: BlockPos,
    reason: SpawnReason,
    rng: &mut StdRng,
) -> bool {
    (rng.gen_range(0..20) == 0 || !sees_sky_from_below_water(level, pos))
        && level.difficulty != tuff_core::world::player::Difficulty::Peaceful
        && (reason.is_spawner() || water_fluid_at(level, pos))
        && water_fluid_at(level, below(pos))
}

/// Whether the sky shows at `pos`, looking through liquids and clear
/// blocks between it and sea level.
fn sees_sky_from_below_water(level: &SpawnLevel, pos: BlockPos) -> bool {
    let full_sky = |p: BlockPos| (level.light)(p).is_some_and(|(sky, _)| sky >= 15);
    if pos.y >= level.sea_level {
        return full_sky(pos);
    }
    if !full_sky(BlockPos::new(pos.x, level.sea_level, pos.z)) {
        return false;
    }
    (pos.y + 1..level.sea_level).all(|y| {
        (level.block)(BlockPos::new(pos.x, y, pos.z))
            .is_none_or(|s| s.light_dampening() == 0 || s.is_liquid())
    })
}

/// Whether nothing but air stands above `pos` as far as the level knows.
fn sees_sky(level: &SpawnLevel, pos: BlockPos) -> bool {
    const MAX_HEIGHT: i32 = 4064;
    (pos.y + 1..MAX_HEIGHT)
        .map_while(|y| (level.block)(BlockPos::new(pos.x, y, pos.z)))
        .all(is_air)
}

fn below(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y - 1, pos.z)
}

/// Whether nothing at `pos` stops `kind` from spawning in it: no full
/// block, signal source, fluid or danger.
pub fn valid_empty_spawn_block(state: Option<BlockState>, kind: EntityType) -> bool {
    let Some(state) = state else {
        return false;
    };
    !state.is_collision_full_block()
        && !state.is_signal_source()
        && state.fluid_state().is_empty()
        && !tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::PREVENT_MOB_SPAWNING_INSIDE,
            state.block(),
        )
        && !state.is_dangerous_to(kind)
}

/// Whether `kind` may be placed at `pos` by its spawn placement.
pub fn placement_ok(kind: EntityType, pos: BlockPos, block: BlockLookup) -> bool {
    let fluid_at =
        |p: BlockPos, fluids: tuff_vanilla_data::tag::Key<tuff_vanilla_data::fluid::Fluid>| {
            block(p).is_some_and(|s| tuff_datapack::tags::is(fluids, s.fluid_state().fluid()))
        };
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    match kind.mob().placement {
        SpawnPlacement::NoRestrictions => block(pos).is_some(),
        SpawnPlacement::InWater => {
            fluid_at(pos, tuff_vanilla_data::tag::fluid::key::WATER)
                && block(above).is_some_and(|s| !s.is_redstone_conductor())
        }
        SpawnPlacement::InLava => fluid_at(pos, tuff_vanilla_data::tag::fluid::key::LAVA),
        SpawnPlacement::OnGround => {
            block(below(pos)).is_some_and(|s| s.is_valid_spawn(kind))
                && valid_empty_spawn_block(block(pos), kind)
                && valid_empty_spawn_block(block(above), kind)
        }
    }
}

/// The spawn list of `category` in a biome.
pub fn spawners(biome: &str, category: MobCategory) -> &'static [Spawner] {
    Biome::get(biome).map_or(&[], |b| b.natural_spawns.spawners(category))
}

/// Whether a natural spawn roll of `category` in `biome` is dropped for
/// the biome's reduced water ambient spawns.
pub fn reduces_water_ambient(category: MobCategory, biome: &str, rng: &mut impl Rng) -> bool {
    category == MobCategory::WaterAmbient
        && tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::REDUCE_WATER_AMBIENT_SPAWNS,
            biome,
        )
        && rng.r#gen::<f32>() < 0.98
}

/// One weighted pick from a spawn list.
pub fn pick(spawners: &'static [Spawner], rng: &mut StdRng) -> Option<&'static Spawner> {
    let total: u32 = spawners.iter().map(|s| s.weight).sum();
    if total == 0 {
        return None;
    }
    let mut roll = rng.gen_range(0..total);
    spawners.iter().find(|s| {
        if roll < s.weight {
            return true;
        }
        roll -= s.weight;
        false
    })
}

/// A group size for one spawner entry.
pub fn group_size(spawner: &Spawner, rng: &mut StdRng) -> u32 {
    rng.gen_range(spawner.min_count..=spawner.max_count)
}

/// The spawn potential charged mobs build up: types with a spawn cost only
/// spawn where adding their charge stays within budget.
#[derive(Debug, Default)]
pub struct Potential {
    charges: Vec<(BlockPos, f64)>,
}

impl Potential {
    pub fn add(&mut self, pos: BlockPos, charge: f64) {
        if charge != 0.0 {
            self.charges.push((pos, charge));
        }
    }

    pub fn energy_change(&self, pos: BlockPos, charge: f64) -> f64 {
        if charge == 0.0 {
            return 0.0;
        }
        let change: f64 = self
            .charges
            .iter()
            .map(|(at, c)| {
                let d = [
                    (at.x - pos.x) as f64,
                    (at.y - pos.y) as f64,
                    (at.z - pos.z) as f64,
                ];
                let distance_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                if distance_sq == 0.0 {
                    f64::INFINITY
                } else {
                    c / distance_sq.sqrt()
                }
            })
            .sum();
        change * charge
    }
}

/// The top of a column by a heightmap's rule: one above the highest block
/// it counts, looking down from `top`.
pub fn heightmap_top(
    kind: Heightmap,
    x: i32,
    z: i32,
    top: i32,
    min_y: i32,
    block: BlockLookup,
) -> i32 {
    for y in (min_y..=top).rev() {
        let Some(state) = block(BlockPos::new(x, y, z)) else {
            continue;
        };
        let solid = tuff_core::world::contact::blocks_motion(state);
        let motion = solid || !state.fluid_state().is_empty();
        let counts = match kind {
            Heightmap::WorldSurface | Heightmap::WorldSurfaceWg => !is_air(state),
            Heightmap::OceanFloor | Heightmap::OceanFloorWg => solid,
            Heightmap::MotionBlocking => motion,
            Heightmap::MotionBlockingNoLeaves => {
                motion && state.block().kind() != BlockKind::Leaves
            }
        };
        if counts {
            return y + 1;
        }
    }
    min_y
}

#[cfg(test)]
mod tests {
    #[tuff_core::watchdog]
    #[test]
    fn ghasts_spawn_in_clusters_of_four_except_in_basalt_deltas() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut size = |biome: &str| {
            let ghast = spawners(biome, MobCategory::Monster)
                .iter()
                .find(|s| s.kind == EntityType::Ghast)
                .expect("ghasts spawn here");
            group_size(ghast, &mut rng)
        };
        assert_eq!(size("minecraft:nether_wastes"), 4);
        assert_eq!(size("minecraft:soul_sand_valley"), 4);
        assert_eq!(size("minecraft:basalt_deltas"), 1);
    }

    use rand::SeedableRng;
    use tuff_vanilla_data::Block;

    use super::*;

    fn meadow(pos: BlockPos) -> Option<BlockState> {
        Some(match pos.y {
            ..63 => Block::Dirt.default_state(),
            63 => Block::GrassBlock.default_state(),
            64 if pos.x == 5 => Block::Cactus.default_state(),
            _ => Block::Air.default_state(),
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn river_biomes_drop_most_water_ambient_spawn_rolls() {
        let mut rng = StdRng::seed_from_u64(2);
        let mut dropped = |category, biome| {
            (0..1000)
                .filter(|_| reduces_water_ambient(category, biome, &mut rng))
                .count()
        };
        let river = dropped(MobCategory::WaterAmbient, "minecraft:river");
        assert!((950..=1000).contains(&river), "{river}");
        assert_eq!(dropped(MobCategory::WaterAmbient, "minecraft:ocean"), 0);
        assert_eq!(dropped(MobCategory::WaterCreature, "minecraft:river"), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn silverfish_and_endermites_keep_five_blocks_from_players() {
        let mut rng = StdRng::seed_from_u64(1);
        let bright = |_: BlockPos| Some((15, 0));
        let block = |p: BlockPos| {
            Some(if p.y < 64 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let pos = BlockPos::new(0, 64, 0);
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        let mut spawns = |near: bool, reason: SpawnReason| {
            let player_within = |_: [f64; 3], range: f64| near && range == 5.0;
            let level = SpawnLevel {
                block: &block,
                light: &bright,
                sky_darken: 0,
                min_y: -64,
                sea_level: 63,
                dimension: overworld,
                thundering: false,
                biome: &|_| None,
                seed: 0,
                surface_slime_chance: 0.0,
                difficulty: tuff_core::world::player::Difficulty::Normal,
                player_within: &player_within,
            };
            no_player_close_rule(EntityType::Silverfish, &level, pos, reason, &mut rng)
        };
        assert!(spawns(false, SpawnReason::Natural));
        assert!(!spawns(true, SpawnReason::Natural));
        assert!(spawns(true, SpawnReason::Spawner));
    }

    #[tuff_core::watchdog]
    #[test]
    fn bats_spawn_only_in_dark_caves_on_their_blocks() {
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        let pos = BlockPos::new(0, 64, 0);
        let dark = |_: BlockPos| Some((0, 0));
        let passes = |floor: Block, roof: bool| {
            let block = move |p: BlockPos| {
                Some(if p.y == 63 {
                    floor.default_state()
                } else if roof && p.y == 70 {
                    Block::Stone.default_state()
                } else {
                    Block::Air.default_state()
                })
            };
            let level = SpawnLevel {
                block: &block,
                light: &dark,
                sky_darken: 0,
                min_y: -64,
                sea_level: 63,
                dimension: overworld,
                thundering: false,
                biome: &|_| None,
                seed: 0,
                surface_slime_chance: 0.0,
                difficulty: tuff_core::world::player::Difficulty::Normal,
                player_within: &|_, _| false,
            };
            let mut rng = StdRng::seed_from_u64(5);
            (0..200)
                .filter(|_| bat_rule(EntityType::Bat, &level, pos, SpawnReason::Natural, &mut rng))
                .count()
        };
        let cave = passes(Block::Stone, true);
        assert!((60..140).contains(&cave), "about half pass, got {cave}");
        assert_eq!(passes(Block::Stone, false), 0, "never on the surface");
        assert_eq!(passes(Block::Glass, true), 0, "never on other blocks");
    }

    #[tuff_core::watchdog]
    #[test]
    fn mooshrooms_goats_and_rabbits_need_their_own_ground() {
        let mut rng = StdRng::seed_from_u64(1);
        let bright = |_: BlockPos| Some((15, 0));
        let pos = BlockPos::new(0, 64, 0);
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        let mut on = |ground: Block, rule: SpawnRule, kind: EntityType| {
            let block = move |p: BlockPos| {
                Some(if p.y == 63 {
                    ground.default_state()
                } else {
                    Block::Air.default_state()
                })
            };
            let level = SpawnLevel {
                block: &block,
                light: &bright,
                sky_darken: 0,
                min_y: -64,
                sea_level: 63,
                dimension: overworld,
                thundering: false,
                biome: &|_| None,
                seed: 0,
                surface_slime_chance: 0.0,
                difficulty: tuff_core::world::player::Difficulty::Normal,
                player_within: &|_, _| false,
            };
            rule(kind, &level, pos, SpawnReason::Natural, &mut rng)
        };
        assert!(on(Block::Mycelium, mooshroom_rule, EntityType::Mooshroom));
        assert!(!on(
            Block::GrassBlock,
            mooshroom_rule,
            EntityType::Mooshroom
        ));
        assert!(!on(Block::Mycelium, animal_rule, EntityType::Cow));
        assert!(on(Block::Stone, goat_rule, EntityType::Goat));
        assert!(!on(Block::Stone, animal_rule, EntityType::Goat));
        assert!(on(Block::Sand, rabbit_rule, EntityType::Rabbit));
        assert!(!on(Block::Sand, animal_rule, EntityType::Rabbit));
        assert!(on(Block::Podzol, wolf_rule, EntityType::Wolf));
        assert!(!on(Block::Podzol, animal_rule, EntityType::Wolf));
        assert!(on(Block::OakLeaves, parrot_rule, EntityType::Parrot));
        assert!(!on(Block::OakLeaves, animal_rule, EntityType::Parrot));
    }

    #[tuff_core::watchdog]
    #[test]
    fn animals_spawn_on_grass_in_the_light() {
        let mut rng = StdRng::seed_from_u64(1);
        let bright = |_: BlockPos| Some((15, 0));
        let dark = |_: BlockPos| Some((4, 0));
        let pos = BlockPos::new(0, 64, 0);
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        let lit = SpawnLevel {
            block: &meadow,
            light: &bright,
            sky_darken: 0,
            min_y: -64,
            sea_level: 63,
            dimension: overworld,
            thundering: false,
            biome: &|_| None,
            seed: 0,
            surface_slime_chance: 0.0,
            difficulty: tuff_core::world::player::Difficulty::Normal,
            player_within: &|_, _| false,
        };
        let unlit = SpawnLevel {
            block: &meadow,
            light: &dark,
            sky_darken: 0,
            min_y: -64,
            sea_level: 63,
            dimension: overworld,
            thundering: false,
            biome: &|_| None,
            seed: 0,
            surface_slime_chance: 0.0,
            difficulty: tuff_core::world::player::Difficulty::Normal,
            player_within: &|_, _| false,
        };
        assert!(placement_ok(EntityType::Cow, pos, &meadow));
        assert!(animal_rule(
            EntityType::Cow,
            &lit,
            pos,
            SpawnReason::Natural,
            &mut rng
        ));
        assert!(!animal_rule(
            EntityType::Cow,
            &unlit,
            pos,
            SpawnReason::Natural,
            &mut rng
        ));
        assert!(!placement_ok(
            EntityType::Cow,
            BlockPos::new(5, 64, 0),
            &meadow
        ));
        assert!(!placement_ok(
            EntityType::Cow,
            BlockPos::new(0, 63, 0),
            &meadow
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn plains_spawn_farm_animals_and_heightmaps_find_the_ground() {
        let creatures = spawners("minecraft:plains", MobCategory::Creature);
        assert!(creatures.iter().any(|s| s.kind == EntityType::Sheep));
        let mut rng = StdRng::seed_from_u64(3);
        for _ in 0..20 {
            let picked = pick(creatures, &mut rng).unwrap();
            let size = group_size(picked, &mut rng);
            assert!((picked.min_count..=picked.max_count).contains(&size));
        }
        assert_eq!(
            heightmap_top(Heightmap::MotionBlockingNoLeaves, 0, 0, 100, -64, &meadow),
            64
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn charges_repel_with_distance() {
        let mut potential = Potential::default();
        potential.add(BlockPos::new(0, 64, 0), 0.7);
        let near = potential.energy_change(BlockPos::new(1, 64, 0), 0.7);
        let far = potential.energy_change(BlockPos::new(10, 64, 0), 0.7);
        assert!(near > far && far > 0.0);
        assert_eq!(potential.energy_change(BlockPos::new(3, 64, 0), 0.0), 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn monsters_need_the_dark() {
        let mut rng = StdRng::seed_from_u64(7);
        let overworld =
            tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld").unwrap();
        assert_eq!(overworld.monster_spawn_light_level, (0, 7));
        assert_eq!(overworld.monster_spawn_block_light_limit, 0);
        let noon = |_: BlockPos| Some((15, 0));
        let cave = |_: BlockPos| Some((0, 0));
        let torch = |_: BlockPos| Some((0, 12));
        let lights: [&dyn Fn(BlockPos) -> Option<(u8, u8)>; 3] = [&noon, &cave, &torch];
        let pos = BlockPos::new(0, 64, 0);
        let results: Vec<bool> = lights
            .into_iter()
            .map(|light| {
                let level = SpawnLevel {
                    block: &meadow,
                    light,
                    sky_darken: 0,
                    min_y: -64,
                    sea_level: 63,
                    dimension: overworld,
                    thundering: false,
                    biome: &|_| None,
                    seed: 0,
                    surface_slime_chance: 0.0,
                    difficulty: tuff_core::world::player::Difficulty::Normal,
                    player_within: &|_, _| false,
                };
                (0..50).any(|_| {
                    monster_rule(
                        EntityType::Zombie,
                        &level,
                        pos,
                        SpawnReason::Natural,
                        &mut rng,
                    )
                })
            })
            .collect();
        assert_eq!(results, [false, true, false]);
    }

    fn sea(pos: BlockPos) -> Option<BlockState> {
        Some(match pos.y {
            ..=0 => Block::Clay.default_state(),
            1..=62 => Block::Water.default_state(),
            _ => Block::Air.default_state(),
        })
    }

    fn water_level<'a>(
        light: &'a dyn Fn(BlockPos) -> Option<(u8, u8)>,
        biome: &'a dyn Fn(BlockPos) -> Option<&'static str>,
    ) -> SpawnLevel<'a> {
        SpawnLevel {
            block: &sea,
            light,
            sky_darken: 0,
            min_y: -64,
            sea_level: 63,
            dimension: tuff_vanilla_data::dimension_type::DimensionType::get("minecraft:overworld")
                .unwrap(),
            thundering: false,
            biome,
            seed: 0,
            surface_slime_chance: 0.0,
            difficulty: tuff_core::world::player::Difficulty::Normal,
            player_within: &|_, _| false,
        }
    }

    fn holds(rule: SpawnRule, kind: EntityType, level: &SpawnLevel, y: i32) -> bool {
        let mut rng = StdRng::seed_from_u64(5);
        rule(
            kind,
            level,
            BlockPos::new(0, y, 0),
            SpawnReason::Natural,
            &mut rng,
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_rules_follow_their_depth_bands() {
        let lit = |p: BlockPos| Some((if p.y > 40 { 15 } else { 0 }, 0));
        let ocean = |_: BlockPos| Some("minecraft:ocean");
        let lush = |_: BlockPos| Some("minecraft:lush_caves");
        let level = water_level(&lit, &ocean);
        let surface = |y| holds(surface_water_rule, EntityType::Squid, &level, y);
        assert!(surface(50) && surface(61));
        assert!(!surface(49), "13 below sea level is the floor");
        assert!(!surface(62), "the block above must be water");
        let nautilus = |y| holds(nautilus_rule, EntityType::Nautilus, &level, y);
        assert!(nautilus(38) && nautilus(58));
        assert!(!nautilus(37) && !nautilus(59));
        assert!(!holds(
            tropical_fish_rule,
            EntityType::TropicalFish,
            &level,
            20
        ));
        let lush_level = water_level(&lit, &lush);
        assert!(holds(
            tropical_fish_rule,
            EntityType::TropicalFish,
            &lush_level,
            20
        ));
        let glow = |y| holds(glow_squid_rule, EntityType::GlowSquid, &level, y);
        assert!(glow(30));
        assert!(!glow(31), "33 below sea level at most");
        let bright = |_: BlockPos| Some((0, 3));
        let lamp = water_level(&bright, &ocean);
        assert!(!holds(glow_squid_rule, EntityType::GlowSquid, &lamp, 30));
        assert!(holds(axolotl_rule, EntityType::Axolotl, &level, 1));
        assert!(!holds(axolotl_rule, EntityType::Axolotl, &level, 2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn guardians_prefer_water_the_sky_cannot_reach() {
        let ocean = |_: BlockPos| Some("minecraft:ocean");
        let open = |_: BlockPos| Some((15, 0));
        let covered = |_: BlockPos| Some((0, 0));
        let mut rng = StdRng::seed_from_u64(11);
        let count = |light: &dyn Fn(BlockPos) -> Option<(u8, u8)>, rng: &mut StdRng| {
            let level = water_level(light, &ocean);
            (0..400)
                .filter(|_| {
                    guardian_rule(
                        EntityType::Guardian,
                        &level,
                        BlockPos::new(0, 30, 0),
                        SpawnReason::Natural,
                        rng,
                    )
                })
                .count()
        };
        assert_eq!(count(&covered, &mut rng), 400);
        let under_sky = count(&open, &mut rng);
        assert!(
            (5..50).contains(&under_sky),
            "{under_sky} of 400 under the sky"
        );
        let level = water_level(&covered, &ocean);
        assert!(!holds(guardian_rule, EntityType::Guardian, &level, 70));
    }

    #[tuff_core::watchdog]
    #[test]
    fn drowned_away_from_rivers_need_depth() {
        let dark = |_: BlockPos| Some((0, 0));
        let ocean = |_: BlockPos| Some("minecraft:ocean");
        let level = water_level(&dark, &ocean);
        let mut rng = StdRng::seed_from_u64(2);
        let mut spawns = |y| {
            (0..2000)
                .filter(|_| {
                    drowned_rule(
                        EntityType::Drowned,
                        &level,
                        BlockPos::new(0, y, 0),
                        SpawnReason::Natural,
                        &mut rng,
                    )
                })
                .count()
        };
        assert!(spawns(40) > 0);
        assert_eq!(spawns(58), 0, "five blocks under sea level is too shallow");
    }

    #[tuff_core::watchdog]
    #[test]
    fn swimmers_ignore_liquid_in_their_spawn_box() {
        let bounds = spawn_box(EntityType::Cod, [0.5, 30.0, 0.5]);
        assert!(unobstructed_for(EntityType::Cod, bounds, &sea));
        assert!(!unobstructed_for(EntityType::Zombie, bounds, &sea));
    }
}
