//! Speleothems growing and dripping: stalactites lengthen (and seed a
//! stalagmite below) under their growth block, water and lava above a
//! stalactite fill cauldrons beneath it, and mud under one turns to clay.

use tuff_vanilla_data::block::{Facing, Thickness, VerticalDirection};
use tuff_vanilla_data::fluid::Fluid;
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, above, below};
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_vanilla_common::placement::step;
use tuff_vanilla_common::speleothem;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const GROWTH_CHANCE: f32 = 0.011377778;
const WATER_TRANSFER_CHANCE: f32 = 0.17578125;
const LAVA_TRANSFER_CHANCE: f32 = 0.05859375;
const SEARCH_LENGTH: i32 = 11;
const DRIP_EVENT: i32 = 1504;
const DRIP_LAVA_INTO_CAULDRON: i32 = 1046;
const DRIP_WATER_INTO_CAULDRON: i32 = 1047;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("pointed_dripstone", random_tick);
    behaviours.register_random_kind("speleothem", random_tick);
    for cauldron in [
        Block::Cauldron,
        Block::WaterCauldron,
        Block::LavaCauldron,
        Block::PowderSnowCauldron,
    ] {
        behaviours.register_scheduled(cauldron, cauldron_tick);
    }
}

fn grows_on(block: Block) -> Block {
    if block == Block::SulfurSpike {
        Block::Sulfur
    } else {
        Block::DripstoneBlock
    }
}

fn max_growth_length(block: Block) -> i32 {
    if block == Block::SulfurSpike { 2 } else { 7 }
}

fn is_tip(state: BlockState, include_merged: bool) -> bool {
    match state.thickness() {
        Some(Thickness::Tip) => true,
        Some(Thickness::TipMerge) => include_merged,
        _ => false,
    }
}

fn is_stalactite_start(level: &dyn GrowthLevel, state: BlockState, pos: BlockPos) -> bool {
    speleothem::is_stalactite(state) && level.state(above(pos)).block() != state.block()
}

fn is_free_hanging_stalactite(state: BlockState) -> bool {
    speleothem::is_stalactite(state)
        && state.thickness() == Some(Thickness::Tip)
        && state.waterlogged() != Some(true)
}

type Path<'a> = &'a dyn Fn(BlockPos, BlockState) -> bool;

fn find_vertical(
    level: &dyn GrowthLevel,
    pos: BlockPos,
    direction: Facing,
    path: Path,
    target: &dyn Fn(BlockState) -> bool,
    max_steps: i32,
) -> Option<BlockPos> {
    let mut at = pos;
    for _ in 1..max_steps {
        at = step(at, direction);
        let state = level.state(at);
        if target(state) {
            return Some(at);
        }
        if !level.is_inside_build_height(at.y) || !path(at, state) {
            return None;
        }
    }
    None
}

fn find_tip(
    level: &dyn GrowthLevel,
    state: BlockState,
    pos: BlockPos,
    max: i32,
    include_merged: bool,
) -> Option<BlockPos> {
    if is_tip(state, include_merged) {
        return Some(pos);
    }
    let direction = speleothem::tip(state)?;
    find_vertical(
        level,
        pos,
        direction,
        &|_, s| s.block() == state.block() && speleothem::tip(s) == Some(direction),
        &|s| is_tip(s, include_merged),
        max,
    )
}

fn is_unmerged_tip(state: BlockState, block: Block, direction: Facing) -> bool {
    state.block() == block && is_tip(state, false) && speleothem::tip(state) == Some(direction)
}

fn can_grow(level: &dyn GrowthLevel, block: Block, pos: BlockPos) -> bool {
    if level.state(above(pos)).block() != grows_on(block) {
        return false;
    }
    if block != Block::PointedDripstone {
        return true;
    }
    let fluid = level.state(above(above(pos))).fluid_state();
    fluid.fluid() == Fluid::Water && fluid.is_source()
}

