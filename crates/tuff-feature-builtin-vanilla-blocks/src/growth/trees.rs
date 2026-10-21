//! Saplings growing into trees and mushrooms into huge mushrooms, through
//! the world generation features the game grows them with.

use tuff_vanilla_data::growth::TreeGrower;
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, Bonemeal, Features, GrowthLevel, GrowthLevelExt, above, aged, offset};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS, UPDATE_KNOWN_SHAPE, UPDATE_NONE};
use tuff_item::random::RandomSource;
use tuff_vanilla_common::placement;
use tuff_world_model::BlockPos;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("sapling", sapling_tick);
    behaviours.register_random_kind("propagule", propagule_tick);

    let sapling = Bonemeal {
        valid: |level, pos, state| {
            state.block().tree_grower().is_some_and(|grower| {
                can_grow(level, grower, pos, state)
                    && level.is_inside_build_height(pos.y + trunk_base_height(grower))
            })
        },
        success: |level, _, _| (level.random_float() as f64) < 0.45,
        perform: advance_tree,
    };
    behaviours.register_bonemeal_kind("sapling", sapling);
    behaviours.register_bonemeal_kind(
        "propagule",
        Bonemeal {
            valid: |_, _, state| !(state.hanging() == Some(true) && fully_grown(state)),
            success: |level, _, state| {
                state.hanging() == Some(true) || (level.random_float() as f64) < 0.45
            },
            perform: |level, pos, state| {
                if state.hanging() == Some(true) && !fully_grown(state) {
                    level.set(
                        pos,
                        aged(state, state.age().unwrap_or(0) + 1),
                        UPDATE_CLIENTS,
                    );
                } else {
                    advance_tree(level, pos, state);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "azalea",
        Bonemeal {
            valid: |level, pos, _| {
                let reach = TreeGrower::by_name("azalea").map_or(0, trunk_base_height) + 2;
                level.is_inside_build_height(pos.y + reach)
                    && level.state(above(pos)).fluid_state().is_empty()
            },
            success: |level, _, _| (level.random_float() as f64) < 0.45,
            perform: |level, pos, state| {
                if let Some(grower) = TreeGrower::by_name("azalea") {
                    grow_tree(level, grower, pos, state);
                }
            },
        },
    );
    behaviours.register_bonemeal_kind(
        "mushroom",
        Bonemeal {
            valid: |level, pos, state| {
                state.block().grown_feature().is_some_and(|feature| {
                    level.is_inside_build_height(pos.y + 4 + foliage_radius(feature))
                })
            },
            success: |level, _, _| (level.random_float() as f64) < 0.4,
            perform: grow_mushroom,
        },
    );
}

const PROPAGULE_MAX_AGE: u8 = 4;

fn configured(name: &str) -> Option<serde_json::Value> {
    let name = super::qualify(name);
    let table = tuff_vanilla_data::worldgen::CONFIGURED_FEATURES;
    let index = table
        .binary_search_by(|(key, _)| (*key).cmp(name.as_str()))
        .ok()?;
    serde_json::from_str(table[index].1).ok()
}

/// The base height of the grower's shortest tree, zero if it has none.
fn trunk_base_height(grower: &TreeGrower) -> i32 {
    grower
        .shortest
        .and_then(configured)
        .and_then(|json| json.pointer("/trunk_placer/base_height")?.as_i64())
        .map_or(0, |height| height as i32)
}

fn foliage_radius(feature: &str) -> i32 {
    configured(feature)
        .and_then(|json| json.pointer("/foliage_radius")?.as_i64())
        .map_or(2, |radius| radius as i32)
}

fn fully_grown(state: BlockState) -> bool {
    state.age() == Some(PROPAGULE_MAX_AGE)
}

fn sapling_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.max_local_raw_brightness(above(pos)) >= 9 && level.random_int(7) == 0 {
        advance_tree(level, pos, state);
    }
}

fn propagule_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.hanging() != Some(true) {
        if level.random_int(7) == 0 {
            advance_tree(level, pos, state);
        }
    } else if !fully_grown(state) {
        level.set(
            pos,
            aged(state, state.age().unwrap_or(0) + 1),
            UPDATE_CLIENTS,
        );
    }
}

