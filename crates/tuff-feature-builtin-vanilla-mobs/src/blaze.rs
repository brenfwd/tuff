//! Blazes: they hover towards their target's height, shoot bursts of three
//! small fireballs, burn when charging, hate water, and drop blaze rods.

use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::physics::block_pos;
use super::target::{HurtByTarget, NearestAttackablePlayer};
use super::{MobCore, MobSounds, Species, data, monster};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Culprit, Effect, TickContext};
use tuff_core::world::projectile::{self, Launch, Owner, Shot, triangle};
use tuff_core::world::{framed, packets};

const FLAGS: u8 = data::BABY;
const FLAG_CHARGED: i8 = 1;
const BLAZE_XP: i32 = 10;
const FIREBALL_SOUND_EVENT: i32 = 1018;

/// Up close it hits; within its follow range and in sight it charges for
/// three seconds, then shoots three fireballs and rests for five.
#[derive(Debug, Default)]
struct BlazeAttack {
    step: i32,
    time: i32,
    last_seen: i32,
}

impl Goal for BlazeAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx)
            .is_some_and(|t| t.alive && mob.can_attack(&t, cx))
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.step = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.signal = 0;
        self.last_seen = 0;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.time -= 1;
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let sees = mob.has_line_of_sight(&target, cx);
        if sees {
            self.last_seen = 0;
        } else {
            self.last_seen += 1;
        }
        let distance = mob.distance_sq(target.position);
        let range = mob.follow_range();
        if distance < 4.0 {
            if !sees {
                return;
            }
            if self.time <= 0 {
                self.time = 20;
                mob.hurt_target(&target, cx);
            }
            mob.movement.set_wanted(target.position, 1.0);
        } else if distance < range * range && sees {
            let height = f64::from(mob.size.height);
            let target_height = target.bounds[4] - target.bounds[1];
            let dx = target.position[0] - mob.base.position[0];
            let dy =
                target.position[1] + target_height * 0.5 - (mob.base.position[1] + height * 0.5);
            let dz = target.position[2] - mob.base.position[2];
            if self.time <= 0 {
                self.step += 1;
                if self.step == 1 {
                    self.time = 60;
                    mob.signal = 1;
                } else if self.step <= 4 {
                    self.time = 6;
                } else {
                    self.time = 100;
                    self.step = 0;
                    mob.signal = 0;
                }
                if self.step > 1 {
                    let spread = distance.sqrt().sqrt() * 0.5;
                    if !mob.base.silent {
                        let event = framed(&play::LevelEvent {
                            event: FIREBALL_SOUND_EVENT,
                            position: block_pos(mob.base.position),
                            data: 0,
                            global: false,
                        });
                        cx.effects.push(Effect::ChunkPackets(packets([event])));
                    }
                    let direction = [
                        triangle(cx.rng, dx, 2.297 * spread),
                        dy,
                        triangle(cx.rng, dz, 2.297 * spread),
                    ];
                    let [x, y, z] = mob.base.position;
                    let start = [x, y + height * 0.5 + 0.5, z];
                    let owner = Owner::of(Culprit::entity(&mob.base), None);
                    let id = cx.shared.next_entity_id();
                    projectile::launch(
                        Launch {
                            id,
                            position: start,
                            owner,
                            shot: Shot::SmallFireball { direction },
                        },
                        cx,
                    );
                }
            }
            mob.look.set_look_at(target.eye, 10.0, 10.0);
        } else if self.last_seen < 5 {
            mob.movement.set_wanted(target.position, 1.0);
        }
    }
}

#[derive(Debug)]
pub struct Blaze {
    height_offset: f32,
    next_height_change: i32,
    charged: bool,
}

impl Species for Blaze {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self {
            height_offset: 0.5,
            next_height_change: 0,
            charged: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = BLAZE_XP;
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Water, -1.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Lava, 8.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::FireInNeighbor, 0.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Fire, 0.0);
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]).alerting(&[EntityType::Blaze]));
        goals.add(2, NearestAttackablePlayer::new(true));
        goals.add(4, BlazeAttack::default());
        goals.add(5, super::goals::MoveTowardsHome::new(1.0));
        goals.add(7, RandomStroll::water_avoiding(1.0));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let flags = if self.charged { FLAG_CHARGED } else { 0 };
        vec![(FLAGS, MetadataValue::Byte(flags))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            step: None,
            ..monster::sounds(
                SoundEvent::EntityBlazeAmbient,
                SoundEvent::EntityBlazeHurt,
                SoundEvent::EntityBlazeDeath,
                None,
            )
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let [x, _, z] = core.base.position;
        let eye = block_pos([x, core.eye_position()[1], z]);
        let rained_on = cx.raining && core.can_see_sky(eye, cx);
        if core.is_alive() && (core.body.in_water || rained_on) {
            core.hurt_later(1.0, &damage_type::DROWN);
        }
        if !core.base.on_ground && core.base.velocity[1] < 0.0 {
            core.base.velocity[1] *= 0.6;
        }
        self.next_height_change -= 1;
        if self.next_height_change <= 0 {
            self.next_height_change = 100;
            self.height_offset = triangle(cx.rng, 0.5, 6.891) as f32;
        }
        if let Some(target) = core.target_view(cx)
            && target.eye[1] > core.eye_position()[1] + f64::from(self.height_offset)
            && core.can_attack(&target, cx)
        {
            let v = core.base.velocity[1];
            core.base.velocity[1] += (0.300_000_011_920_929 - v) * 0.300_000_011_920_929;
            core.impulse = true;
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        let charged = core.signal > 0;
        if charged != self.charged {
            self.charged = charged;
            core.changed(FLAGS);
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Blaze, super::load::<Blaze>);
    registry.register_spawn_rule(EntityType::Blaze, super::spawning::mob_rule);
}