fn can_tip_grow(level: &dyn GrowthLevel, tip: BlockState, pos: BlockPos) -> bool {
    let Some(direction) = speleothem::tip(tip) else {
        return false;
    };
    let target = level.state(step(pos, direction));
    if !target.fluid_state().is_empty() {
        return false;
    }
    is_air(target) || is_unmerged_tip(target, tip.block(), direction.opposite())
}

fn create(
    level: &mut dyn GrowthLevel,
    block: Block,
    pos: BlockPos,
    direction: Facing,
    thickness: Thickness,
) {
    let water = level.state(pos).fluid_state().fluid() == Fluid::Water;
    let vertical = if direction == Facing::Up {
        VerticalDirection::Up
    } else {
        VerticalDirection::Down
    };
    let state = block
        .default_state()
        .with_vertical_direction(vertical)
        .and_then(|s| s.with_thickness(thickness))
        .and_then(|s| s.with_waterlogged(water));
    if let Some(state) = state {
        level.set(pos, state, UPDATE_ALL);
    }
}

fn grow(level: &mut dyn GrowthLevel, block: Block, from: BlockPos, direction: Facing) {
    let target = step(from, direction);
    let existing = level.state(target);
    if is_unmerged_tip(existing, block, direction.opposite()) {
        let (stalactite, stalagmite) = if speleothem::tip(existing) == Some(Facing::Up) {
            (above(target), target)
        } else {
            (target, below(target))
        };
        create(level, block, stalactite, Facing::Down, Thickness::TipMerge);
        create(level, block, stalagmite, Facing::Up, Thickness::TipMerge);
    } else if is_air(existing) || existing.block() == Block::Water {
        create(level, block, target, direction, Thickness::Tip);
    }
}

fn grow_stalagmite_below(level: &mut dyn GrowthLevel, block: Block, above_stalagmite: BlockPos) {
    let mut pos = above_stalagmite;
    for _ in 0..10 {
        pos = below(pos);
        let state = level.state(pos);
        if !state.fluid_state().is_empty() {
            return;
        }
        let held = {
            let lookup = |p: BlockPos| level.state(p);
            speleothem::valid_placement(block, pos, Facing::Up, &lookup)
        };
        if is_unmerged_tip(state, block, Facing::Up) && can_tip_grow(level, state, pos) {
            grow(level, block, pos, Facing::Up);
            return;
        } else if held && level.state(below(pos)).fluid_state().fluid() != Fluid::Water {
            grow(level, block, below(pos), Facing::Up);
            return;
        } else if block == Block::PointedDripstone && !can_drip_through(state) {
            return;
        }
    }
}

fn random_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let drip_roll = level.random_float();
    drip_from(level, state, pos, drip_roll);
    if level.random_float() < GROWTH_CHANCE && is_stalactite_start(level, state, pos) {
        grow_if_possible(level, state, pos);
    }
}

fn grow_if_possible(level: &mut dyn GrowthLevel, start: BlockState, pos: BlockPos) {
    let block = start.block();
    if !can_grow(level, block, pos) {
        return;
    }
    let Some(tip_pos) = find_tip(level, start, pos, max_growth_length(block), false) else {
        return;
    };
    let tip = level.state(tip_pos);
    if !is_free_hanging_stalactite(tip) || !can_tip_grow(level, tip, tip_pos) {
        return;
    }
    if level.random_bool() {
        grow(level, block, tip_pos, Facing::Down);
    } else {
        grow_stalagmite_below(level, block, tip_pos);
    }
}

/// Whether a drip falls through `state`: air, or something non-solid and
/// fluid-free whose collision leaves the 4 pixel wide centre column clear.
fn can_drip_through(state: BlockState) -> bool {
    if is_air(state) {
        return true;
    }
    if state.is_solid_render() || !state.fluid_state().is_empty() {
        return false;
    }
    let (low, high) = (6.0 / 16.0, 10.0 / 16.0);
    !state
        .collision_boxes()
        .iter()
        .any(|b| b[0] < high && b[3] > low && b[2] < high && b[5] > low && b[4] > b[1])
}

/// What feeds a stalactite: the block resting on its root and the fluid it
/// drips (wet mud counts as water).
struct DripSource {
    pos: BlockPos,
    fluid: Fluid,
    mud: bool,
}

