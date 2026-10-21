//! Sniffer eggs: they crack twice and then hatch a baby sniffer, each step
//! after a long delay that warm ground (moss) halves.

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, below};
use tuff_core::world::redstone::UPDATE_CLIENTS;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const MAX_HATCH: u8 = 2;
const REGULAR_HATCH_TICKS: u32 = 24000;
const BOOSTED_HATCH_TICKS: u32 = 12000;
const RANDOM_HATCH_OFFSET: u32 = 300;
const EGG_CRACK_PARTICLES: i32 = 3009;
const BABY_AGE: i32 = -24000;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_placed(Block::SnifferEgg, placed);
    behaviours.register_scheduled(Block::SnifferEgg, tick);
}

fn boosted(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::block::key::SNIFFER_EGG_HATCH_BOOST,
        level.state(below(pos)).block(),
    )
}

fn placed(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    let boosted = boosted(level, pos);
    if boosted {
        level.level_event(EGG_CRACK_PARTICLES, pos, 0);
    }
    let hatch_time = if boosted {
        BOOSTED_HATCH_TICKS
    } else {
        REGULAR_HATCH_TICKS
    };
    let delay = hatch_time / 3 + level.rng().gen_range(0..RANDOM_HATCH_OFFSET);
    level.schedule_tick(pos, Block::SnifferEgg, delay);
}

fn pitch(level: &mut dyn GrowthLevel) -> f32 {
    0.9 + level.rng().r#gen::<f32>() * 0.2
}

fn tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let cracked = state.hatch().unwrap_or(0);
    let pitch = pitch(level);
    if cracked < MAX_HATCH {
        level.block_sound(SoundEvent::BlockSnifferEggCrack, pos, 0.7, pitch);
        if let Some(next) = state.with_hatch(cracked + 1) {
            level.set(pos, next, UPDATE_CLIENTS);
        }
        return;
    }
    level.block_sound(SoundEvent::BlockSnifferEggHatch, pos, 0.7, pitch);
    level.destroy(pos, false);
    let yaw = level.rng().r#gen::<f32>() * 360.0 - 180.0;
    let position = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    ];
    level.spawn_saved_entity(
        Compound::new()
            .with("id", EntityType::Sniffer.name())
            .with("Pos", Tag::List(position.map(Tag::Double).to_vec()))
            .with(
                "Rotation",
                Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
            )
            .with("Age", BABY_AGE),
    );
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn placing_schedules_a_third_of_the_hatch_time_plus_jitter() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::SnifferEgg.default_state());
        level.run(placed, pos);
        let delay = level.ticks[0].2;
        assert!((8000..8300).contains(&delay));
        assert!(level.events.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn moss_underneath_halves_the_delay_and_shows_particles() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(below(pos), Block::MossBlock.default_state());
        level.put(pos, Block::SnifferEgg.default_state());
        level.run(placed, pos);
        assert!((4000..4300).contains(&level.ticks[0].2));
        assert_eq!(level.events[0].0, EGG_CRACK_PARTICLES);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_egg_cracks_twice_then_hatches_a_baby_sniffer() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::SnifferEgg.default_state());
        level.run(tick, pos);
        assert_eq!(level.state(pos).hatch(), Some(1));
        level.run(tick, pos);
        assert_eq!(level.state(pos).hatch(), Some(2));
        assert!(level.spawned.is_empty());
        level.run(tick, pos);
        assert!(!level.blocks.contains_key(&pos));
        assert_eq!(level.spawned.len(), 1);
        assert_eq!(level.spawned[0].0.get_str("id"), Some("minecraft:sniffer"));
        assert_eq!(level.spawned[0].0.get_int("Age"), Some(BABY_AGE));
    }
}
