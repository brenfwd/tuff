//! Raids as a villager lives them: before the first wave (and between
//! waves) it runs to the bell and rings it, while a wave is on it hides in
//! a house, and after a win it heads out under the open sky.

use rand::Rng;
use tuff_core::world::entity::Culprit;
use tuff_core::world::projectile::{self, Flight, Launch, Owner, Shot};
use tuff_item::item::ItemStack;
use tuff_item::item::decor::{FireworkExplosion, Fireworks};
use tuff_vanilla_data::item::{DyeColor, FireworkShape};
use tuff_vanilla_data::registry::Activity;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, Item};

use super::{Villager, center, distance_sq};
use crate::MobCore;
use crate::physics::block_pos;
use crate::raid::{Phase, Raid, Raids};
use crate::random_pos;
use tuff_core::world::entity::{Effect, TickContext};
use tuff_world_model::BlockPos;

fn has_no_blocks_above(core: &MobCore, target: BlockPos, cx: &TickContext) -> bool {
    core.can_see_sky(target, cx)
        && f64::from(crate::spawning::heightmap_top(
            tuff_vanilla_data::entity::Heightmap::MotionBlocking,
            target.x,
            target.z,
            cx.max_y,
            cx.min_y,
            cx.block,
        )) <= core.base.position[1]
}

fn launch_firework(core: &MobCore, cx: &mut TickContext) {
    let dyes = DyeColor::ALL;
    let color = dyes[cx.rng.gen_range(0..dyes.len())].firework_color();
    let mut rocket = ItemStack::new(Item::FireworkRocket, 1);
    rocket.components.fireworks = Some(Box::new(Fireworks {
        flight_duration: cx.rng.gen_range(0..3),
        explosions: vec![FireworkExplosion {
            shape: FireworkShape::Burst,
            colors: vec![color],
            ..FireworkExplosion::default()
        }],
    }));
    let id = cx.shared.next_entity_id();
    let owner = Owner::of(Culprit::entity(&core.base), None);
    let position = [
        core.base.position[0],
        core.eye_position()[1],
        core.base.position[2],
    ];
    projectile::launch(
        Launch {
            id,
            position,
            owner,
            shot: Shot::Firework {
                item: rocket,
                flight: Flight::Launched,
            },
        },
        cx,
    );
}

const SPEED: f64 = 0.5;
const GATHER_SPEED: f64 = SPEED * 1.5;
const HIDE_SPEED: f64 = SPEED * 1.4;
const CELEBRATE_STROLL_SPEED: f64 = SPEED * 1.1;
const STATUS_ODDS: u32 = 20;
const RING_CHANCE: f32 = 0.95;
const RING_DISTANCE: f64 = 3.0;
const HIDE_RADIUS: i32 = 24;
const HIDE_CLOSE: f64 = 1.0;
const GATHER_CLOSE: i32 = 2;
const SKY_TRIES: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    PreRaid,
    Raid,
}

#[derive(Debug, Default)]
pub struct State {
    status: Option<Status>,
    phase: Option<Phase>,
    hiding: Option<BlockPos>,
}

impl Villager {
    pub(super) fn raid_activity(&self) -> Option<Activity> {
        self.brain.raid.status.map(|status| match status {
            Status::PreRaid => Activity::PreRaid,
            Status::Raid => Activity::Raid,
        })
    }

    pub(super) fn update_raid_status(&mut self, core: &MobCore, cx: &mut TickContext) {
        if cx.rng.gen_range(0..STATUS_ODDS) != 0 {
            return;
        }
        let raid = cx.state.get::<Raids>().at(block_pos(core.base.position));
        let state = &mut self.brain.raid;
        state.phase = raid.map(|r| r.phase);
        state.status = match raid {
            Some(Raid {
                phase: Phase::Stopped | Phase::Loss,
                ..
            })
            | None => None,
            Some(Raid {
                phase: Phase::Preparing | Phase::BetweenWaves,
                ..
            }) => Some(Status::PreRaid),
            Some(_) => Some(Status::Raid),
        };
        if state.status != Some(Status::Raid) {
            state.hiding = None;
        }
    }

