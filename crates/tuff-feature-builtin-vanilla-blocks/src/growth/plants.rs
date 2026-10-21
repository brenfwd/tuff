//! Plants that grow taller or spread: sugar cane, cactus, bamboo, kelp and
//! the other growing plants, vines, mushrooms and amethyst buds.

use tuff_vanilla_data::block::{Facing, Leaves};
use tuff_vanilla_data::{Block, BlockState};

use super::{
    Behaviours, Bonemeal, DIRECTIONS, GrowthLevel, GrowthLevelExt, HORIZONTAL, above, aged, always,
    below, can_survive, offset, survives,
};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS, UPDATE_NONE};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("sugar_cane", sugar_cane_tick);
    behaviours.register_random_kind("cactus", cactus_tick);
    behaviours.register_random_kind("bamboo", bamboo_tick);
    behaviours.register_random_kind("bamboo_sapling", bamboo_sapling_tick);
    behaviours.register_random_kind("growing_plant_head", head_tick);
    behaviours.register_random_kind("kelp", head_tick);
    behaviours.register_random_kind("cave_vines", head_tick);
    behaviours.register_random_kind("nether_vines", head_tick);
    behaviours.register_random_kind("vine", vine_tick);
    behaviours.register_random_kind("mushroom", mushroom_tick);
    behaviours.register_random_kind("budding_amethyst", budding_amethyst_tick);
    behaviours.register_random_kind("chorus_flower", chorus_flower_tick);

    behaviours.register_bonemeal_kind(
        "bamboo",
        Bonemeal {
            valid: |level, pos, _| {
                let height_above = bamboo_height(level, pos, Facing::Up);
                let height_below = bamboo_height(level, pos, Facing::Down);
                let top = level.state(offset(pos, 0, height_above, 0));
                let growth = offset(pos, 0, height_above + 1, 0);
                height_above + height_below + 1 < 16
                    && top.stage() != Some(1)
                    && level.is_inside_build_height(growth.y)
                    && level.is_empty(growth)
            },
            success: always,
            perform: bamboo_bonemeal,
        },
    );
    behaviours.register_bonemeal_kind(
        "bamboo_sapling",
        Bonemeal {
            valid: |level, pos, _| {
                is_air(level.state(above(pos))) && level.is_inside_build_height(pos.y + 1)
            },
            success: always,
            perform: |level, pos, _| grow_bamboo_sapling(level, pos),
        },
    );
    let head = Bonemeal {
        valid: head_valid,
        success: always,
        perform: head_bonemeal,
    };
    behaviours.register_bonemeal_kind("growing_plant_head", head);
    behaviours.register_bonemeal_kind("kelp", head);
    behaviours.register_bonemeal_kind("nether_vines", head);
    behaviours.register_bonemeal_kind(
        "growing_plant_body",
        Bonemeal {
            valid: |level, pos, state| {
                let Some(head) = head_of(level, pos, state) else {
                    return false;
                };
                let head_state = level.state(head);
                head_valid(level, head, head_state)
            },
            success: always,
            perform: |level, pos, state| {
                if let Some(head) = head_of(level, pos, state) {
                    let head_state = level.state(head);
                    head_bonemeal(level, head, head_state);
                }
            },
        },
    );
}

fn sugar_cane_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if !level.is_empty(above(pos)) {
        return;
    }
    let block = state.block();
    let mut height = 1;
    while level.state(offset(pos, 0, -height, 0)).block() == block {
        height += 1;
    }
    if height >= 3 {
        return;
    }
    let age = state.age().unwrap_or(0);
    if age == 15 {
        level.set(above(pos), block.default_state(), UPDATE_ALL);
        level.set(pos, aged(state, 0), UPDATE_NONE);
    } else {
        level.set(pos, aged(state, age + 1), UPDATE_NONE);
    }
}

fn cactus_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let top = above(pos);
    if !level.is_empty(top) {
        return;
    }
    let block = state.block();
    let age = state.age().unwrap_or(0);
    let mut height = 1;
    while level.state(offset(pos, 0, -height, 0)).block() == block {
        height += 1;
        if height == 3 && age == 15 {
            return;
        }
    }
    if age == 8 && can_survive(level, block.default_state(), top) {
        let chance = if height >= 3 { 0.25 } else { 0.1 };
        if level.random_double() <= chance {
            level.set(top, Block::CactusFlower.default_state(), UPDATE_ALL);
        }
    } else if age == 15 && height < 3 {
        level.set(top, block.default_state(), UPDATE_ALL);
        level.set(pos, aged(state, 0), UPDATE_NONE);
    }
    if age < 15 {
        let current = level.state(pos);
        if current.block() == block {
            level.set(pos, aged(current, age + 1), UPDATE_NONE);
        }
    }
}

