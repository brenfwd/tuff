//! Phantoms: they fly, circling an anchor point in the sky until they take
//! a player as a target, then alternate between circling above it and
//! swooping down to bite. Natural spawning (insomnia-driven, above
//! sleepless players at night) lives in `region/phantoms.rs`.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};

use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::FaceVelocity;
use super::physics::{inflate, intersects};
use super::target::{NearestAttackablePlayer, TargetView};
use super::{MobCore, MobSounds, Species};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{TickContext, take_number};
use tuff_nbt::Compound;

/// Entity data after the shared fields: its size (0..=64), which scales its
/// attack damage; natural spawns always use 0. Phantoms are not ageable,
/// so the baby/age-locked slots the shared core would otherwise claim
/// (16/17) are free, and the dumped layout puts this at the first of them.
const SIZE: u8 = super::data::BABY;

const SWOOP_TOUCH_MARGIN: f64 = 0.2;
const CIRCLE_REACHED_SQ: f64 = 4.0;
const CIRCLE_SPEED: f64 = 0.6;
const SWEEP_SPEED: f64 = 0.8;
/// Ticks the strategy goal waits before its first swoop once it has a
/// target.
const FIRST_SWEEP_DELAY: i32 = 10;

#[derive(Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Circle,
    Swoop,
}

const CAT_FEAR_RANGE: f64 = 16.0;
const CAT_SEARCH_INTERVAL: u64 = 20;

/// Circles an anchor point (its spawn point, or above its target once it
/// has one) and swoops down to bite when it decides to, all in one goal
/// since this framework's goals only see the shared `MobCore`, not a
/// species' own state; vanilla splits this into three goals that share
/// state on the entity itself.
#[derive(Debug)]
struct PhantomAttack {
    phase: Phase,
    next_sweep: i32,
    anchor: Option<[f64; 3]>,
    move_target: [f64; 3],
    angle: f32,
    distance: f32,
    height: f32,
    clockwise: f32,
    cat_search_tick: u64,
    scared_of_cat: bool,
}

impl Default for PhantomAttack {
    fn default() -> Self {
        Self {
            phase: Phase::Circle,
            next_sweep: FIRST_SWEEP_DELAY,
            anchor: None,
            move_target: [0.0; 3],
            angle: 0.0,
            distance: 5.0,
            height: 0.0,
            clockwise: 1.0,
            cat_search_tick: 0,
            scared_of_cat: false,
        }
    }
}

impl PhantomAttack {
    fn select_next(&mut self) {
        let anchor = self.anchor.unwrap_or([0.0; 3]);
        self.angle += self.clockwise * 15.0_f32.to_radians();
        self.move_target = [
            anchor[0] + f64::from(self.distance * self.angle.cos()),
            anchor[1] + f64::from(self.height - 4.0),
            anchor[2] + f64::from(self.distance * self.angle.sin()),
        ];
    }

    fn tick_circle(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.gen_range(0..350) == 0 {
            self.height = cx.rng.r#gen::<f32>() * 9.0 - 4.0;
        }
        if cx.rng.gen_range(0..250) == 0 {
            self.distance += 1.0;
            if self.distance > 15.0 {
                self.distance = 5.0;
                self.clockwise = -self.clockwise;
            }
        }
        if cx.rng.gen_range(0..450) == 0 {
            self.angle = cx.rng.r#gen::<f32>() * std::f32::consts::TAU;
            self.select_next();
        }
        if mob.distance_sq(self.move_target) < CIRCLE_REACHED_SQ {
            self.select_next();
        }
        mob.movement.set_wanted(self.move_target, CIRCLE_SPEED);
    }

    fn start_swoop(&mut self, target: &TargetView, sea_level: i32, cx: &mut TickContext) {
        self.phase = Phase::Swoop;
        let up = 20.0 + f64::from(cx.rng.gen_range(0..20));
        let mut anchor = [
            target.position[0],
            target.position[1] + up,
            target.position[2],
        ];
        if anchor[1] < f64::from(sea_level) {
            anchor[1] = f64::from(sea_level) + 1.0;
        }
        self.anchor = Some(anchor);
        self.next_sweep = (8 + cx.rng.gen_range(0..4)) * 20;
    }
}

