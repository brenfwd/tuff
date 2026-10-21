//! Daylight detectors: sky light scaled by the sun's height, or its
//! inverse, sampled every second.

use tuff_vanilla_data::BlockState;

use super::{BlockBehavior, RedstoneLevel, SignalBehavior, UPDATE_ALL, UPDATE_CLIENTS};
use tuff_world_model::BlockPos;

pub struct DaylightDetector;

/// The power the detector at `pos` should give, or `None` without a sky.
fn target(level: &dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> Option<u8> {
    let mut target = i32::from(level.sky_brightness(pos)?);
    if state.inverted() == Some(true) {
        target = 15 - target;
    } else if target > 0 {
        let angle = level.sun_angle().to_radians();
        let offset = if angle < std::f32::consts::PI {
            0.0
        } else {
            std::f32::consts::TAU
        };
        target = (target as f32 * (angle + (offset - angle) * 0.2).cos()).round() as i32;
    }
    Some(target.clamp(0, 15) as u8)
}

fn update(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    if let Some(power) = target(level, pos, state)
        && state.power() != Some(power)
    {
        level.set_block(pos, state.with_power(power).unwrap_or(state), UPDATE_ALL);
    }
}

impl BlockBehavior for DaylightDetector {
    fn has_slow_tick(&self) -> bool {
        true
    }

    fn slow_tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        update(level, pos, state);
    }

    fn used(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
        let inverted = state.inverted() != Some(true);
        let Some(next) = state.with("inverted", if inverted { "true" } else { "false" }) else {
            return false;
        };
        level.set_block(pos, next, UPDATE_CLIENTS);
        update(level, pos, next);
        true
    }
}

impl SignalBehavior for DaylightDetector {}
