//! Frogspawn: hatches into two to five tadpoles a few minutes after it
//! was laid, and rots away if the water under it goes.

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, can_survive};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const MIN_HATCH_DELAY: u32 = 3600;
const MAX_HATCH_DELAY: u32 = 12000;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_placed(Block::Frogspawn, placed);
    behaviours.register_scheduled(Block::Frogspawn, hatch);
}

fn placed(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    let delay = level.rng().gen_range(MIN_HATCH_DELAY..MAX_HATCH_DELAY);
    level.schedule_tick(pos, Block::Frogspawn, delay);
}

fn tadpole_offset(level: &mut dyn GrowthLevel) -> f64 {
    level.rng().r#gen::<f64>().clamp(0.2, 0.8)
}

fn hatch(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let survives = can_survive(level, state, pos);
    level.destroy(pos, false);
    if !survives {
        return;
    }
    level.play_block_sound(SoundEvent::BlockFrogspawnHatch, pos, 1.0, 1.0);
    for _ in 0..level.rng().gen_range(2..=5) {
        let x = f64::from(pos.x) + tadpole_offset(level);
        let z = f64::from(pos.z) + tadpole_offset(level);
        let yaw = level.rng().gen_range(1..361) as f32;
        let position = [x, f64::from(pos.y) - 0.5, z];
        level.spawn_saved_entity(
            Compound::new()
                .with("id", EntityType::Tadpole.name())
                .with("Pos", Tag::List(position.map(Tag::Double).to_vec()))
                .with(
                    "Rotation",
                    Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
                )
                .with("PersistenceRequired", true),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn spawn_over_water(level: &mut TestLevel) -> BlockPos {
        let pos = BlockPos::new(0, 64, 0);
        level.put(BlockPos::new(0, 63, 0), Block::Water.default_state());
        level.put(pos, Block::Frogspawn.default_state());
        pos
    }

    #[tuff_core::watchdog]
    #[test]
    fn placed_frogspawn_schedules_a_hatch_within_the_delay_range() {
        let mut level = TestLevel::new();
        let pos = spawn_over_water(&mut level);
        level.run(placed, pos);
        let delay = level.ticks[0].2;
        assert!((MIN_HATCH_DELAY..MAX_HATCH_DELAY).contains(&delay));
    }

    #[tuff_core::watchdog]
    #[test]
    fn hatching_frogspawn_releases_two_to_five_tadpoles() {
        let mut level = TestLevel::new();
        let pos = spawn_over_water(&mut level);
        level.run(hatch, pos);
        assert!(!level.blocks.contains_key(&pos));
        assert!((2..=5).contains(&level.spawned.len()));
        assert_eq!(level.spawned[0].0.get_str("id"), Some("minecraft:tadpole"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn unsupported_frogspawn_breaks_without_hatching() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        level.put(pos, Block::Frogspawn.default_state());
        level.run(hatch, pos);
        assert!(level.spawned.is_empty());
    }
}
