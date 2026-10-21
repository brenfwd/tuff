//! Evokers: they keep their distance and cast two spells at a target in
//! sight, summoning vexes and striking with a spike of fangs, rather than
//! ever fighting hand to hand.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::block::{Facing, Support};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{AvoidPlayer, LookAtPlayer, RandomLookAround, RandomStroll};
use super::illager::{self, CastSpell};
use super::raider;
use super::target::{HurtByTarget, NearestAttackablePlayer, TargetView};
use super::vex::Vex;
use super::{Mob, MobCore, MobSounds, Species};
use crate::fangs::EvokerFangs;
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Culprit, Effect, EntityBase, Notice, TickContext};
use tuff_core::world::projectile::Owner;
use tuff_world_model::BlockPos;

const FANGS_CASTING: i32 = 40;
const FANGS_INTERVAL: i32 = 100;
const SUMMON_CASTING: i32 = 100;
const SUMMON_INTERVAL: i32 = 340;
const CLOSE_RANGE_SQ: f64 = 9.0;
const WOLOLO_WARMUP: i32 = 40;
const WOLOLO_CASTING: i32 = 60;
const WOLOLO_INTERVAL: u64 = 140;
const WOLOLO_RANGE: f64 = 16.0;
const WOLOLO_RANGE_Y: f64 = 4.0;

/// With no target to fight, turns a nearby blue sheep red after a short
/// warm-up, when mobs may grief.
#[derive(Debug, Default)]
struct Wololo {
    sheep: Option<i32>,
    warmup: i32,
    casting: i32,
    next_tick: u64,
}

impl Goal for Wololo {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.target.is_some()
            || mob.signal != 0
            || cx.tick < self.next_tick
            || !cx.shared.rule_bool("mob_griefing")
        {
            return false;
        }
        let [x, y, z] = mob.base.position;
        let blue: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| {
                e.blue_sheep
                    && e.living.as_ref().is_some_and(|l| l.alive)
                    && (e.position[0] - x).abs() <= WOLOLO_RANGE
                    && (e.position[2] - z).abs() <= WOLOLO_RANGE
                    && (e.position[1] - y).abs() <= WOLOLO_RANGE_Y
                    && mob.distance_sq(e.position) <= WOLOLO_RANGE * WOLOLO_RANGE
            })
            .map(|e| e.id)
            .collect();
        if blue.is_empty() {
            return false;
        }
        self.sheep = Some(blue[cx.rng.gen_range(0..blue.len())]);
        true
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.sheep.is_some() && (self.warmup > 0 || self.casting > 0)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.warmup = WOLOLO_WARMUP;
        self.casting = WOLOLO_CASTING;
        mob.signal = i32::from(illager::spell::WOLOLO);
        mob.changed(super::data::AGE_LOCKED);
        self.next_tick = cx.tick + WOLOLO_INTERVAL;
        mob.navigation.stop();
        mob.sound(SoundEvent::EntityEvokerPrepareWololo, 1.0, 1.0, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.signal = 0;
        mob.changed(super::data::AGE_LOCKED);
        self.sheep = None;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(id) = self.sheep else { return };
        if self.casting > 0 {
            self.casting -= 1;
        }
        if let Some(sheep) = cx.entities.iter().find(|e| e.id == id) {
            let eye = [
                sheep.position[0],
                sheep.position[1] + 0.5,
                sheep.position[2],
            ];
            mob.look.set_look_at(eye, 30.0, 30.0);
        }
        if self.warmup <= 0 {
            return;
        }
        self.warmup -= 1;
        if self.warmup == 0 {
            cx.effects.push(Effect::Notify {
                target: id,
                notice: Notice::Wololo,
            });
            mob.sound(SoundEvent::EntityEvokerCastSpell, 1.0, 1.0, cx);
        }
    }
}

/// A dumb but effective search: from `max_y` down to `min_y - 1`, the
/// first position with a sturdy top face. `None` if nothing solid is
/// found, in which case the fang is skipped, as the game skips it too.
fn find_floor(x: f64, z: f64, min_y: f64, max_y: f64, cx: &TickContext) -> Option<f64> {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let mut y = max_y.floor() as i32;
    let bottom = min_y.floor() as i32 - 1;
    while y >= bottom {
        let below = BlockPos::new(bx, y - 1, bz);
        let state = (cx.block)(below)?;
        if state.is_face_sturdy(Facing::Up, Support::Full) {
            return Some(f64::from(y));
        }
        y -= 1;
    }
    None
}

