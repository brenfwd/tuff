//! Ravagers: large, heavy melee raid mobs. A bite roots them for a moment;
//! a raised shield may stun them, and a stunned ravager roars when it
//! recovers, hurting and throwing back everything around it. They trample
//! through leaves.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::PathType;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::tag;

use super::attack::MeleeAttack;
use super::goal::GoalSelector;
use super::goals::{Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::physics;
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer, TargetView};
use super::{MobCore, MobSounds, Species};
use super::{illager, raider};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Culprit, Effect, EntityHit, Notice, TickContext, take_number};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

pub const ATTACK_DURATION: i32 = 10;
pub const STUN_DURATION: i32 = 40;
pub const ROAR_DURATION: i32 = 20;
const ROAR_AT: i32 = 10;
const ROAR_RADIUS: f64 = 4.0;
const ROAR_DAMAGE: f32 = 6.0;
const BASE_SPEED: f64 = 0.3;
const ATTACK_SPEED: f64 = 0.35;

pub mod event {
    pub const ATTACK: i8 = 4;
    pub const STUNNED: i8 = 39;
    pub const ROAR: i8 = 69;
}

const ILLAGERS: &[EntityType] = &[
    EntityType::Evoker,
    EntityType::Illusioner,
    EntityType::Pillager,
    EntityType::Vindicator,
];

fn attack_hook(mob: &MobCore, _target: &TargetView, hit: &mut EntityHit, cx: &mut TickContext) {
    hit.report_blocked = true;
    mob.entity_event(event::ATTACK, cx);
    mob.sound(SoundEvent::EntityRavagerAttack, 1.0, 1.0, cx);
}

fn bit(mob: &mut MobCore) {
    mob.signal = ATTACK_DURATION;
}

/// Its stun and roar timers. The bite timer lives in `MobCore::signal`
/// so the melee goal can start it.
#[derive(Debug, Default)]
pub struct Ravager {
    stunned: i32,
    roar: i32,
    /// The defender whose shield stopped its last bite: it reacts on its
    /// next tick.
    blocked_by: Option<i32>,
}

fn is_trampled_crop(block: Block) -> bool {
    matches!(
        block,
        Block::PitcherCrop
            | Block::Wheat
            | Block::Carrots
            | Block::Potatoes
            | Block::Beetroots
            | Block::TorchflowerCrop
    )
}

impl Ravager {
    pub fn stunned_ticks(&self) -> i32 {
        self.stunned
    }

    pub fn roar_ticks(&self) -> i32 {
        self.roar
    }