fn drip_source(level: &dyn GrowthLevel, pos: BlockPos, state: BlockState) -> Option<DripSource> {
    if !speleothem::is_stalactite(state) {
        return None;
    }
    let kind = state.block();
    let root = find_vertical(
        level,
        pos,
        Facing::Up,
        &|_, s| s.block() == kind && speleothem::is_stalactite(s),
        &|s| s.block() != kind,
        SEARCH_LENGTH,
    )?;
    let source = above(root);
    let feeder = level.state(source);
    let mud = feeder.block() == Block::Mud;
    let fluid = if mud && !level.water_evaporates() {
        Fluid::Water
    } else {
        feeder.fluid_state().fluid()
    };
    Some(DripSource {
        pos: source,
        fluid,
        mud,
    })
}

const DRIP_CHANCES: [(Fluid, f32); 2] = [
    (Fluid::Water, WATER_TRANSFER_CHANCE),
    (Fluid::Lava, LAVA_TRANSFER_CHANCE),
];

fn drip_chance(fluid: Fluid) -> Option<f32> {
    DRIP_CHANCES
        .iter()
        .find_map(|&(f, chance)| (f == fluid).then_some(chance))
}

fn accepts_drip(block: Block, fluid: Fluid) -> bool {
    match block {
        Block::Cauldron => matches!(fluid, Fluid::Water | Fluid::Lava),
        Block::WaterCauldron => fluid == Fluid::Water,
        _ => false,
    }
}

fn drip_from(level: &mut dyn GrowthLevel, state: BlockState, pos: BlockPos, roll: f32) {
    let any_chance = WATER_TRANSFER_CHANCE.max(LAVA_TRANSFER_CHANCE);
    if roll > any_chance || !is_stalactite_start(level, state, pos) {
        return;
    }
    let Some(source) = drip_source(level, pos, state) else {
        return;
    };
    if drip_chance(source.fluid).is_none_or(|chance| roll >= chance) {
        return;
    }
    let Some(tip) = find_tip(level, state, pos, SEARCH_LENGTH, false) else {
        return;
    };
    let target = if source.mud && source.fluid == Fluid::Water {
        level.set(source.pos, Block::Clay.default_state(), UPDATE_ALL);
        None
    } else {
        let fluid = source.fluid;
        let Some(cauldron) = find_vertical(
            level,
            tip,
            Facing::Down,
            &|_, s| can_drip_through(s),
            &|s| accepts_drip(s.block(), fluid),
            SEARCH_LENGTH,
        ) else {
            return;
        };
        Some(cauldron)
    };
    level.level_event(DRIP_EVENT, tip, 0);
    if let Some(cauldron) = target {
        let fall = (tip.y - cauldron.y) as u32;
        let block = level.state(cauldron).block();
        level.schedule_tick(cauldron, block, 50 + fall);
    }
}