/// A sapling first ripens (stage 1), then grows into its tree.
pub fn advance_tree(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.stage() == Some(0) {
        if let Some(ripe) = state.with_stage(1) {
            level.set(pos, ripe, UPDATE_NONE);
        }
        return;
    }
    if let Some(grower) = state.block().tree_grower() {
        grow_tree(level, grower, pos, state);
    }
}

type Weighted = &'static [(&'static str, u32)];

fn pick(random: &mut dyn FnMut(i32) -> i32, list: Weighted) -> Option<&'static str> {
    let total: u32 = list.iter().map(|(_, weight)| weight).sum();
    if total == 0 {
        return None;
    }
    let mut selection = random(total as i32) as u32;
    for (feature, weight) in list {
        if selection < *weight {
            return Some(feature);
        }
        selection -= weight;
    }
    None
}

fn has_flowers(block: &dyn Fn(BlockPos) -> BlockState, pos: BlockPos) -> bool {
    for dx in -2..=2 {
        for dy in -1..=1 {
            for dz in -2..=2 {
                let found = block(offset(pos, dx, dy, dz)).block();
                if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::FLOWERS, found) {
                    return true;
                }
            }
        }
    }
    false
}

const SQUARE: [(i32, i32); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];

/// The corner of a 2x2 of saplings `pos` is part of, as an offset.
fn two_by_two(
    block: &dyn Fn(BlockPos) -> BlockState,
    state: BlockState,
    pos: BlockPos,
) -> Option<(i32, i32)> {
    for dx in [0, -1] {
        for dz in [0, -1] {
            let all = SQUARE
                .iter()
                .all(|(x, z)| block(offset(pos, dx + x, 0, dz + z)).block() == state.block());
            if all {
                return Some((dx, dz));
            }
        }
    }
    None
}

fn tree_list(grower: &'static TreeGrower, flowers: bool) -> Weighted {
    if flowers && !grower.flower.is_empty() {
        grower.flower
    } else {
        grower.trees
    }
}

/// Whether bone meal could grow the tree: growers with only 2x2 trees
/// need the 2x2.
fn can_grow(
    level: &mut dyn GrowthLevel,
    grower: &'static TreeGrower,
    pos: BlockPos,
    state: BlockState,
) -> bool {
    let list = tree_list(grower, has_flowers(&|p| level.state(p), pos));
    let tree = pick(&mut |bound| level.random_int(bound), list);
    let mega = pick(&mut |bound| level.random_int(bound), grower.mega);
    if tree.is_none() && mega.is_some() {
        return two_by_two(&|p| level.state(p), state, pos).is_some();
    }
    true
}

/// Grows the grower's tree at `pos` (a 2x2 tree if the saplings form one
/// and the grower has them), putting the saplings back if it fails.
pub fn grow_tree(
    level: &mut dyn GrowthLevel,
    grower: &'static TreeGrower,
    pos: BlockPos,
    state: BlockState,
) -> bool {
    level.with_features_flagged(
        pos,
        UPDATE_ALL | UPDATE_KNOWN_SHAPE,
        &mut |features: &mut Features| grow_in(features, grower, pos, state),
    )
}

fn grow_in(
    features: &mut Features,
    grower: &'static TreeGrower,
    pos: BlockPos,
    state: BlockState,
) -> bool {
    let random = &mut *features.random;
    let mega = pick(&mut |bound| random.next_int_bounded(bound), grower.mega);
    if let Some(mega) = mega {
        let corner = {
            let region = &*features.region;
            two_by_two(&|p| region.block(p), state, pos)
        };
        if let Some((dx, dz)) = corner {
            let saplings: Vec<(BlockPos, BlockState)> = SQUARE
                .iter()
                .map(|(x, z)| {
                    let at = offset(pos, dx + x, 0, dz + z);
                    (at, features.region.block(at))
                })
                .collect();
            for (at, sapling) in &saplings {
                features.region.set_block(*at, placement::remains(*sapling));
            }
            if features.place_configured(mega, offset(pos, dx, 0, dz)) {
                return true;
            }
            for (at, sapling) in saplings {
                features.region.set_block(at, sapling);
            }
            return false;
        }
    }
    let flowers = {
        let region = &*features.region;
        has_flowers(&|p| region.block(p), pos)
    };
    let random = &mut *features.random;
    let Some(tree) = pick(
        &mut |bound| random.next_int_bounded(bound),
        tree_list(grower, flowers),
    ) else {
        return false;
    };
    features.region.set_block(pos, placement::remains(state));
    if features.place_configured(tree, pos) {
        return true;
    }
    features.region.set_block(pos, state);
    false
}

