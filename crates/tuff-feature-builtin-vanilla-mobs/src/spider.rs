//! Spiders: they climb walls, leap at their prey, and only go looking for
//! a fight in the dark (by day they are neutral unless hurt). Cave spiders
//! are a smaller, weaker variant whose bite poisons whatever it hits.

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};

use super::attack::{LeapAtTarget, MeleeAttack};
use super::goal::GoalSelector;
use super::goals::{AvoidPlayer, Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster::{self, LocalDifficulty};
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer, TargetView};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{EntityHit, TickContext};
use tuff_core::world::player::Difficulty;

const FLAGS: u8 = data::BABY;
const FLAG_CLIMBING: i8 = 1;

fn in_the_dark(mob: &MobCore, cx: &TickContext) -> bool {
    mob.brightness(cx) < 0.5
}

/// A cave spider's bite poisons its target: seven seconds on normal, fifteen
/// on hard, nothing on easy.
fn poison_hook(mob: &MobCore, _target: &TargetView, hit: &mut EntityHit, cx: &mut TickContext) {
    let seconds = match LocalDifficulty::at(cx, mob.base.position).base {
        Difficulty::Normal => 7,
        Difficulty::Hard => 15,
        _ => 0,
    };
    if seconds > 0 {
        hit.effects.push(ActiveEffect::timed(
            tuff_vanilla_data::registry::MobEffect::Poison,
            seconds * 20,
            0,
        ));
    }
}

/// On hard difficulty a spawn sometimes carries a lasting effect.
fn spawn_effect(difficulty: LocalDifficulty, rng: &mut rand::rngs::StdRng) -> Option<MobEffect> {
    if difficulty.base != Difficulty::Hard
        || rng.r#gen::<f32>() >= 0.1 * difficulty.special_multiplier()
    {
        return None;
    }
    Some(match rng.gen_range(0..5) {
        0 | 1 => MobEffect::Speed,
        2 => MobEffect::Strength,
        3 => MobEffect::Regeneration,
        _ => MobEffect::Invisibility,
    })
}

#[derive(Debug, Default)]
pub struct Spider;

impl Species for Spider {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.navigation.climber = true;
        if core.kind() == EntityType::CaveSpider {
            core.attack_hook = Some(poison_hook);
        }
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]));
        goals.add(2, NearestAttackablePlayer::new(true).when(in_the_dark));
        goals.add(
            3,
            NearestAttackableEntity::new(&[EntityType::IronGolem], true).when(in_the_dark),
        );
        goals.add(1, Float::new(core));
        goals.add(
            2,
            AvoidPlayer::entities(&[EntityType::Armadillo], 6.0, 1.0, 1.2),
        );
        goals.add(3, LeapAtTarget::new(0.4));
        goals.add(4, MeleeAttack::new(1.0, true).shy());
        goals.add(5, RandomStroll::water_avoiding(0.8));
        goals.add(6, LookAtPlayer::new(8.0));
        goals.add(6, RandomLookAround::default());
        core.goals = goals;
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let flags = if core.climbing { FLAG_CLIMBING } else { 0 };
        vec![(FLAGS, MetadataValue::Byte(flags))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntitySpiderAmbient,
            SoundEvent::EntitySpiderHurt,
            SoundEvent::EntitySpiderDeath,
            Some(SoundEvent::EntitySpiderStep),
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.kind() == EntityType::CaveSpider {
            return;
        }
        if cx.rng.gen_range(0..100) == 0 {
            carry_skeleton(core);
        }
        if let Some(effect) = spawn_effect(LocalDifficulty::at(cx, core.base.position), cx.rng) {
            core.effects.add(ActiveEffect::timed(
                effect,
                tuff_core::world::effects::INFINITE,
                0,
            ));
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        let climbing = core.body.horizontal_collision;
        if core.climbing != climbing {
            core.climbing = climbing;
            core.changed(FLAGS);
        }
    }
}

/// A spider jockey: a fresh skeleton boards when the spider is added.
pub fn carry_skeleton(core: &mut MobCore) {
    core.base.passengers.push(
        tuff_nbt::Compound::new()
            .with("id", EntityType::Skeleton.name())
            .with(tuff_core::world::entity::FINALIZE_SPAWN, true),
    );
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Spider, super::load::<Spider>);
    registry.register_spawn_rule(EntityType::Spider, super::spawning::monster_rule);
    registry.register(EntityType::CaveSpider, super::load::<Spider>);
    registry.register_spawn_rule(EntityType::CaveSpider, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[tuff_core::watchdog]
    #[test]
    fn only_hard_spawns_carry_a_lasting_effect() {
        let hard = LocalDifficulty {
            base: Difficulty::Hard,
            effective: 6.0,
        };
        let normal = LocalDifficulty {
            base: Difficulty::Normal,
            effective: 6.0,
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let rolled: Vec<_> = (0..2000)
            .filter_map(|_| spawn_effect(hard, &mut rng))
            .collect();
        assert!((120..280).contains(&rolled.len()), "{}", rolled.len());
        for effect in [
            MobEffect::Speed,
            MobEffect::Strength,
            MobEffect::Regeneration,
            MobEffect::Invisibility,
        ] {
            assert!(rolled.contains(&effect));
        }
        assert!((0..2000).all(|_| spawn_effect(normal, &mut rng).is_none()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn one_spider_in_a_hundred_carries_a_fresh_skeleton() {
        let jockeys = |kind: EntityType| {
            let mut count = 0;
            crate::tests::with_context(|_, cx| {
                for seed in 0..3000 {
                    *cx.rng = rand::rngs::StdRng::seed_from_u64(seed);
                    let base = tuff_core::world::entity::EntityBase::new(
                        kind,
                        9,
                        9,
                        [0.5, 64.0, 0.5],
                        [0.0; 3],
                    );
                    let mut spider = crate::Mob::<Spider>::new(base, cx.rng);
                    spider.species.finalize_spawn(&mut spider.core, cx);
                    if let Some(rider) = spider.core.base.passengers.first() {
                        assert_eq!(rider.get_str("id"), Some("minecraft:skeleton"));
                        assert!(rider.contains_key(tuff_core::world::entity::FINALIZE_SPAWN));
                        count += 1;
                    }
                }
            });
            count
        };
        let spiders = jockeys(EntityType::Spider);
        assert!((10..55).contains(&spiders), "{spiders}");
        assert_eq!(jockeys(EntityType::CaveSpider), 0);
    }
}