fn cauldron_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let Some(tip) = find_vertical(
        level,
        pos,
        Facing::Up,
        &|_, s| can_drip_through(s),
        &is_free_hanging_stalactite,
        SEARCH_LENGTH,
    ) else {
        return;
    };
    let tip_state = level.state(tip);
    let Some(source) = drip_source(level, tip, tip_state) else {
        return;
    };
    match (state.block(), source.fluid) {
        (Block::Cauldron, Fluid::Water) => {
            level.set(pos, Block::WaterCauldron.default_state(), UPDATE_ALL);
            level.level_event(DRIP_WATER_INTO_CAULDRON, pos, 0);
        }
        (Block::Cauldron, Fluid::Lava) => {
            level.set(pos, Block::LavaCauldron.default_state(), UPDATE_ALL);
            level.level_event(DRIP_LAVA_INTO_CAULDRON, pos, 0);
        }
        (Block::WaterCauldron, Fluid::Water) => {
            let fill = state.level().unwrap_or(1);
            if fill < 3
                && let Some(fuller) = state.with_level(fill + 1)
            {
                level.set(pos, fuller, UPDATE_ALL);
                level.level_event(DRIP_WATER_INTO_CAULDRON, pos, 0);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn cave() -> (TestLevel, BlockPos) {
        let mut level = TestLevel::new();
        let ceiling = BlockPos::new(0, 70, 0);
        level.put(ceiling, Block::DripstoneBlock.default_state());
        level.put(above(ceiling), Block::Water.default_state());
        level.put(BlockPos::new(0, 60, 0), Block::Stone.default_state());
        let tip = below(ceiling);
        let state = Block::PointedDripstone
            .default_state()
            .with_vertical_direction(VerticalDirection::Down)
            .and_then(|s| s.with_thickness(Thickness::Tip))
            .unwrap();
        level.put(tip, state);
        (level, tip)
    }

    #[tuff_core::watchdog]
    #[test]
    fn stalactites_grow_longer_under_water() {
        let (mut level, tip) = cave();
        for _ in 0..20000 {
            level.run(random_tick, tip);
        }
        assert!(level.count(Block::PointedDripstone) > 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn stalactites_do_not_grow_without_water_above_the_growth_block() {
        let (mut level, tip) = cave();
        level.put(BlockPos::new(0, 71, 0), Block::Air.default_state());
        for _ in 0..20000 {
            level.run(random_tick, tip);
        }
        assert_eq!(level.count(Block::PointedDripstone), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dripping_stalactite_fills_the_cauldron_below_it() {
        let (mut level, tip) = cave();
        let cauldron = BlockPos::new(0, 66, 0);
        level.put(cauldron, Block::Cauldron.default_state());
        for _ in 0..2000 {
            level.run(random_tick, tip);
            if !level.ticks.is_empty() {
                break;
            }
        }
        assert_eq!(level.ticks[0].0, cauldron);
        level.run(cauldron_tick, cauldron);
        assert_eq!(level.state(cauldron).block(), Block::WaterCauldron);
        for _ in 0..4 {
            level.run(cauldron_tick, cauldron);
        }
        assert_eq!(level.state(cauldron).level(), Some(3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn mud_under_a_dripping_stalactite_turns_to_clay() {
        let (mut level, tip) = cave();
        level.put(BlockPos::new(0, 71, 0), Block::Mud.default_state());
        for _ in 0..2000 {
            level.run(random_tick, tip);
            if level.count(Block::Clay) > 0 {
                break;
            }
        }
        assert_eq!(level.count(Block::Clay), 1);
    }

    fn transfer_trace(level: &mut TestLevel, tip: BlockPos, roll: f32) -> String {
        let state = level.state(tip);
        drip_from(level, state, tip, roll);
        let trace = format!("{:?}|{:?}", level.ticks, level.events);
        level.ticks.clear();
        level.events.clear();
        trace
    }

    #[tuff_core::watchdog]
    #[test]
    fn fluid_transfer_rolls_pin_events_and_delays() {
        let mut out = Vec::new();
        for (cauldron_y, fluid, mud, evaporates) in [
            (66, Block::Water, false, false),
            (61, Block::Lava, false, false),
            (66, Block::Water, true, false),
            (66, Block::Water, true, true),
            (55, Block::Water, false, false),
        ] {
            for roll in [0.0f32, 0.05, 0.0586, 0.1, 0.17578, 0.1758, 0.5] {
                let (mut level, tip) = cave();
                level.evaporates = evaporates;
                level.put(BlockPos::new(0, 71, 0), fluid.default_state());
                if mud {
                    level.put(BlockPos::new(0, 71, 0), Block::Mud.default_state());
                }
                if cauldron_y == 55 {
                    level.put(BlockPos::new(0, 60, 0), Block::Air.default_state());
                }
                level.put(
                    BlockPos::new(0, cauldron_y, 0),
                    Block::Cauldron.default_state(),
                );
                let trace = transfer_trace(&mut level, tip, roll);
                out.push(format!("{trace}|{}", level.count(Block::Clay)));
            }
        }
        let joined = out.join("\n");
        let hash = joined.bytes().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
        assert_eq!(hash, 18083993614891149595, "{joined}");
    }
}