/// A mushroom grows into its huge mushroom, or stays if there is no room.
pub fn grow_mushroom(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
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
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn ground(level: &mut TestLevel) {
        level.fill(
            BlockPos::new(-6, 63, -6),
            BlockPos::new(6, 63, 6),
            Block::GrassBlock.default_state(),
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn grown_bee_nests_carry_their_bees() {
        let mut level = TestLevel::new();
        ground(&mut level);
        let pos = BlockPos::new(0, 64, 0);
        let grown = level.with_features(pos, &mut |features: &mut Features| {
            features.place_configured("minecraft:fancy_oak_bees", pos)
        });
        assert!(grown);
        assert_eq!(level.count(Block::BeeNest), 1);
        let bees = level
            .merged
            .iter()
            .find_map(|(_, data)| data.get_list("bees"));
        assert!(bees.is_some_and(|bees| bees.len() >= 2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn bone_meal_needs_room_below_the_build_limit() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        let high = BlockPos::new(0, 318, 0);
        level.put(high, Block::OakSapling.default_state());
        let bonemeal = behaviours.bonemeal(Block::OakSapling).unwrap();
        let state = level.state(high);
        assert!(!(bonemeal.valid)(&mut level, high, state));
        let low = BlockPos::new(0, 100, 0);
        level.put(low, Block::OakSapling.default_state());
        let state = level.state(low);
        assert!((bonemeal.valid)(&mut level, low, state));
    }

    #[tuff_core::watchdog]
    #[test]
    fn azalea_bone_meal_needs_room_below_the_build_limit() {
        let behaviours = Behaviours::builtin();
        let mut level = TestLevel::new();
        let high = BlockPos::new(0, 318, 0);
        level.put(high, Block::Azalea.default_state());
        let bonemeal = behaviours.bonemeal(Block::Azalea).unwrap();
        let state = level.state(high);
        assert!(!(bonemeal.valid)(&mut level, high, state));
        let low = BlockPos::new(0, 100, 0);
        level.put(low, Block::Azalea.default_state());
        let state = level.state(low);
        assert!((bonemeal.valid)(&mut level, low, state));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_ripe_oak_sapling_grows_into_an_oak() {
        let mut level = TestLevel::new();
        ground(&mut level);
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::OakSapling.default_state());
        level.run(advance_tree, pos);
        assert_eq!(level.state(pos).stage(), Some(1));
        level.run(advance_tree, pos);
        assert_eq!(level.state(pos).block(), Block::OakLog);
        assert!(level.count(Block::OakLeaves) > 10);
        assert_eq!(level.state(BlockPos::new(0, 63, 0)).block(), Block::Dirt);
    }

    #[tuff_core::watchdog]
    #[test]
    fn four_spruce_saplings_grow_a_mega_spruce() {
        let mut level = TestLevel::new();
        ground(&mut level);
        let ripe = Block::SpruceSapling.default_state().with_stage(1).unwrap();
        for (x, z) in SQUARE {
            level.put(BlockPos::new(x, 64, z), ripe);
        }
        level.run(advance_tree, BlockPos::new(1, 64, 1));
        let logs = level
            .blocks
            .iter()
            .filter(|(pos, state)| state.block() == Block::SpruceLog && pos.y == 64)
            .count();
        assert_eq!(logs, 4, "a two by two trunk");
        assert_eq!(level.count(Block::SpruceSapling), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lone_dark_oak_sapling_does_not_grow() {
        let mut level = TestLevel::new();
        ground(&mut level);
        let pos = BlockPos::new(0, 64, 0);
        let ripe = Block::DarkOakSapling.default_state().with_stage(1).unwrap();
        level.put(pos, ripe);
        level.run(advance_tree, pos);
        assert_eq!(level.state(pos), ripe);
        assert_eq!(level.count(Block::DarkOakLog), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn mushrooms_grow_huge_on_mycelium() {
        let mut level = TestLevel::new();
        level.fill(
            BlockPos::new(-4, 63, -4),
            BlockPos::new(4, 63, 4),
            Block::Mycelium.default_state(),
        );
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::RedMushroom.default_state());
        level.run(grow_mushroom, pos);
        assert_eq!(level.state(pos).block(), Block::MushroomStem);
        assert!(level.count(Block::RedMushroomBlock) > 0);
    }
}
