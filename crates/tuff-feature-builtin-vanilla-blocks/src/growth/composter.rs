//! Composters ripen: a full one (level 7) turns ready a second after it
//! filled.

use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::redstone::UPDATE_ALL;
use tuff_world_model::BlockPos;

use super::{Behaviours, GrowthLevel};

const RIPEN_DELAY: u32 = 20;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_placed(Block::Composter, placed);
    behaviours.register_scheduled(Block::Composter, ripen);
}

fn is_full(state: BlockState) -> bool {
    state.get("level") == Some("7")
}

fn placed(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if is_full(state) {
        level.schedule_tick(pos, Block::Composter, RIPEN_DELAY);
    }
}

fn ripen(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if !is_full(state) {
        return;
    }
    let Some(ready) = state.with("level", "8") else {
        return;
    };
    level.set(pos, ready, UPDATE_ALL);
    level.play_block_sound(SoundEvent::BlockComposterReady, pos, 1.0, 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::growth::testing::TestLevel;

    #[tuff_core::watchdog]
    #[test]
    fn a_full_composter_schedules_its_ripening_and_turns_ready() {
        let behaviours = Behaviours::builtin();
        let pos = BlockPos::new(0, 64, 0);
        let full = Block::Composter.default_state().with("level", "7").unwrap();
        assert!(full.has_analog_output());
        assert_eq!(full.state_analog_output(), 7);
        let mut level = TestLevel::new();
        level.put(pos, full);
        behaviours.placed(&mut level, pos, full);
        assert_eq!(level.ticks, vec![(pos, Block::Composter, RIPEN_DELAY)]);
        behaviours.scheduled_tick(&mut level, pos, full);
        assert_eq!(level.state_at(pos).get("level"), Some("8"));
        assert_eq!(level.sounds, vec![(SoundEvent::BlockComposterReady, pos)]);

        let half = Block::Composter.default_state().with("level", "3").unwrap();
        let mut level = TestLevel::new();
        level.put(pos, half);
        behaviours.placed(&mut level, pos, half);
        behaviours.scheduled_tick(&mut level, pos, half);
        assert!(level.ticks.is_empty());
        assert_eq!(level.state_at(pos), half);
    }
}