fn bamboo_height(level: &dyn GrowthLevel, pos: BlockPos, direction: Facing) -> i32 {
    let dy = if direction == Facing::Up { 1 } else { -1 };
    let mut height = 0;
    while height < 16 && level.state(offset(pos, 0, dy * (height + 1), 0)).block() == Block::Bamboo
    {
        height += 1;
    }
    height
}

fn grow_bamboo(level: &mut dyn GrowthLevel, state: BlockState, pos: BlockPos, height: i32) {
    let below_state = level.state(below(pos));
    let two_below = offset(pos, 0, -2, 0);
    let two_below_state = level.state(two_below);
    let mut leaves = Leaves::None;
    if height >= 1 {
        let leafy_below =
            below_state.block() == Block::Bamboo && below_state.leaves() != Some(Leaves::None);
        if !leafy_below {
            leaves = Leaves::Small;
        } else {
            leaves = Leaves::Large;
            if two_below_state.block() == Block::Bamboo {
                if let Some(small) = below_state.with_leaves(Leaves::Small) {
                    level.set(below(pos), small, UPDATE_ALL);
                }
                if let Some(none) = two_below_state.with_leaves(Leaves::None) {
                    level.set(two_below, none, UPDATE_ALL);
                }
            }
        }
    }
    let age = if state.age() == Some(1) || two_below_state.block() == Block::Bamboo {
        1
    } else {
        0
    };
    let stage = if (height < 11 || level.random_float() >= 0.25) && height != 15 {
        0
    } else {
        1
    };
    let grown = Block::Bamboo
        .default_state()
        .with_age(age)
        .and_then(|s| s.with_leaves(leaves))
        .and_then(|s| s.with_stage(stage));
    if let Some(grown) = grown {
        level.set(above(pos), grown, UPDATE_ALL);
    }
}

fn bamboo_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.stage() != Some(0) || level.random_int(3) != 0 {
        return;
    }
    if !level.is_empty(above(pos)) || level.raw_brightness(above(pos), 0) < 9 {
        return;
    }
    let height = bamboo_height(level, pos, Facing::Down) + 1;
    if height < 16 {
        grow_bamboo(level, state, pos, height);
    }
}

fn bamboo_bonemeal(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    let start_above = bamboo_height(level, pos, Facing::Up);
    let height_below = bamboo_height(level, pos, Facing::Down);
    let start_total = start_above + height_below + 1;
    let count = 1 + level.random_int(2);
    for step in 0..count {
        let height_above = start_above + step;
        let total = start_total + step;
        let top = offset(pos, 0, height_above, 0);
        let top_state = level.state(top);
        let growth = above(top);
        if total >= 16
            || top_state.stage() == Some(1)
            || !level.is_empty(growth)
            || !level.is_inside_build_height(growth.y)
        {
            return;
        }
        grow_bamboo(level, top_state, top, total);
    }
}

fn grow_bamboo_sapling(level: &mut dyn GrowthLevel, pos: BlockPos) {
    if let Some(bamboo) = Block::Bamboo.default_state().with_leaves(Leaves::Small) {
        level.set(above(pos), bamboo, UPDATE_ALL);
    }
}

fn bamboo_sapling_tick(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if level.random_int(3) == 0
        && level.is_empty(above(pos))
        && level.raw_brightness(above(pos), 0) >= 9
    {
        grow_bamboo_sapling(level, pos);
    }
}

const MAX_PLANT_AGE: u8 = 25;

/// Whether the head of `head` can grow into a block in `state`.
fn can_grow_into(head: Block, state: BlockState) -> bool {
    match head.random_tick_kind() {
        Some("kelp") => state.block() == Block::Water,
        _ => is_air(state),
    }
}

/// The head a growing plant grows into its next block with.
fn grow_into_state(level: &mut dyn GrowthLevel, from: BlockState) -> BlockState {
    let age = from.age().unwrap_or(0);
    let next = aged(from, if age >= MAX_PLANT_AGE { 0 } else { age + 1 });
    match from.block().random_tick_kind() {
        Some("cave_vines") => {
            let berries = level.random_float() < 0.11;
            next.with_berries(berries).unwrap_or(next)
        }
        _ => next,
    }
}

