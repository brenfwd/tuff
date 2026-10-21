//! Rabbit movement and appetite: it moves in hops, the killer bunny hops
//! at its target, and rabbits strip ripe carrots from farmland.

use super::core::MobCore;
use super::goal::{Flags, Goal};
use super::goals::MoveToBlock;
use rand::Rng;
use tuff_core::world::entity::{Effect, TickContext};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::{Attribute, SoundEvent};
use tuff_world_model::BlockPos;

const JUMP_DURATION: i32 = 15;
const ADULT_KICK_HEIGHT: f64 = 1.5;
const BABY_KICK_HEIGHT: f64 = 0.5;
const KICK: f64 = 0.1;
const EVIL_HOP_RANGE_SQ: f64 = 16.0;
const FASTEST_SPEED: f64 = 2.2;
const WATER_SPEED: f64 = 1.5;
const ENTITY_EVENT_JUMP: i8 = 1;
const CARROT_MAX_AGE: i32 = 7;
const MORE_CARROT_TICKS: i32 = 40;
const RAID_RANGE: i32 = 16;
const RAID_SPEED: f64 = 0.7;

/// What a rabbit remembers between hops.
#[derive(Debug, Default)]
pub struct Hop {
    jumping: bool,
    jump_delay: i32,
    jump_ticks: i32,
    jump_duration: i32,
    can_jump: bool,
    was_on_ground: bool,
    next_jump_speed: f64,
    started: bool,
}

impl Hop {
    /// One server AI step, ahead of the move control: counts down, lands,
    /// and decides whether to hop, then makes the move control walk only
    /// while it is airborne.
    pub fn step(&mut self, core: &mut MobCore, evil: bool, cx: &mut TickContext) {
        if self.jump_ticks != self.jump_duration {
            self.jump_ticks += 1;
        } else if self.jump_duration != 0 {
            self.jump_ticks = 0;
            self.jump_duration = 0;
            self.jumping = false;
        }
        if self.jump_delay > 0 {
            self.jump_delay -= 1;
        }
        if core.more_carrot_ticks > 0 {
            core.more_carrot_ticks = (core.more_carrot_ticks - cx.rng.gen_range(0..3)).max(0);
        }
        let speed = core.movement.speed_modifier;
        if speed > 0.0 {
            self.next_jump_speed = if core.body.in_water {
                WATER_SPEED
            } else {
                speed
            };
        }
        self.started = false;
        let on_ground = core.base.on_ground;
        let want_jump = core.jump.jump;
        if on_ground {
            if !self.was_on_ground {
                self.jumping = false;
                self.jump_delay = if core.movement.speed_modifier < FASTEST_SPEED {
                    10
                } else {
                    3
                };
                self.can_jump = false;
            }
            let target = core
                .target_view(cx)
                .filter(|t| core.distance_sq(t.position) < EVIL_HOP_RANGE_SQ);
            if let Some(target) = target.filter(|_| evil && self.jump_delay == 0) {
                face(core, target.position);
                let speed = core.movement.speed_modifier;
                core.movement.set_wanted(target.position, speed);
                self.start_jumping(core, cx);
            } else if !want_jump {
                if core.movement.has_wanted() && self.jump_delay == 0 {
                    let mut at = core.movement.wanted;
                    if let Some(path) = core.navigation.path.as_ref().filter(|p| !p.is_done()) {
                        let node = path.next_node();
                        at = [node.x as f64 + 0.5, node.y as f64, node.z as f64 + 0.5];
                    }
                    face(core, at);
                    self.start_jumping(core, cx);
                }
            } else if !self.can_jump {
                self.can_jump = true;
            }
        }
        self.was_on_ground = on_ground;
        if on_ground && !self.jumping && !want_jump {
            core.movement.speed_modifier = 0.0;
            core.navigation.speed_modifier = 0.0;
        } else if core.movement.has_wanted()
            || core.movement.operation == super::control::Operation::Jumping
        {
            core.movement.speed_modifier = self.next_jump_speed;
            core.navigation.speed_modifier = self.next_jump_speed;
        }
        if want_jump {
            self.start_jumping(core, cx);
        }
        core.jump.jump = self.jumping;
        self.set_jump_power(core);
        if std::mem::take(&mut self.started) && on_ground {
            self.kick(core, cx);
        }
    }

    fn start_jumping(&mut self, core: &MobCore, cx: &mut TickContext) {
        if !self.jumping {
            let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0;
            core.sound(SoundEvent::EntityRabbitJump, 1.0, pitch * 0.8, cx);
        }
        self.jumping = true;
        self.jump_duration = JUMP_DURATION;
        self.jump_ticks = 0;
        self.started = true;
    }