fn cat_nearby(mob: &MobCore, cx: &TickContext) -> bool {
    let area = inflate(mob.bounds(), [CAT_FEAR_RANGE; 3]);
    cx.entities.iter().any(|e| {
        e.kind == EntityType::Cat && e.living.is_some_and(|l| l.alive) && intersects(area, e.bounds)
    })
}

impl Goal for PhantomAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        true
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if self.anchor.is_none() {
            let [x, y, z] = mob.base.position;
            self.anchor = Some([x, y + 5.0, z]);
            self.select_next();
        }
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let target = mob.target_view(cx).filter(|t| t.alive);
        let Some(target) = target else {
            self.phase = Phase::Circle;
            self.tick_circle(mob, cx);
            return;
        };
        match self.phase {
            Phase::Circle => {
                self.next_sweep -= 1;
                if self.next_sweep <= 0 {
                    let sea_level = cx.shared.dimension(cx.dimension).sea_level();
                    self.start_swoop(&target, sea_level, cx);
                    if !mob.base.silent {
                        mob.sound(SoundEvent::EntityPhantomSwoop, 10.0, 1.0, cx);
                    }
                } else {
                    self.tick_circle(mob, cx);
                }
            }
            Phase::Swoop => {
                if mob.tick_count > self.cat_search_tick {
                    self.cat_search_tick = mob.tick_count + CAT_SEARCH_INTERVAL;
                    self.scared_of_cat = cat_nearby(mob, cx);
                }
                if self.scared_of_cat {
                    mob.target = None;
                    self.phase = Phase::Circle;
                    self.tick_circle(mob, cx);
                    return;
                }
                self.move_target = [
                    target.position[0],
                    target.position[1] + 0.5,
                    target.position[2],
                ];
                mob.movement.set_wanted(self.move_target, SWEEP_SPEED);
                if intersects(
                    inflate(mob.bounds(), [SWOOP_TOUCH_MARGIN; 3]),
                    target.bounds,
                ) {
                    mob.hurt_target(&target, cx);
                    self.phase = Phase::Circle;
                    if !mob.base.silent {
                        mob.sound(SoundEvent::EntityPhantomBite, 1.0, 1.0, cx);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Phantom {
    size: u8,
}

impl Species for Phantom {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 5;
        core.move_style = MoveStyle::Float { duration: 0 };
        let mut goals = GoalSelector::default();
        goals.add(1, NearestAttackablePlayer::new(true));
        goals.add(1, PhantomAttack::default());
        goals.add(1, FaceVelocity);
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if let Some(size) = take_number(nbt, "size") {
            self.size = (size as i32).clamp(0, 64) as u8;
            if self.size > 0 {
                core.set_attribute_base(Attribute::AttackDamage, 6.0 + f64::from(self.size));
            }
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("size", i32::from(self.size));
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(SIZE, MetadataValue::VarInt(i32::from(self.size)))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityPhantomAmbient),
            hurt: Some(SoundEvent::EntityPhantomHurt),
            death: Some(SoundEvent::EntityPhantomDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        // Vanilla always resets a freshly spawned phantom's size to 0
        // (bigger phantoms only ever come from hand-edited or plugin-set
        // saved data), so a natural spawn's attack damage stays the
        // dumped base value.
        self.size = 0;
        let _ = core;
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Phantom, super::load::<Phantom>);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn phantom_data_indices_match_the_dumped_layout() {
        let varint = MetadataValue::VarInt(0).type_id() as u8;
        let layout = EntityType::Phantom.data_layout();
        assert_eq!(layout.len(), SIZE as usize + 1);
        assert_eq!(layout[SIZE as usize], varint);
    }

    fn cat_at(x: f64) -> tuff_core::world::entity::EntitySummary {
        use tuff_core::world::entity::{EntitySummary, LivingSummary};
        EntitySummary {
            id: 9,
            uuid: 9,
            kind: EntityType::Cat,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.3, 64.0, 0.2, x + 0.3, 64.7, 0.8],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 0.5,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 10.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cat_within_sixteen_blocks_scares_a_phantom_off_its_swoop() {
        crate::tests::with_entities(&[cat_at(10.5)], |core, cx| {
            core.base.position = [0.5, 64.0, 0.5];
            assert!(cat_nearby(core, cx));
        });
        crate::tests::with_entities(&[cat_at(30.5)], |core, cx| {
            core.base.position = [0.5, 64.0, 0.5];
            assert!(!cat_nearby(core, cx));
        });
    }
}