fn head_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let Some(plant) = state.block().growing_plant() else {
        return;
    };
    if state.age().unwrap_or(0) >= MAX_PLANT_AGE || level.random_double() >= plant.chance {
        return;
    }
    let growth = step(pos, plant.direction);
    if can_grow_into(state.block(), level.state(growth)) {
        let grown = grow_into_state(level, state);
        level.set(growth, grown, UPDATE_ALL);
    }
}

fn blocks_to_grow(level: &mut dyn GrowthLevel, head: Block) -> i32 {
    match head.bonemeal_kind() {
        Some("nether_vines") => {
            let mut probability = 1.0;
            let mut count = 0;
            while level.random_double() < probability {
                probability *= 0.826;
                count += 1;
            }
            count
        }
        _ => 1,
    }
}

fn head_bonemeal(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let Some(plant) = state.block().growing_plant() else {
        return;
    };
    let mut forward = step(pos, plant.direction);
    let mut age = (state.age().unwrap_or(0) + 1).min(MAX_PLANT_AGE);
    let count = blocks_to_grow(level, state.block());
    let mut grown = 0;
    while grown < count
        && can_grow_into(state.block(), level.state(forward))
        && level.is_inside_build_height(forward.y)
    {
        level.set(forward, aged(state, age), UPDATE_ALL);
        forward = step(forward, plant.direction);
        age = (age + 1).min(MAX_PLANT_AGE);
        grown += 1;
    }
}

fn head_valid(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) -> bool {
    let Some(plant) = state.block().growing_plant() else {
        return false;
    };
    let growth = step(pos, plant.direction);
    can_grow_into(state.block(), level.state(growth)) && level.is_inside_build_height(growth.y)
}

/// The head at the end of the plant a body block belongs to.
pub fn head_of(level: &dyn GrowthLevel, pos: BlockPos, state: BlockState) -> Option<BlockPos> {
    let plant = state.block().growing_plant()?;
    let mut at = pos;
    loop {
        at = step(at, plant.direction);
        let here = level.state(at).block();
        if here == plant.head {
            return Some(at);
        }
        if here != plant.body {
            return None;
        }
    }
}

fn vine_neighbour(level: &dyn GrowthLevel, pos: BlockPos, direction: Facing) -> bool {
    level
        .state(pos)
        .is_face_sturdy(direction.opposite(), tuff_vanilla_data::Support::Full)
}

fn vine_face(state: BlockState, direction: Facing) -> bool {
    state.get(direction.name()) == Some("true")
}

fn with_face(state: BlockState, direction: Facing, value: bool) -> BlockState {
    state
        .with(direction.name(), if value { "true" } else { "false" })
        .unwrap_or(state)
}

