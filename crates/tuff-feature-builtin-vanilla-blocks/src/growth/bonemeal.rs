//! What bone meal does to plants and blocks that are not simply aged:
//! grass sprouting grass and the biome's flowers, tall plants, sea
//! pickles, seagrass, moss, roots and glow berries, and underwater growth
//! from a sturdy face.

use std::collections::HashMap;
use std::sync::OnceLock;

use tuff_vanilla_data::block::{Facing, Half};
use tuff_vanilla_data::tag::Key;
use tuff_vanilla_data::{Block, BlockState};

use super::{
    Behaviours, Bonemeal, Features, GrowthLevel, GrowthLevelExt, above, always, below, offset,
};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS};
use tuff_item::random::RandomSource;
use tuff_vanilla_common::placement::{self, step};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// Level event: bone meal's growth particles and sound, data = count.
pub const GROWTH_EVENT: i32 = tuff_core::world::growth::BONEMEAL_EVENT;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_bonemeal_kind(
        "grass",
        Bonemeal {
            valid: |level, pos, _| {
                is_air(level.state(above(pos))) && level.is_inside_build_height(pos.y + 1)
            },
            success: always,
            perform: grass,
        },
    );
    behaviours.register_bonemeal_kind(
        "tall_grass",
        Bonemeal {
            valid: |level, pos, state| {
                let Some(tall) = state.block().grown_plant() else {
                    return false;
                };
                super::can_survive(level, tall.default_state(), pos)
                    && level.is_empty(above(pos))
                    && level.is_inside_build_height(pos.y + 1)
            },
            success: always,
            perform: |level, pos, state| {
                if let Some(tall) = state.block().grown_plant() {
                    place_double(level, tall.default_state(), pos);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "tall_flower",
        Bonemeal {
            valid: |_, _, _| true,
            success: always,
            perform: |level, pos, state| pop_self(level, pos, state),
        },
    );
    behaviours.register_bonemeal_kind(
        "flower_bed",
        Bonemeal {
            valid: |_, _, _| true,
            success: always,
            perform: |level, pos, state| {
                let amount = state.flower_amount().unwrap_or(1);
                match state.with_flower_amount(amount + 1).filter(|_| amount < 4) {
                    Some(fuller) => level.set(pos, fuller, UPDATE_CLIENTS),
                    None => pop_self(level, pos, state),
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "sea_pickle",
        Bonemeal {
            valid: |level, pos, state| {
                state.waterlogged() == Some(true)
                    && tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::CORAL_BLOCKS,
                        level.state(below(pos)).block(),
                    )
            },
            success: always,
            perform: sea_pickles,
        },
    );
    behaviours.register_bonemeal_kind(
        "seagrass",
        Bonemeal {
            valid: |level, pos, _| level.state(above(pos)).block() == Block::Water,
            success: always,
            perform: |level, pos, _| {
                let lower = Block::TallSeagrass.default_state();
                level.set(pos, lower, UPDATE_CLIENTS);
                if let Some(upper) = lower.with_half(Half::Upper) {
                    level.set(above(pos), upper, UPDATE_CLIENTS);
                }
            },
        },
    );
    let berries = Bonemeal {
        valid: |_, _, state| state.berries() == Some(false),
        success: always,
        perform: |level, pos, state| {
            if let Some(ripe) = state.with_berries(true) {
                level.set(pos, ripe, UPDATE_CLIENTS);
            }
        },
    };
    behaviours.register_bonemeal_kind("cave_vines", berries);
    behaviours.register_bonemeal_kind("cave_vines_plant", berries);
    behaviours.register_bonemeal_kind(
        "feature_placer",
        Bonemeal {
            valid: |level, pos, _| is_air(level.state(above(pos))),
            success: always,
            perform: |level, pos, state| {
                let Some(feature) = state.block().grown_feature() else {
                    return;
                };
                level.with_features(pos, &mut |features: &mut Features| {
                    features.place_configured(feature, above(pos))
                });
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "rooted_dirt",
        Bonemeal {
            valid: |level, pos, _| {
                is_air(level.state(below(pos))) && level.is_inside_build_height(pos.y - 1)
            },
            success: always,
            perform: |level, pos, _| {
                level.set(below(pos), Block::HangingRoots.default_state(), UPDATE_ALL);
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "nylium",
        Bonemeal {
            valid: |level, pos, _| {
                is_air(level.state(above(pos))) && level.is_inside_build_height(pos.y + 1)
            },
            success: always,
            perform: |level, pos, _| {
                let target = above(pos);
                if level.is_inside_build_height(target.y) {
                    level.with_features(target, &mut |features: &mut Features| {
                        features.place_configured("minecraft:nylium_bonemeal", target)
                    });
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "netherrack",
        Bonemeal {
            valid: |level, pos, _| {
                let over = level.state(above(pos));
                !over.is_shape_full_block()
                    && over.fluid_state().is_empty()
                    && !nylium_around(level, pos).is_empty()
            },
            success: always,
            perform: |level, pos, _| {
                let found = nylium_around(level, pos);
                let (red, blue) = (
                    found.contains(&Block::CrimsonNylium),
                    found.contains(&Block::WarpedNylium),
                );
                let block = match (red, blue) {
                    (true, true) if level.random_bool() => Block::WarpedNylium,
                    (true, true) => Block::CrimsonNylium,
                    (false, true) => Block::WarpedNylium,
                    (true, false) => Block::CrimsonNylium,
                    (false, false) => return,
                };
                level.set(pos, block.default_state(), UPDATE_ALL);
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "nether_fungus",
        Bonemeal {
            valid: |level, pos, state| {
                fungus_base(state.block()) == Some(level.state(below(pos)).block())
                    && level.is_inside_build_height(pos.y + 1)
            },
            success: |level, _, _| level.random_float() < 0.4,
            perform: |level, pos, state| {
                let Some(feature) = state.block().grown_feature() else {
                    return;
                };
                level.with_features(pos, &mut |features: &mut Features| {
                    features.region.set_block(pos, Block::Air.default_state());
                    if features.place_configured(feature, pos) {
                        return true;
                    }
                    features.region.set_block(pos, state);
                    false
                });
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "spreading_bush",
        Bonemeal {
            valid: |level, pos, state| spreadable_neighbour(level, pos, state, false).is_some(),
            success: always,
            perform: |level, pos, state| {
                if let Some(at) = spreadable_neighbour(level, pos, state, true) {
                    level.set(at, state.block().default_state(), UPDATE_ALL);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "short_dry_grass",
        Bonemeal {
            valid: |_, _, _| true,
            success: always,
            perform: |level, pos, _| {
                level.set(pos, Block::TallDryGrass.default_state(), UPDATE_ALL);
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "tall_dry_grass",
        Bonemeal {
            valid: |level, pos, _| {
                spreadable_neighbour(level, pos, Block::ShortDryGrass.default_state(), false)
                    .is_some()
            },
            success: always,
            perform: |level, pos, _| {
                let short = Block::ShortDryGrass.default_state();
                if let Some(at) = spreadable_neighbour(level, pos, short, true) {
                    level.set(at, short, UPDATE_ALL);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "hanging_moss",
        Bonemeal {
            valid: |level, pos, state| {
                let grow = below(moss_tip(level, pos, state.block()));
                is_air(level.state(grow)) && level.is_inside_build_height(grow.y)
            },
            success: always,
            perform: |level, pos, state| {
                let grow = below(moss_tip(level, pos, state.block()));
                if is_air(level.state(grow))
                    && let Some(tip) = state.with_tip(true)
                {
                    level.set(grow, tip, UPDATE_ALL);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "mangrove_leaves",
        Bonemeal {
            valid: |level, pos, _| is_air(level.state(below(pos))),
            success: always,
            perform: |level, pos, _| {
                if let Some(propagule) = Block::MangrovePropagule
                    .default_state()
                    .with_hanging(true)
                    .and_then(|state| state.with_age(0))
                {
                    level.set(below(pos), propagule, UPDATE_CLIENTS);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "shelf_mushroom",
        Bonemeal {
            valid: |_, _, state| state.age().is_some_and(|age| age < 1),
            success: always,
            perform: |level, pos, state| {
                if let Some(older) = state.with_age(state.age().unwrap_or(0) + 1) {
                    level.set(pos, older, UPDATE_CLIENTS);
                }
            },
        },
    );
}

fn nylium_around(level: &dyn GrowthLevel, pos: BlockPos) -> Vec<Block> {
    let mut found = Vec::new();
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let block = level.state(offset(pos, dx, dy, dz)).block();
                if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::NYLIUM, block)
                    && !found.contains(&block)
                {
                    found.push(block);
                }
            }
        }
    }
    found
}

fn fungus_base(block: Block) -> Option<Block> {
    match block {
        Block::CrimsonFungus => Some(Block::CrimsonNylium),
        Block::WarpedFungus => Some(Block::WarpedNylium),
        _ => None,
    }
}

fn moss_tip(level: &dyn GrowthLevel, pos: BlockPos, block: Block) -> BlockPos {
    let mut at = pos;
    while level.state(below(at)).block() == block {
        at = below(at);
    }
    at
}

/// A horizontal neighbour of `pos` where `state` could be placed, taking
/// the directions in shuffled order when `shuffle` is set.
fn spreadable_neighbour(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    state: BlockState,
    shuffle: bool,
) -> Option<BlockPos> {
    let mut directions = super::HORIZONTAL;
    if shuffle {
        for i in (1..directions.len()).rev() {
            let j = level.random_int(i as i32 + 1) as usize;
            directions.swap(i, j);
        }
    }
    let state = state.block().default_state();
    directions
        .into_iter()
        .map(|d| step(pos, d))
        .find(|&at| level.is_empty(at) && super::can_survive(level, state, at))
}

/// Uses bone meal on the block at `pos`, clicked on `face`. Returns where
/// it grew something (for the particles), or `None` if it does nothing
/// there and is not used up.
pub fn apply(
    behaviours: &Behaviours,
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    face: Facing,
) -> Option<BlockPos> {
    let state = level.block(pos)?;
    if let Some(bonemeal) = behaviours.bonemeal(state.block())
        && (bonemeal.valid)(level, pos, state)
    {
        if (bonemeal.success)(level, pos, state) {
            (bonemeal.perform)(level, pos, state);
        }
        return Some(pos);
    }
    let target = step(pos, face);
    if state.is_face_sturdy(face, tuff_vanilla_data::Support::Full)
        && grow_water_plant(level, target, Some(face))
    {
        return Some(target);
    }
    None
}

fn pop_self(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if let Some(item) = state.block().item() {
        level.drop_item(pos, tuff_item::item::ItemStack::new(item, 1));
    }
}

fn place_double(level: &mut dyn GrowthLevel, state: BlockState, pos: BlockPos) {
    let water = |level: &dyn GrowthLevel, at: BlockPos| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::WATER,
            level.state(at).fluid_state().fluid(),
        )
    };
    for (at, half) in [(pos, Half::Lower), (above(pos), Half::Upper)] {
        let mut half_state = state.with_half(half).unwrap_or(state);
        if let Some(wet) = half_state.with_waterlogged(water(level, at)) {
            half_state = wet;
        }
        level.set(at, half_state, UPDATE_CLIENTS);
    }
}

fn sea_pickles(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let mut z_span = 1;
    let mut z_offset = 0;
    let x_start = pos.x - 2;
    for (count, x) in (0..5).enumerate() {
        for z in 0..z_span {
            let end_y = 2 + pos.y - 1;
            for y in end_y - 2..end_y {
                let at = BlockPos::new(x_start + x, y, pos.z - z_offset + z);
                if at == pos || level.random_int(6) != 0 {
                    continue;
                }
                if level.state(at).block() != Block::Water {
                    continue;
                }
                if tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::CORAL_BLOCKS,
                    level.state(below(at)).block(),
                ) {
                    let pickles = level.random_int(4) as u8 + 1;
                    if let Some(pickle) = Block::SeaPickle.default_state().with_pickles(pickles) {
                        level.set(at, pickle, UPDATE_ALL);
                    }
                }
            }
        }
        if count < 2 {
            z_span += 2;
            z_offset += 1;
        } else {
            z_span -= 2;
            z_offset -= 1;
        }
    }
    if let Some(full) = state.with_pickles(4) {
        level.set(pos, full, UPDATE_CLIENTS);
    }
}

/// The configured features a biome places, in the biome's feature order.
fn biome_features(biome: &str) -> &'static [&'static str] {
    static BY_BIOME: OnceLock<HashMap<&'static str, Vec<&'static str>>> = OnceLock::new();
    let by_biome = BY_BIOME.get_or_init(|| {
        tuff_vanilla_data::biome::ALL
            .iter()
            .map(|(name, biome)| {
                let features = biome
                    .features
                    .iter()
                    .flat_map(|step| step.iter())
                    .filter_map(|placed| configured_of(placed))
                    .collect();
                (*name, features)
            })
            .collect()
    });
    by_biome.get(biome).map_or(&[], Vec::as_slice)
}

/// The configured feature a placed feature places.
fn configured_of(placed: &str) -> Option<&'static str> {
    let table = tuff_vanilla_data::worldgen::PLACED_FEATURES;
    let index = table.binary_search_by(|(name, _)| name.cmp(&placed)).ok()?;
    let json: serde_json::Value = serde_json::from_str(table[index].1).ok()?;
    let name = super::qualify(json.get("feature")?.as_str()?);
    let configured = tuff_vanilla_data::worldgen::CONFIGURED_FEATURES;
    let index = configured
        .binary_search_by(|(candidate, _)| (*candidate).cmp(name.as_str()))
        .ok()?;
    Some(configured[index].0)
}

fn grass(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    level.with_features(pos, &mut |features: &mut Features| {
        grass_in(features, pos, state.block());
        true
    });
}

fn grass_in(features: &mut Features, pos: BlockPos, grass: Block) {
    let start = above(pos);
    for attempt in 0..128 {
        let mut test = start;
        let mut stopped = false;
        for _ in 0..attempt / 16 {
            let random = &mut *features.random;
            let dx = random.next_int_between_inclusive(-1, 1);
            let dy = random.next_int_between_inclusive(-1, 1) * random.next_int_bounded(3) / 2;
            let dz = random.next_int_between_inclusive(-1, 1);
            test = offset(test, dx, dy, dz);
            let region = &*features.region;
            if region.block(below(test)).block() != grass
                || region.block(test).is_collision_full_block()
            {
                stopped = true;
                break;
            }
        }
        if !stopped {
            grass_effect(features, test);
        }
    }
}

fn grass_effect(features: &mut Features, test: BlockPos) {
    let state = features.region.block(test);
    if state.block() == Block::ShortGrass
        && features.random.next_float() < 0.1
        && let Some(tall) = state.block().grown_plant()
    {
        let region = &*features.region;
        let fits = placement::can_survive(tall.default_state(), test, &|p| region.block(p))
            && is_air(region.block(above(test)))
            && !region.is_outside_build_height(test.y + 1);
        if fits {
            for (at, half) in [(test, Half::Lower), (above(test), Half::Upper)] {
                let half_state = tall
                    .default_state()
                    .with_half(half)
                    .unwrap_or(tall.default_state());
                features.region.set_block(at, half_state);
            }
        }
    }
    if !is_air(state) || features.region.is_outside_build_height(test.y) {
        return;
    }
    if features.random.next_float() >= 0.125 {
        features.place_placed("minecraft:grass_bonemeal", test);
    } else {
        let biome = features.region.biome(test);
        let list: Vec<&str> = biome_features(tuff_world_model::chunk::biome_name(biome))
            .iter()
            .copied()
            .filter(|name| {
                tuff_datapack::tags::has(
                    tuff_vanilla_data::tag::worldgen::feature::key::CAN_SPAWN_FROM_BONE_MEAL,
                    name,
                )
            })
            .collect();
        if list.is_empty() {
            return;
        }
        let pick = list[features.random.next_int_bounded(list.len() as i32) as usize];
        features.place_configured(pick, test);
    }
}

/// A random member of the block tag `key` in the tags in effect; draws
/// nothing from an empty tag.
fn random_member(level: &mut dyn GrowthLevel, key: Key<Block>) -> Option<Block> {
    let members =
        tuff_datapack::tags::with(|tags| tags.typed::<Block>(key.name).unwrap_or_default());
    (!members.is_empty()).then(|| members[level.random_int(members.len() as i32) as usize])
}

/// Seagrass (or coral in warm oceans) spreading through the water in front
/// of a sturdy face.
pub fn grow_water_plant(level: &mut dyn GrowthLevel, pos: BlockPos, face: Option<Facing>) -> bool {
    let full_water = |level: &dyn GrowthLevel, at: BlockPos| {
        let state = level.state(at);
        state.block() == Block::Water && state.fluid_state().amount() == 8
    };
    if !full_water(level, pos) {
        return false;
    }
    for attempt in 0..128 {
        let mut test = pos;
        let mut blocked = false;
        for _ in 0..attempt / 16 {
            let dx = level.random_int(3) - 1;
            let dy = (level.random_int(3) - 1) * level.random_int(3) / 2;
            let dz = level.random_int(3) - 1;
            test = offset(test, dx, dy, dz);
            if level.state(test).is_collision_full_block() {
                blocked = true;
                break;
            }
        }
        if blocked {
            continue;
        }
        let mut grow = Block::Seagrass.default_state();
        let corals = level.biome(test).is_some_and(|biome| {
            tuff_datapack::tags::has(
                tuff_vanilla_data::tag::worldgen::biome::key::PRODUCES_CORALS_FROM_BONEMEAL,
                tuff_world_model::chunk::biome_name(biome),
            )
        });
        if corals {
            if attempt == 0 && face.is_some_and(Facing::is_horizontal) {
                if let Some(wall) =
                    random_member(level, tuff_vanilla_data::tag::block::key::WALL_CORALS)
                {
                    grow = wall.default_state();
                }
                if let Some(face) = face
                    && let Some(faced) = grow.with_facing(face)
                {
                    grow = faced;
                }
            } else if level.random_int(4) == 0
                && let Some(plant) = random_member(
                    level,
                    tuff_vanilla_data::tag::block::key::UNDERWATER_BONEMEALS,
                )
            {
                grow = plant.default_state();
            }
        }
        if tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::WALL_CORALS,
            grow.block(),
        ) && grow.facing().is_some()
        {
            let mut tries = 0;
            while !super::can_survive(level, grow, test) && tries < 4 {
                let facing = super::HORIZONTAL[level.random_int(4) as usize];
                grow = grow.with_facing(facing).unwrap_or(grow);
                tries += 1;
            }
        }
        if !super::can_survive(level, grow, test) {
            continue;
        }
        let current = level.state(test);
        if full_water(level, test) {
            level.set(test, grow, UPDATE_ALL);
            continue;
        }
        if current.block() == Block::Seagrass
            && level.state(above(test)).block() == Block::Water
            && level.random_int(10) == 0
        {
            let lower = Block::TallSeagrass.default_state();
            level.set(test, lower, UPDATE_CLIENTS);
            if let Some(upper) = lower.with_half(Half::Upper) {
                level.set(above(test), upper, UPDATE_CLIENTS);
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_on_grass_grows_grass_and_flowers_around() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-8, 63, -8),
            BlockPos::new(8, 63, 8),
            Block::GrassBlock.default_state(),
        );
        let mut flowers = 0;
        let mut grass = 0;
        for _ in 0..6 {
            let used = apply(&behaviours, &mut level, BlockPos::new(0, 63, 0), Facing::Up);
            assert_eq!(used, Some(BlockPos::new(0, 63, 0)));
            level.put(BlockPos::new(0, 64, 0), Block::Air.default_state());
            flowers = level
                .blocks
                .values()
                .filter(|s| {
                    tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::SMALL_FLOWERS,
                        s.block(),
                    )
                })
                .count();
            grass = level.count(Block::ShortGrass);
        }
        assert!(grass > 10, "{grass} grass");
        assert!(flowers > 0, "no flowers");
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_ages_crops_and_is_not_used_up_on_ripe_ones() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(below(pos), Block::Farmland.default_state());
        level.put(pos, Block::Carrots.default_state());
        assert!(apply(&behaviours, &mut level, pos, Facing::Up).is_some());
        let age = level.state(pos).age().unwrap();
        assert!((2..=5).contains(&age));
        level.put(pos, Block::Carrots.default_state().with_age(7).unwrap());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_in_water_grows_seagrass() {
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-6, 60, -6),
            BlockPos::new(6, 60, 6),
            Block::Sand.default_state(),
        );
        level.fill(
            BlockPos::new(-6, 61, -6),
            BlockPos::new(6, 63, 6),
            Block::Water.default_state(),
        );
        assert!(grow_water_plant(
            &mut level,
            BlockPos::new(0, 61, 0),
            Some(Facing::Up)
        ));
        assert!(level.count(Block::Seagrass) > 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_turns_netherrack_beside_nylium_into_it() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::Netherrack.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), None);
        level.put(BlockPos::new(1, 64, 0), Block::WarpedNylium.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), Some(pos));
        assert_eq!(level.state(pos).block(), Block::WarpedNylium);
        level.put(pos, Block::Netherrack.default_state());
        level.put(above(pos), Block::Stone.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), None);
        level.put(above(pos), Block::Glass.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), None);
        level.put(above(pos), Block::StoneSlab.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), Some(pos));
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_hangs_a_propagule_under_mangrove_leaves() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::MangroveLeaves.default_state());
        level.put(below(pos), Block::Stone.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), None);
        level.put(below(pos), Block::Air.default_state());
        assert!(apply(&behaviours, &mut level, pos, Facing::Up).is_some());
        let hung = level.state(below(pos));
        assert_eq!(hung.block(), Block::MangrovePropagule);
        assert_eq!(hung.hanging(), Some(true));
        assert_eq!(hung.age(), Some(0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_always_ages_a_hanging_propagule() {
        let behaviours = Behaviours::builtin();
        let pos = BlockPos::new(0, 64, 0);
        for _ in 0..40 {
            let mut level = TestLevel::new();
            level.put(above(pos), Block::MangroveLeaves.default_state());
            level.put(
                pos,
                Block::MangrovePropagule
                    .default_state()
                    .with_hanging(true)
                    .unwrap()
                    .with_age(1)
                    .unwrap(),
            );
            assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), Some(pos));
            assert_eq!(level.state(pos).age(), Some(2));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_spreads_bushes_and_dry_grass_sideways() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-3, 63, -3),
            BlockPos::new(3, 63, 3),
            Block::Dirt.default_state(),
        );
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::Bush.default_state());
        assert_eq!(apply(&behaviours, &mut level, pos, Facing::Up), Some(pos));
        assert_eq!(level.count(Block::Bush), 2);
        level.put(pos, Block::ShortDryGrass.default_state());
        apply(&behaviours, &mut level, pos, Facing::Up);
        assert_eq!(level.state(pos).block(), Block::TallDryGrass);
        let before = level.count(Block::ShortDryGrass);
        apply(&behaviours, &mut level, pos, Facing::Up);
        assert_eq!(level.count(Block::ShortDryGrass), before + 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_extends_hanging_moss_and_ages_shelf_mushrooms() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        let top = BlockPos::new(0, 70, 0);
        level.put(above(top), Block::Stone.default_state());
        level.put(top, Block::PaleHangingMoss.default_state());
        assert_eq!(apply(&behaviours, &mut level, top, Facing::Up), Some(top));
        assert_eq!(level.state(below(top)).block(), Block::PaleHangingMoss);
        assert_eq!(level.state(below(top)).tip(), Some(true));

        let shelf = BlockPos::new(5, 70, 0);
        level.put(shelf, Block::ShelfMushroom.default_state());
        assert_eq!(
            apply(&behaviours, &mut level, shelf, Facing::Up),
            Some(shelf)
        );
        assert_eq!(level.state(shelf).age(), Some(1));
        assert_eq!(apply(&behaviours, &mut level, shelf, Facing::Up), None);
    }
}