    pub(super) fn pre_raid(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(bell) = self.brain.memories.meeting_point.clone() else {
            self.stroll(core, cx, GATHER_SPEED);
            return;
        };
        if cx.rng.r#gen::<f32>() > RING_CHANCE
            && distance_sq(center(bell.pos), core.base.position) < RING_DISTANCE * RING_DISTANCE
            && (cx.block)(bell.pos).is_some_and(|s| s.block() == Block::Bell)
        {
            cx.effects.push(Effect::RingBell { pos: bell.pos });
            self.heard_bell();
        }
        if !core.navigation.is_done() {
            return;
        }
        if cx.rng.gen_range(0..8) < 6 {
            let here = block_pos(core.base.position);
            if super::manhattan(bell.pos, here) > GATHER_CLOSE {
                core.move_to_block(bell.pos, GATHER_CLOSE, GATHER_SPEED, cx);
            }
        } else if let Some(target) = random_pos::land_pos(core, 10, 7, cx) {
            core.move_to_position(target, GATHER_SPEED, cx);
        }
    }

    pub(super) fn raid(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.brain.raid.phase == Some(Phase::Victory) {
            self.celebrate(core, cx);
            return;
        }
        let here = block_pos(core.base.position);
        if let Some(place) = self.brain.raid.hiding
            && distance_sq(center(place), core.base.position) < HIDE_CLOSE * HIDE_CLOSE + 1.0
        {
            core.navigation.stop();
            return;
        }
        if !core.navigation.is_done() {
            return;
        }
        let Some(place) = self.hiding_place(core, cx, HIDE_RADIUS) else {
            return;
        };
        self.end_interaction();
        self.brain.breeding = None;
        self.brain.raid.hiding = Some(place);
        if super::manhattan(place, here) > HIDE_CLOSE as i32 {
            core.move_to_block(place, HIDE_CLOSE as i32, HIDE_SPEED, cx);
        }
    }

    fn celebrate(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let here = block_pos(core.base.position);
        if cx.rng.gen_range(0..100) == 0 {
            core.sound(SoundEvent::EntityVillagerCelebrate, 1.0, 1.0, cx);
        }
        if cx.rng.gen_range(0..200) == 0 && core.can_see_sky(here, cx) {
            launch_firework(core, cx);
        }
        if !core.navigation.is_done() {
            return;
        }
        if core.can_see_sky(here, cx) {
            if cx.rng.gen_range(0..60) == 0
                && let Some(target) = random_pos::land_pos(core, 10, 7, cx)
            {
                core.move_to_position(target, CELEBRATE_STROLL_SPEED, cx);
            }
            return;
        }
        for _ in 0..SKY_TRIES {
            let target = BlockPos::new(
                here.x + cx.rng.gen_range(0..20) - 10,
                here.y + cx.rng.gen_range(0..6) - 3,
                here.z + cx.rng.gen_range(0..20) - 10,
            );
            if has_no_blocks_above(core, target, cx) {
                core.move_to_position(
                    [
                        target.x as f64 + 0.5,
                        target.y as f64,
                        target.z as f64 + 0.5,
                    ],
                    SPEED,
                    cx,
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_celebrating_villager_launches_a_burst_firework() {
        crate::tests::with_context(|core, cx| {
            launch_firework(core, cx);
            assert!(matches!(cx.effects.last(), Some(Effect::Spawn(_))));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sky_spot_above_the_ground_level_of_the_body_is_not_outdoors() {
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 || (pos.x == 5 && pos.y < 70) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        crate::tests::with_world(&block, &[], |core, cx| {
            core.base.position[1] = 64.0;
            assert!(has_no_blocks_above(core, BlockPos::new(0, 64, 0), cx));
            assert!(core.can_see_sky(BlockPos::new(5, 75, 0), cx));
            assert!(!has_no_blocks_above(core, BlockPos::new(5, 75, 0), cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_heard_bell_does_not_hide_a_villager_inside_a_raid() {
        use crate::Species;
        use rand::SeedableRng;
        crate::tests::with_context(|core, cx| {
            let mut villager = Villager::create(&mut rand::rngs::StdRng::seed_from_u64(1));
            villager.brain.memories.heard_bell = Some(cx.tick as i64);
            villager.brain.raid.status = Some(Status::PreRaid);
            villager.update_activity(core, cx);
            assert_eq!(villager.brain.activity, Some(Activity::PreRaid));
        });
    }
}