fn spawn_fangs(
    mob: &MobCore,
    x: f64,
    z: f64,
    (min_y, max_y): (f64, f64),
    angle: f32,
    delay: i32,
    cx: &mut TickContext,
) {
    let Some(y) = find_floor(x, z, min_y, max_y, cx) else {
        return;
    };
    let id = cx.shared.next_entity_id();
    let owner = Owner::of(Culprit::entity(&mob.base), None);
    let fangs = EvokerFangs::new(id, [x, y, z], angle.to_degrees(), delay, owner);
    cx.effects.push(Effect::Spawn(Box::new(fangs)));
}

/// The fangs spell: a ring of five plus eight around the evoker when its
/// target is within three blocks, otherwise a line of sixteen stepping
/// toward it with an increasing delay.
fn cast_fangs(mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
    let [ex, ey, ez] = mob.base.position;
    let min_y = ey.min(target.position[1]);
    let max_y = ey.max(target.position[1]) + 1.0;
    let towards = (target.position[2] - ez).atan2(target.position[0] - ex) as f32;
    if mob.distance_sq(target.position) < CLOSE_RANGE_SQ {
        for i in 0..5 {
            let angle = towards + i as f32 * std::f32::consts::PI * 0.4;
            let x = ex + f64::from(angle.cos()) * 1.5;
            let z = ez + f64::from(angle.sin()) * 1.5;
            spawn_fangs(mob, x, z, (min_y, max_y), angle, 0, cx);
        }
        for i in 0..8 {
            let angle = towards + (i as f32 * std::f32::consts::PI * 2.0) / 8.0 + 1.256_637_1;
            let x = ex + f64::from(angle.cos()) * 2.5;
            let z = ez + f64::from(angle.sin()) * 2.5;
            spawn_fangs(mob, x, z, (min_y, max_y), angle, 3, cx);
        }
    } else {
        for i in 0..16 {
            let reach = 1.25 * f64::from(i + 1);
            let x = ex + f64::from(towards.cos()) * reach;
            let z = ez + f64::from(towards.sin()) * reach;
            spawn_fangs(mob, x, z, (min_y, max_y), towards, i, cx);
        }
    }
}

const VEX_COUNT_RANGE: f64 = 16.0;

/// Summons only while a roll of 1 to 8 beats the vexes within 16 blocks.
fn few_vexes_nearby(mob: &MobCore, cx: &mut TickContext) -> bool {
    let area = super::physics::inflate(mob.bounds(), [VEX_COUNT_RANGE; 3]);
    let vexes = cx
        .entities
        .iter()
        .filter(|e| {
            e.kind == EntityType::Vex
                && e.living.as_ref().is_some_and(|l| l.alive)
                && super::physics::intersects(area, e.bounds)
        })
        .count();
    cx.rng.gen_range(0..8) + 1 > vexes
}

/// The summon spell: up to three vexes around the evoker's feet, bound to
/// where they were summoned and, unlike a plain `/summon`ed vex, alive
/// only 30 to 120 seconds.
fn cast_summon_vex(mob: &mut MobCore, _target: &TargetView, cx: &mut TickContext) {
    let [ex, ey, ez] = mob.base.position;
    let (bx, by, bz) = (ex.floor() as i32, ey.floor() as i32, ez.floor() as i32);
    for _ in 0..3 {
        let origin = BlockPos::new(
            bx - 2 + cx.rng.gen_range(0..5),
            by + 1,
            bz - 2 + cx.rng.gen_range(0..5),
        );
        let position = [
            f64::from(origin.x) + 0.5,
            f64::from(origin.y),
            f64::from(origin.z) + 0.5,
        ];
        let base = EntityBase::new(
            EntityType::Vex,
            cx.shared.next_entity_id(),
            cx.rng.r#gen(),
            position,
            [0.0; 3],
        );
        let mut child = Mob::<Vex>::new(base, cx.rng);
        child.species.finalize_spawn(&mut child.core, cx);
        let life = 20 * (30 + cx.rng.gen_range(0..90));
        child
            .species
            .bind(&mut child.core, mob.base.uuid, origin, life);
        cx.effects.push(Effect::Spawn(Box::new(child)));
    }
}

#[derive(Debug, Default)]
pub struct Evoker;