fn nearby_count_at_most(level: &dyn GrowthLevel, pos: BlockPos, block: Block, most: usize) -> bool {
    let mut found = 0;
    for dx in -4..=4 {
        for dy in -1..=1 {
            for dz in -4..=4 {
                if level.state(offset(pos, dx, dy, dz)).block() == block {
                    found += 1;
                    if found > most {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn vine_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if !level.rule_bool("spread_vines") || level.random_int(4) != 0 {
        return;
    }
    let block = state.block();
    let direction = DIRECTIONS[level.random_int(6) as usize];
    let up = above(pos);
    if direction.is_horizontal() && !vine_face(state, direction) {
        if !nearby_count_at_most(level, pos, block, 4) {
            return;
        }
        let target = step(pos, direction);
        let edge = level.state(target);
        if !is_air(edge) {
            if vine_neighbour(level, target, direction) {
                level.set(pos, with_face(state, direction, true), UPDATE_CLIENTS);
            }
            return;
        }
        let (cw, ccw) = (direction.clockwise(), direction.counter_clockwise());
        let (cw_face, ccw_face) = (vine_face(state, cw), vine_face(state, ccw));
        let (cw_target, ccw_target) = (step(target, cw), step(target, ccw));
        let fresh = block.default_state();
        if cw_face && vine_neighbour(level, cw_target, cw) {
            level.set(target, with_face(fresh, cw, true), UPDATE_CLIENTS);
            return;
        }
        if ccw_face && vine_neighbour(level, ccw_target, ccw) {
            level.set(target, with_face(fresh, ccw, true), UPDATE_CLIENTS);
            return;
        }
        let opposite = direction.opposite();
        if cw_face && level.is_empty(cw_target) && vine_neighbour(level, step(pos, cw), opposite) {
            level.set(cw_target, with_face(fresh, opposite, true), UPDATE_CLIENTS);
            return;
        }
        if ccw_face && level.is_empty(ccw_target) && vine_neighbour(level, step(pos, ccw), opposite)
        {
            level.set(ccw_target, with_face(fresh, opposite, true), UPDATE_CLIENTS);
            return;
        }
        if (level.random_float() as f64) < 0.05 && vine_neighbour(level, above(target), Facing::Up)
        {
            level.set(target, with_face(fresh, Facing::Up, true), UPDATE_CLIENTS);
        }
        return;
    }
    if direction == Facing::Up && pos.y < level.max_y() {
        if vine_neighbour(level, up, Facing::Up) {
            level.set(pos, with_face(state, Facing::Up, true), UPDATE_CLIENTS);
            return;
        }
        if level.is_empty(up) {
            if !nearby_count_at_most(level, pos, block, 4) {
                return;
            }
            let mut grown = state;
            for side in HORIZONTAL {
                if level.random_bool() || !vine_neighbour(level, step(up, side), side) {
                    grown = with_face(grown, side, false);
                }
            }
            if HORIZONTAL.into_iter().any(|side| vine_face(grown, side)) {
                level.set(up, grown, UPDATE_CLIENTS);
            }
            return;
        }
    }
    if pos.y > level.min_y() {
        let down = below(pos);
        let below_state = level.state(down);
        if is_air(below_state) || below_state.block() == block {
            let before = if is_air(below_state) {
                block.default_state()
            } else {
                below_state
            };
            let mut after = before;
            for side in HORIZONTAL {
                if level.random_bool() && vine_face(state, side) {
                    after = with_face(after, side, true);
                }
            }
            if before != after && HORIZONTAL.into_iter().any(|side| vine_face(after, side)) {
                level.set(down, after, UPDATE_CLIENTS);
            }
        }
    }
}

fn mushroom_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if level.random_int(25) != 0 || !nearby_count_at_most(level, pos, state.block(), 4) {
        return;
    }
    let mut pos = pos;
    let roll = |level: &mut dyn GrowthLevel, from: BlockPos| {
        let dx = level.random_int(3) - 1;
        let dy = level.random_int(2) - level.random_int(2);
        let dz = level.random_int(3) - 1;
        offset(from, dx, dy, dz)
    };
    let mut target = roll(level, pos);
    for _ in 0..4 {
        if level.is_empty(target) && survives(level, state, target) {
            pos = target;
        }
        target = roll(level, pos);
    }
    if level.is_empty(target) && survives(level, state, target) {
        level.set(target, state, UPDATE_CLIENTS);
    }
}

fn budding_amethyst_tick(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if level.random_int(5) != 0 {
        return;
    }
    let direction = DIRECTIONS[level.random_int(6) as usize];
    let target = step(pos, direction);
    let current = level.state(target);
    let facing = current.facing() == Some(direction);
    let next = if is_air(current)
        || (current.block() == Block::Water && current.fluid_state().amount() == 8)
    {
        Block::SmallAmethystBud
    } else if current.block() == Block::SmallAmethystBud && facing {
        Block::MediumAmethystBud
    } else if current.block() == Block::MediumAmethystBud && facing {
        Block::LargeAmethystBud
    } else if current.block() == Block::LargeAmethystBud && facing {
        Block::AmethystCluster
    } else {
        return;
    };
    let water = current.fluid_state().fluid() == tuff_vanilla_data::fluid::Fluid::Water;
    let grown = next
        .default_state()
        .with_facing(direction)
        .and_then(|s| s.with_waterlogged(water));
    if let Some(grown) = grown {
        level.set(target, grown, UPDATE_ALL);
    }
}

const CHORUS_GROW: i32 = 1033;
const CHORUS_DEATH: i32 = 1034;
const CHORUS_MAX_AGE: u8 = 5;

fn horizontal_neighbours_empty(
    level: &dyn GrowthLevel,
    pos: BlockPos,
    ignore: Option<Facing>,
) -> bool {
    HORIZONTAL
        .into_iter()
        .filter(|direction| Some(*direction) != ignore)
        .all(|direction| level.is_empty(step(pos, direction)))
}

fn chorus_plant_here(level: &mut dyn GrowthLevel, pos: BlockPos) {
    let plant = tuff_vanilla_common::placement::chorus_plant_state(pos, &|p| level.state(p));
    level.set(pos, plant, UPDATE_CLIENTS);
}

fn place_chorus_flower(level: &mut dyn GrowthLevel, flower: Block, pos: BlockPos, age: u8) {
    level.set(pos, aged(flower.default_state(), age), UPDATE_CLIENTS);
    level.level_event(CHORUS_GROW, pos, 0);
}

fn kill_chorus_flower(level: &mut dyn GrowthLevel, flower: Block, pos: BlockPos) {
    level.set(
        pos,
        aged(flower.default_state(), CHORUS_MAX_AGE),
        UPDATE_CLIENTS,
    );
    level.level_event(CHORUS_DEATH, pos, 0);
}

fn chorus_flower_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let flower = state.block();
    let up = above(pos);
    let age = state.age().unwrap_or(0);
    if !level.is_empty(up) || up.y > level.max_y() || age >= CHORUS_MAX_AGE {
        return;
    }
    let plant = Block::ChorusPlant;
    let supports = |state: BlockState| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::SUPPORTS_CHORUS_FLOWER,
            state.block(),
        )
    };
    let mut grow_up = false;
    let mut on_support = false;
    let below_state = level.state(below(pos));
    if supports(below_state) {
        grow_up = true;
    } else if below_state.block() == plant {
        let mut height = 1;
        for _ in 0..4 {
            let test = level.state(offset(pos, 0, -(height + 1), 0));
            if test.block() == plant {
                height += 1;
            } else {
                if supports(test) {
                    on_support = true;
                }
                break;
            }
        }
        if height < 2 || height <= level.random_int(if on_support { 5 } else { 4 }) {
            grow_up = true;
        }
    } else if is_air(below_state) {
        grow_up = true;
    }
    if grow_up
        && horizontal_neighbours_empty(level, up, None)
        && level.is_empty(offset(pos, 0, 2, 0))
    {
        chorus_plant_here(level, pos);
        place_chorus_flower(level, flower, up, age);
        return;
    }
    if age < 4 {
        let mut attempts = level.random_int(4);
        if on_support {
            attempts += 1;
        }
        let mut branched = false;
        for _ in 0..attempts {
            let direction = HORIZONTAL[level.random_int(4) as usize];
            let target = step(pos, direction);
            if level.is_empty(target)
                && level.is_empty(below(target))
                && horizontal_neighbours_empty(level, target, Some(direction.opposite()))
            {
                place_chorus_flower(level, flower, target, age + 1);
                branched = true;
            }
        }
        if branched {
            chorus_plant_here(level, pos);
        } else {
            kill_chorus_flower(level, flower, pos);
        }
        return;
    }
    kill_chorus_flower(level, flower, pos);
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn sugar_cane_grows_three_high() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 63, 0);
        level.put(ground, Block::Sand.default_state());
        level.put(BlockPos::new(1, 63, 0), Block::Water.default_state());
        let base = above(ground);
        level.put(base, Block::SugarCane.default_state());
        for _ in 0..200 {
            for y in 64..67 {
                let pos = BlockPos::new(0, y, 0);
                if level.state(pos).block() == Block::SugarCane {
                    level.run(sugar_cane_tick, pos);
                }
            }
        }
        assert_eq!(level.count(Block::SugarCane), 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn kelp_grows_only_into_water() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 60, 0);
        level.put(below(pos), Block::Sand.default_state());
        level.put(pos, Block::Kelp.default_state());
        level.put(above(pos), Block::Water.default_state());
        for _ in 0..200 {
            level.run(head_tick, pos);
        }
        assert_eq!(level.state(above(pos)).block(), Block::Kelp);
        assert_eq!(level.state(above(pos)).age(), Some(1));
        let top = above(above(pos));
        for _ in 0..200 {
            level.run(head_tick, above(pos));
        }
        assert!(is_air(level.state(top)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn chorus_flowers_grow_a_plant_until_they_die() {
        let mut level = TestLevel::new();
        let base = BlockPos::new(0, 64, 0);
        level.put(base, Block::EndStone.default_state());
        level.put(above(base), Block::ChorusFlower.default_state());
        for _ in 0..200 {
            let flowers: Vec<BlockPos> = level
                .blocks
                .iter()
                .filter(|(_, s)| s.block() == Block::ChorusFlower && s.age() != Some(5))
                .map(|(p, _)| *p)
                .collect();
            for pos in flowers {
                level.run(chorus_flower_tick, pos);
            }
        }
        assert!(level.count(Block::ChorusPlant) >= 2);
        let bottom = level.state(above(base));
        assert_eq!(bottom.block(), Block::ChorusPlant);
        assert_eq!(bottom.down(), Some(true), "rests on end stone");
    }

    #[tuff_core::watchdog]
    #[test]
    fn budding_amethyst_grows_buds_into_clusters() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 0, 0);
        level.put(pos, Block::BuddingAmethyst.default_state());
        for _ in 0..2000 {
            level.run(budding_amethyst_tick, pos);
        }
        assert_eq!(level.count(Block::AmethystCluster), 6);
    }
}