    fn kick(&self, core: &mut MobCore, cx: &mut TickContext) {
        if core.movement.speed_modifier > 0.0 {
            let [vx, _, vz] = core.base.velocity;
            if vx * vx + vz * vz < 0.01 {
                let height = if core.is_baby() {
                    BABY_KICK_HEIGHT
                } else {
                    ADULT_KICK_HEIGHT
                };
                let yaw = f64::from(core.base.yaw).to_radians();
                core.base.velocity[0] -= yaw.sin() * KICK;
                core.base.velocity[1] += KICK * height;
                core.base.velocity[2] += yaw.cos() * KICK;
            }
        }
        core.entity_event(ENTITY_EVENT_JUMP, cx);
    }

    fn set_jump_power(&self, core: &mut MobCore) {
        let mut power = if core.movement.speed_modifier <= 0.6 {
            0.2
        } else {
            0.3
        };
        if let Some(path) = core.navigation.path.as_ref().filter(|p| !p.is_done())
            && path.next_node().y as f64 > core.base.position[1] + 0.5
        {
            power = 0.5;
        }
        if core.body.horizontal_collision
            || (self.jumping && core.movement.wanted[1] > core.base.position[1] + 0.5)
        {
            power = 0.5;
        }
        core.set_attribute_base(Attribute::JumpStrength, power);
    }
}

fn face(core: &mut MobCore, at: [f64; 3]) {
    let [x, _, z] = core.base.position;
    core.base.yaw = ((at[2] - z).atan2(at[0] - x).to_degrees() - 90.0) as f32;
}

fn ripe_carrots_above(pos: BlockPos, cx: &TickContext) -> bool {
    (cx.block)(BlockPos::new(pos.x, pos.y + 1, pos.z)).is_some_and(|s| {
        s.block() == Block::Carrots
            && s.get("age").and_then(|a| a.parse::<i32>().ok()) == Some(CARROT_MAX_AGE)
    })
}

/// Walks to ripe carrots, which it eats a stage at a time, then stays
/// full for a while.
#[derive(Debug)]
pub struct RaidGarden {
    seek: MoveToBlock,
    wants_to_raid: bool,
    can_raid: bool,
}

impl RaidGarden {
    pub fn new() -> Self {
        let mut seek = MoveToBlock::new(RAID_SPEED, RAID_RANGE, 1, |_, pos, cx| {
            (cx.block)(pos).is_some_and(|s| s.block() == Block::Farmland)
                && ripe_carrots_above(pos, cx)
        });
        seek.recalculate_every = 20;
        Self {
            seek,
            wants_to_raid: false,
            can_raid: false,
        }
    }
}

impl Default for RaidGarden {
    fn default() -> Self {
        Self::new()
    }
}

impl Goal for RaidGarden {
    fn flags(&self) -> Flags {
        self.seek.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !cx.shared.game_rules.borrow().bool("mob_griefing") {
            return false;
        }
        self.wants_to_raid = mob.more_carrot_ticks <= 0;
        self.can_raid = false;
        self.wants_to_raid && self.seek.can_use(mob, cx) && {
            self.can_raid = true;
            true
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_raid && self.seek.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.start(mob, cx);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.tick(mob, cx);
        let crops = BlockPos::new(self.seek.pos.x, self.seek.pos.y + 1, self.seek.pos.z);
        mob.look.set_look_at(
            [
                f64::from(crops.x) + 0.5,
                f64::from(crops.y),
                f64::from(crops.z) + 0.5,
            ],
            10.0,
            40.0,
        );
        if !self.seek.reached() {
            return;
        }
        if self.can_raid
            && let Some(state) = (cx.block)(crops).filter(|s| s.block() == Block::Carrots)
        {
            let age = state
                .get("age")
                .and_then(|a| a.parse::<i32>().ok())
                .unwrap_or(0);
            if age == 0 {
                cx.effects.push(Effect::DestroyBlock {
                    pos: crops,
                    drop: true,
                });
            } else if let Some(state) = state.with("age", &(age - 1).to_string()) {
                super::monster::level_event(crops, 2001, state.id() as i32, cx);
                cx.effects.push(Effect::SetBlock { pos: crops, state });
            }
            mob.more_carrot_ticks = MORE_CARROT_TICKS;
        }
        self.can_raid = false;
        self.seek.try_ticks = 0;
    }
}