    fn position_of(id: i32, cx: &TickContext) -> Option<[f64; 3]> {
        cx.players
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.position)
            .or_else(|| cx.entities.iter().find(|e| e.id == id).map(|e| e.position))
    }

    fn strong_knockback(core: &MobCore, target: i32, at: [f64; 3], cx: &mut TickContext) {
        let dx = at[0] - core.base.position[0];
        let dz = at[2] - core.base.position[2];
        let dd = (dx * dx + dz * dz).max(0.001);
        cx.effects.push(Effect::Push {
            target,
            push: [dx / dd * 4.0, 0.2, dz / dd * 4.0],
        });
    }

    /// A raised shield stopped its bite: half the time it is stunned and
    /// the two are nudged apart, else the defender is thrown back.
    fn shield_blocked(&mut self, core: &mut MobCore, defender: i32, cx: &mut TickContext) {
        if self.roar != 0 {
            return;
        }
        let Some(at) = Self::position_of(defender, cx) else {
            return;
        };
        if cx.rng.r#gen::<f64>() >= 0.5 {
            Self::strong_knockback(core, defender, at, cx);
            return;
        }
        self.stunned = STUN_DURATION;
        core.sound(SoundEvent::EntityRavagerStunned, 1.0, 1.0, cx);
        core.entity_event(event::STUNNED, cx);
        let xa = core.base.position[0] - at[0];
        let za = core.base.position[2] - at[2];
        let dd = xa.abs().max(za.abs());
        if dd < 0.009_999_999_776_482_582 {
            return;
        }
        let root = dd.sqrt();
        let power = (1.0 / root).min(1.0);
        let push_x = xa / root * power * 0.050_000_000_745_058_06;
        let push_z = za / root * power * 0.050_000_000_745_058_06;
        cx.effects.push(Effect::Push {
            target: defender,
            push: [-push_x, 0.0, -push_z],
        });
        if core.base.riders.is_empty() && core.base.entity_riders.is_empty() {
            core.base.velocity[0] += push_x;
            core.base.velocity[2] += push_z;
            core.impulse = true;
        }
    }

    fn roar(core: &MobCore, cx: &mut TickContext) {
        let griefing = cx.shared.rule_bool("mob_griefing");
        let area = physics::inflate(core.bounds(), [ROAR_RADIUS; 3]);
        let mut hit = EntityHit::new(ROAR_DAMAGE, &damage_type::MOB_ATTACK);
        hit.attacker = Some(Culprit::entity(&core.base));
        hit.position = Some(core.base.position);
        for player in cx.players {
            if physics::intersects(player.bounds(), area) {
                cx.effects.push(Effect::Hurt {
                    target: player.id,
                    hit: Box::new(hit.clone()),
                });
            }
        }
        let victims: Vec<(i32, EntityType, [f64; 3])> = cx
            .entities
            .iter()
            .filter(|e| {
                e.id != core.base.id
                    && e.kind != EntityType::Ravager
                    && e.living.is_some_and(|l| l.alive)
                    && (griefing || e.kind != EntityType::ArmorStand)
                    && physics::intersects(e.bounds, area)
            })
            .map(|e| (e.id, e.kind, e.position))
            .collect();
        for (id, kind, position) in victims {
            if !ILLAGERS.contains(&kind) {
                cx.effects.push(Effect::Hurt {
                    target: id,
                    hit: Box::new(hit.clone()),
                });
            }
            Self::strong_knockback(core, id, position, cx);
        }
        cx.effects.push(Effect::GameEvent {
            event: tuff_core::world::game_event::GameEvent::EntityAction,
            position: core.base.position,
            source: Some(core.base.id),
        });
        core.entity_event(event::ROAR, cx);
    }

    /// Any crop the ravager stands inside is destroyed while
    /// griefing is on.
    fn trample_crops(core: &mut MobCore, cx: &mut TickContext) {
        if !cx.shared.rule_bool("mob_griefing") {
            return;
        }
        let [x0, y0, z0, x1, y1, z1] = physics::inflate(core.bounds(), [-1.0e-5; 3]);
        for x in x0.floor() as i32..=x1.floor() as i32 {
            for y in y0.floor() as i32..=y1.floor() as i32 {
                for z in z0.floor() as i32..=z1.floor() as i32 {
                    let pos = BlockPos::new(x, y, z);
                    if (cx.block)(pos).is_some_and(|s| is_trampled_crop(s.block())) {
                        cx.effects.push(Effect::DestroyBlock { pos, drop: true });
                    }
                }
            }
        }
    }

    /// Bumping into leaves while griefing is on breaks them; bumping into
    /// anything else makes it jump.
    fn trample(core: &mut MobCore, cx: &mut TickContext) {
        if !core.body.horizontal_collision || !cx.shared.rule_bool("mob_griefing") {
            return;
        }
        let [x0, y0, z0, x1, y1, z1] = physics::inflate(core.bounds(), [0.2; 3]);
        let mut destroyed = false;
        for x in x0.floor() as i32..=x1.floor() as i32 {
            for y in y0.floor() as i32..=y1.floor() as i32 {
                for z in z0.floor() as i32..=z1.floor() as i32 {
                    let pos = BlockPos::new(x, y, z);
                    if (cx.block)(pos).is_some_and(|s| {
                        tuff_datapack::tags::is(tag::block::key::LEAVES, s.block())
                    }) {
                        cx.effects.push(Effect::DestroyBlock { pos, drop: true });
                        destroyed = true;
                    }
                }
            }
        }
        if !destroyed && core.base.on_ground {
            core.jump_from_ground(cx);
        }
    }
}

