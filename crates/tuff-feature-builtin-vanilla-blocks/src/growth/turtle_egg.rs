//! Turtle eggs: they crack and hatch on sand (almost only at night), and
//! break under anything heavy stepping or landing on them.

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, below};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const BREAK_PARTICLES: i32 = 2001;
const PLACED_PARTICLES: i32 = 2012;
const MAX_HATCH: u8 = 2;
const BABY_AGE: i32 = -24000;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("turtle_egg", hatch);
    behaviours.register_placed(Block::TurtleEgg, placed);
}

pub fn on_sand(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::block::key::SAND,
        level.state(below(pos)).block(),
    )
}

fn pitch(level: &mut dyn GrowthLevel) -> f32 {
    0.9 + level.rng().r#gen::<f32>() * 0.2
}

/// A random tick: the egg cracks a little more, or hatches a baby turtle
/// per egg that calls this spot home.
fn hatch(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let chance = level
        .environment()
        .turtle_egg_hatch_chance(|clock| level.clock_ticks(clock));
    if !(chance > 0.0 && level.rng().r#gen::<f32>() < chance) || !on_sand(level, pos) {
        return;
    }
    let cracked = state.hatch().unwrap_or(0);
    if cracked < MAX_HATCH {
        let pitch = pitch(level);
        level.block_sound(SoundEvent::EntityTurtleEggCrack, pos, 0.7, pitch);
        if let Some(next) = state.with_hatch(cracked + 1) {
            level.set(pos, next, UPDATE_CLIENTS);
        }
        return;
    }
    let pitch = pitch(level);
    level.block_sound(SoundEvent::EntityTurtleEggHatch, pos, 0.7, pitch);
    level.set(pos, Block::Air.default_state(), UPDATE_ALL);
    for i in 0..state.eggs().unwrap_or(1) {
        level.level_event(BREAK_PARTICLES, pos, state.id() as i32);
        let position = [
            f64::from(pos.x) + 0.3 + f64::from(i) * 0.2,
            f64::from(pos.y),
            f64::from(pos.z) + 0.3,
        ];
        level.spawn_saved_entity(
            Compound::new()
                .with("id", EntityType::Turtle.name())
                .with("Pos", Tag::List(position.map(Tag::Double).to_vec()))
                .with(
                    "Rotation",
                    Tag::List(vec![Tag::Float(0.0), Tag::Float(0.0)]),
                )
                .with("Age", BABY_AGE)
                .with("home_pos", Tag::IntArray(vec![pos.x, pos.y, pos.z])),
        );
    }
}

fn placed(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if on_sand(level, pos) {
        level.level_event(PLACED_PARTICLES, pos, 15);
    }
}

/// Whether an entity of `kind` can break eggs at all: never turtles or
/// bats, always players, other living things only with mob griefing.
pub fn breaks_eggs(level: &dyn GrowthLevel, kind: EntityType, living: bool) -> bool {
    match kind {
        EntityType::Turtle | EntityType::Bat => false,
        EntityType::Player => true,
        _ => living && level.rule_bool("mob_griefing"),
    }
}

/// One egg in the clutch at `pos` breaks, one time in `odds`.
pub fn crush(level: &mut dyn GrowthLevel, pos: BlockPos, odds: u32) {
    let Some(state) = level.block(pos).filter(|s| s.block() == Block::TurtleEgg) else {
        return;
    };
    if level.rng().gen_range(0..odds) != 0 {
        return;
    }
    let pitch = pitch(level);
    level.block_sound(SoundEvent::EntityTurtleEggBreak, pos, 0.7, pitch);
    let eggs = state.eggs().unwrap_or(1);
    if eggs <= 1 {
        level.destroy(pos, false);
        return;
    }
    if let Some(fewer) = state.with_eggs(eggs - 1) {
        level.set(pos, fewer, UPDATE_CLIENTS);
        level.level_event(BREAK_PARTICLES, pos, state.id() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn clutch(level: &mut TestLevel, eggs: u8) -> BlockPos {
        let pos = BlockPos::new(0, 64, 0);
        level.put(below(pos), Block::Sand.default_state());
        let state = Block::TurtleEgg
            .default_state()
            .with_eggs(eggs)
            .expect("eggs");
        level.put(pos, state);
        pos
    }

    #[tuff_core::watchdog]
    #[test]
    fn eggs_crack_and_hatch_into_baby_turtles_at_night() {
        let mut level = TestLevel::new();
        level.clock = 21500;
        let pos = clutch(&mut level, 3);
        level.run(hatch, pos);
        level.run(hatch, pos);
        assert_eq!(level.blocks[&pos].hatch(), Some(2));
        level.run(hatch, pos);
        assert!(!level.blocks.contains_key(&pos), "the clutch hatched");
        assert_eq!(level.spawned.len(), 3);
        let baby = &level.spawned[0].0;
        assert_eq!(baby.get_str("id"), Some("minecraft:turtle"));
        assert_eq!(baby.get_int("Age"), Some(BABY_AGE));
        assert_eq!(baby.get_int_array("home_pos"), Some(&[0, 64, 0][..]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn eggs_rarely_hatch_by_day_and_never_off_sand() {
        let mut level = TestLevel::new();
        let pos = clutch(&mut level, 1);
        for _ in 0..100 {
            level.run(hatch, pos);
        }
        assert!(level.blocks[&pos].hatch().unwrap() < MAX_HATCH);
        level.clock = 21500;
        level.put(below(pos), Block::Stone.default_state());
        for _ in 0..100 {
            level.run(hatch, pos);
        }
        assert!(level.spawned.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn crushing_takes_one_egg_at_a_time() {
        let mut level = TestLevel::new();
        let pos = clutch(&mut level, 2);
        crush(&mut level, pos, 1);
        assert_eq!(level.blocks[&pos].eggs(), Some(1));
        crush(&mut level, pos, 1);
        assert!(!level.blocks.contains_key(&pos));
    }
}