impl Species for Evoker {
    fn create(_rng: &mut StdRng) -> Self {
        Self
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 10;
        let mut goals = GoalSelector::default();
        goals.add(2, AvoidPlayer::new(8.0, 0.6, 1.0));
        goals.add(3, AvoidPlayer::entities(illager::CREAKING, 8.0, 0.6, 1.0));
        goals.add(1, HurtByTarget::new(&[]).alerting(illager::RAIDERS));
        goals.add(2, NearestAttackablePlayer::with_interval(10, true, None));
        illager::add_villager_targets(&mut goals, 3, false, false);
        goals.add(
            1,
            CastSpell::new(
                illager::spell::FANGS,
                FANGS_CASTING,
                FANGS_INTERVAL,
                SoundEvent::EntityEvokerCastSpell,
                cast_fangs,
            )
            .prepare_sound(SoundEvent::EntityEvokerPrepareAttack),
        );
        goals.add(
            1,
            CastSpell::new(
                illager::spell::SUMMON_VEX,
                SUMMON_CASTING,
                SUMMON_INTERVAL,
                SoundEvent::EntityEvokerCastSpell,
                cast_summon_vex,
            )
            .prepare_sound(SoundEvent::EntityEvokerPrepareSummon)
            .then(few_vexes_nearby),
        );
        goals.add(6, Wololo::default());
        goals.add(8, RandomStroll::water_avoiding(0.6));
        goals.add(9, LookAtPlayer::new(3.0));
        goals.add(9, RandomLookAround::default());
        raider::add_goals(&mut goals);
        core.goals = goals;
        raider::make_raider(core, true, SoundEvent::EntityEvokerCelebrate);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (illager::CELEBRATING, illager::celebrating(core)),
            (
                super::data::AGE_LOCKED,
                MetadataValue::Byte(core.signal as i8),
            ),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        super::monster::sounds(
            SoundEvent::EntityEvokerAmbient,
            SoundEvent::EntityEvokerHurt,
            SoundEvent::EntityEvokerDeath,
            None,
        )
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Evoker, super::load::<Evoker>);
    registry.register(EntityType::EvokerFangs, crate::fangs::load);
    registry.register_spawn_rule(EntityType::Evoker, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn evoker_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let byte = MetadataValue::Byte(0).type_id() as u8;
        let layout = EntityType::Evoker.data_layout();
        assert_eq!(layout.len(), super::super::data::AGE_LOCKED as usize + 1);
        assert_eq!(layout[illager::CELEBRATING as usize..], [boolean, byte]);
    }

    fn sheep(id: i32, blue: bool) -> tuff_core::world::entity::EntitySummary {
        use tuff_core::world::entity::{EntitySummary, LivingSummary};
        EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Sheep,
            position: [3.5, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [3.0, 64.0, 0.0, 4.0, 65.3, 1.0],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.0,
                in_water: false,
                seen_as_enemy: false,
                enemy: false,
                health: 8.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: blue,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn wololo_picks_a_blue_sheep_and_recolors_it_after_the_warmup() {
        let herd = [sheep(5, false), sheep(6, true)];
        crate::tests::with_entities(&herd, |mob, cx| {
            let mut goal = Wololo::default();
            assert!(goal.can_use(mob, cx));
            assert_eq!(goal.sheep, Some(6));
            goal.start(mob, cx);
            for _ in 0..WOLOLO_WARMUP {
                goal.tick(mob, cx);
            }
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::Notify {
                    target: 6,
                    notice: Notice::Wololo
                }
            )));
            goal.stop(mob, cx);
            assert!(!goal.can_use(mob, cx), "waits out the interval");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn wololo_needs_a_blue_sheep_and_no_target() {
        let herd = [sheep(5, false)];
        crate::tests::with_entities(&herd, |mob, cx| {
            assert!(!Wololo::default().can_use(mob, cx));
        });
        let herd = [sheep(6, true)];
        crate::tests::with_entities(&herd, |mob, cx| {
            mob.target = Some(1);
            assert!(!Wololo::default().can_use(mob, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_summon_gate_never_passes_with_eight_vexes_nearby_and_often_with_none() {
        let mut vexes: Vec<_> = (0..8).map(|i| sheep(10 + i, false)).collect();
        for vex in &mut vexes {
            vex.kind = EntityType::Vex;
        }
        crate::tests::with_entities(&vexes, |mob, cx| {
            for _ in 0..200 {
                assert!(!few_vexes_nearby(mob, cx));
            }
        });
        crate::tests::with_entities(&[], |mob, cx| {
            assert!((0..50).all(|_| few_vexes_nearby(mob, cx)));
        });
    }
}