impl Species for Ravager {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 20;
        core.attack_hook = Some(attack_hook);
        core.max_head_y_rot = 45.0;
        core.set_path_malus(PathType::Leaves, 0.0);
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(2, HurtByTarget::new(&[]).alerting(illager::RAIDERS));
        goals.add(3, NearestAttackablePlayer::new(true));
        goals.add(
            4,
            NearestAttackableEntity::new(
                &[EntityType::Villager, EntityType::WanderingTrader],
                true,
            )
            .only(|e| e.living.is_none_or(|l| l.age >= 0)),
        );
        goals.add(
            4,
            NearestAttackableEntity::new(&[EntityType::IronGolem], true),
        );
        goals.add(4, MeleeAttack::new(1.0, true).with_on_hit(bit));
        goals.add(5, RandomStroll::water_avoiding(0.4));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(9, RandomLookAround::default());
        raider::add_goals(&mut goals);
        core.goals = goals;
        raider::make_raider(core, false, SoundEvent::EntityRavagerCelebrate);
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        core.signal = take_number(nbt, "AttackTick").map_or(0, |v| v as i32);
        self.stunned = take_number(nbt, "StunTick").map_or(0, |v| v as i32);
        self.roar = take_number(nbt, "RoarTick").map_or(0, |v| v as i32);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("AttackTick", core.signal);
        out.insert("StunTick", self.stunned);
        out.insert("RoarTick", self.roar);
    }

    fn heard(&mut self, _core: &mut MobCore, notice: Notice) {
        if let Notice::ShieldBlocked { defender } = notice {
            self.blocked_by = Some(defender);
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(defender) = self.blocked_by.take()
            && core.is_alive()
        {
            self.shield_blocked(core, defender, cx);
        }
        core.blind = self.stunned > 0 || self.roar > 0;
    }

    fn immobile(&self, core: &MobCore) -> bool {
        core.signal > 0 || self.stunned > 0 || self.roar > 0
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let speed = if self.immobile(core) {
            0.0
        } else {
            let wanted = if core.target.is_some() {
                ATTACK_SPEED
            } else {
                BASE_SPEED
            };
            let base = core.attribute_base(Attribute::MovementSpeed);
            base + 0.1 * (wanted - base)
        };
        core.set_attribute_base(Attribute::MovementSpeed, speed);
        Self::trample(core, cx);
        Self::trample_crops(core, cx);
        if self.roar > 0 {
            self.roar -= 1;
            if self.roar == ROAR_AT {
                Self::roar(core, cx);
            }
        }
        if core.signal > 0 {
            core.signal -= 1;
        }
        if self.stunned > 0 {
            self.stunned -= 1;
            if self.stunned == 0 {
                core.sound(SoundEvent::EntityRavagerRoar, 1.0, 1.0, cx);
                self.roar = ROAR_DURATION;
            }
        }
        core.blind = self.stunned > 0 || self.roar > 0;
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(illager::CELEBRATING, illager::celebrating(core))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        super::monster::sounds(
            SoundEvent::EntityRavagerAmbient,
            SoundEvent::EntityRavagerHurt,
            SoundEvent::EntityRavagerDeath,
            Some(SoundEvent::EntityRavagerStep),
        )
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Ravager, super::load::<Ravager>);
    registry.register_spawn_rule(EntityType::Ravager, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mob;
    use tuff_core::world::entity::{EntityBase, EntitySummary, LivingSummary, RegionEntity};

    fn ravager(cx: &mut TickContext) -> Mob<Ravager> {
        let base = EntityBase::new(EntityType::Ravager, 2, 2, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut mob = Mob::<Ravager>::new(base, cx.rng);
        mob.core.no_ai = true;
        mob
    }

    fn living(id: i32, kind: EntityType, x: f64) -> EntitySummary {
        EntitySummary {
            id,
            uuid: id as u128,
            kind,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.3, 64.0, 0.2, x + 0.3, 65.9, 0.8],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.6,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 20.0,
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

    fn pushes(cx: &TickContext, target: i32) -> Vec<[f64; 3]> {
        cx.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Push { target: t, push } if *t == target => Some(*push),
                _ => None,
            })
            .collect()
    }

    fn hurts(cx: &TickContext, target: i32) -> Vec<f32> {
        cx.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Hurt { target: t, hit } if *t == target => Some(hit.amount),
                _ => None,
            })
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_blocked_bite_stuns_it_or_throws_the_defender_back() {
        let shield = [living(7, EntityType::Pillager, 2.5)];
        crate::tests::with_entities(&shield, |_, cx| {
            let (mut stunned, mut thrown) = (0, 0);
            for _ in 0..40 {
                let mut mob = ravager(cx);
                cx.effects.clear();
                mob.notice(Notice::ShieldBlocked { defender: 7 });
                mob.tick(cx);
                let pushed = pushes(cx, 7);
                if mob.species.stunned_ticks() > 0 {
                    stunned += 1;
                    assert_eq!(pushed.len(), 1);
                    assert!(pushed[0][0] > 0.0 && pushed[0][0] < 0.051, "{pushed:?}");
                    assert!(mob.core.blind, "a stunned ravager sees nothing");
                    assert!(mob.species.immobile(&mob.core));
                } else {
                    thrown += 1;
                    assert_eq!(pushed, [[2.0, 0.2, 0.0]], "4 / distance, sideways");
                }
            }
            assert!(
                stunned > 5 && thrown > 5,
                "{stunned} stunned, {thrown} thrown"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stun_ends_in_a_roar_that_hurts_and_throws_its_neighbours() {
        let around = [
            living(7, EntityType::Zombie, 3.0),
            living(8, EntityType::Pillager, -2.0),
            living(9, EntityType::Ravager, 1.5),
            living(10, EntityType::Zombie, 9.0),
        ];
        crate::tests::with_entities(&around, |_, cx| {
            let mut mob = ravager(cx);
            mob.species.stunned = STUN_DURATION;
            for _ in 0..STUN_DURATION {
                mob.tick(cx);
            }
            assert_eq!(mob.species.stunned_ticks(), 0);
            assert_eq!(mob.species.roar_ticks(), ROAR_DURATION);
            assert!(mob.core.blind, "still blind while roaring");
            cx.effects.clear();
            for _ in 0..ROAR_DURATION - ROAR_AT {
                mob.tick(cx);
            }
            assert_eq!(hurts(cx, 7), [ROAR_DAMAGE]);
            assert_eq!(pushes(cx, 7).len(), 1);
            assert!(hurts(cx, 8).is_empty(), "illagers are spared the damage");
            assert_eq!(pushes(cx, 8).len(), 1, "but not the push");
            assert!(hurts(cx, 9).is_empty() && pushes(cx, 9).is_empty());
            assert!(hurts(cx, 10).is_empty(), "too far");
            for _ in 0..ROAR_AT {
                mob.tick(cx);
            }
            assert!(!mob.core.blind && !mob.species.immobile(&mob.core));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bite_roots_it_for_its_attack_time() {
        crate::tests::with_context(|_, cx| {
            let mut mob = ravager(cx);
            mob.core.no_ai = false;
            bit(&mut mob.core);
            for _ in 0..ATTACK_DURATION {
                assert!(mob.species.immobile(&mob.core));
                mob.tick(cx);
            }
            mob.tick(cx);
            assert!(!mob.species.immobile(&mob.core));
            assert_eq!(
                mob.core.attribute_base(Attribute::MovementSpeed),
                BASE_SPEED * 0.1,
                "speed creeps back from zero"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_tramples_leaves_it_bumps_into() {
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else if pos.x == 2 && pos.y < 67 {
                Block::OakLeaves.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        crate::tests::with_world(&block, &[], |_, cx| {
            let mut mob = ravager(cx);
            mob.core.base.on_ground = true;
            mob.core.base.velocity = [0.6, 0.0, 0.0];
            mob.tick(cx);
            let broken: Vec<BlockPos> = cx
                .effects
                .iter()
                .filter_map(|e| match e {
                    Effect::DestroyBlock { pos, drop: true } => Some(*pos),
                    _ => None,
                })
                .collect();
            assert!(!broken.is_empty(), "it broke the leaves it hit");
            assert!(broken.iter().all(|p| p.x == 2));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_destroys_a_pitcher_crop_it_stands_in() {
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else if pos.y == 64 {
                Block::PitcherCrop.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        crate::tests::with_world(&block, &[], |_, cx| {
            let mut mob = ravager(cx);
            mob.tick(cx);
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DestroyBlock { drop: true, .. })),
                "the crop is destroyed"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_destroys_a_wheat_crop_it_stands_in() {
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else if pos.y == 64 {
                Block::Wheat.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        crate::tests::with_world(&block, &[], |_, cx| {
            let mut mob = ravager(cx);
            mob.tick(cx);
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DestroyBlock { drop: true, .. }))
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn ravager_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::Ravager.data_layout();
        assert_eq!(layout.len(), illager::CELEBRATING as usize + 1);
        assert_eq!(layout[illager::CELEBRATING as usize], boolean);
    }
}
