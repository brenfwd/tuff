//! Illusioners: a legacy illager, still a real spawnable 26.3 entity type
//! (registered and referenced in the sound/data reports, though it has no
//! spawn egg) that shoots a bow, turns invisible for a minute at a time,
//! and blinds its target on hard difficulty.
//!
//! The mirror-image duplicates are a purely client-side rendering trick
//! layered on top of its invisibility (the client shows a few fake copies
//! while the real illusioner is invisible); the server side of that spell
//! is just the invisibility effect below, applied the same way any other
//! mob's self-buff is.

use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};

use super::goal::GoalSelector;
use super::goals::{AvoidPlayer, LookAtPlayer, RandomLookAround, RandomStroll};
use super::illager::{self, CastSpell};
use super::raider;
use super::ranged::RangedBowAttack;
use super::target::{HurtByTarget, NearestAttackablePlayer, TargetView};
use super::{MobCore, MobSounds, Species};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::effects::PotionHit;
use tuff_core::world::entity::{Effect, TickContext};
use tuff_item::item::{ItemStack, MintReason};

const MIRROR_CASTING: i32 = 20;
const MIRROR_INTERVAL: i32 = 340;
const MIRROR_DURATION: i64 = 1200;
const BLINDNESS_CASTING: i32 = 20;
const BLINDNESS_INTERVAL: i32 = 180;
const BLINDNESS_DURATION: i64 = 400;
const BOW_RANGE: f32 = 15.0;
const BOW_INTERVAL: i32 = 20;

fn cast_disappear(mob: &mut MobCore, _target: &TargetView, _cx: &mut TickContext) {
    mob.effects.add(ActiveEffect::timed(
        MobEffect::Invisibility,
        MIRROR_DURATION,
        0,
    ));
}

fn cast_blindness(_mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
    let effect = ActiveEffect::timed(MobEffect::Blindness, BLINDNESS_DURATION, 0);
    if target.is_player() {
        let hit = PotionHit {
            effects: vec![effect],
            instant: Vec::new(),
            extinguish: false,
        };
        cx.effects.push(Effect::Potion {
            target: target.id,
            hit: Box::new(hit),
        });
    } else {
        cx.effects.push(Effect::MobEffects {
            target: target.id,
            effects: vec![effect],
        });
    }
}

fn not_already_invisible(core: &MobCore, _cx: &TickContext) -> bool {
    !core.effects.has(MobEffect::Invisibility)
}

fn harder_than_normal(core: &MobCore, cx: &TickContext) -> bool {
    super::monster::LocalDifficulty::at(cx, core.base.position).effective > 2.0
}

#[derive(Debug, Default)]
pub struct Illusioner;

impl Species for Illusioner {
    fn create(_rng: &mut StdRng) -> Self {
        Self
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        let mut goals = GoalSelector::default();
        goals.add(3, AvoidPlayer::entities(illager::CREAKING, 8.0, 1.0, 1.2));
        goals.add(1, HurtByTarget::new(&[]).alerting(illager::RAIDERS));
        goals.add(2, NearestAttackablePlayer::with_interval(10, true, None));
        illager::add_villager_targets(&mut goals, 3, false, false);
        goals.add(
            1,
            CastSpell::new(
                illager::spell::DISAPPEAR,
                MIRROR_CASTING,
                MIRROR_INTERVAL,
                SoundEvent::EntityIllusionerCastSpell,
                cast_disappear,
            )
            .prepare_sound(SoundEvent::EntityIllusionerPrepareMirror)
            .when(not_already_invisible),
        );
        goals.add(
            1,
            CastSpell::new(
                illager::spell::BLINDNESS,
                BLINDNESS_CASTING,
                BLINDNESS_INTERVAL,
                SoundEvent::EntityIllusionerCastSpell,
                cast_blindness,
            )
            .prepare_sound(SoundEvent::EntityIllusionerPrepareBlindness)
            .when(harder_than_normal)
            .once_per_target(),
        );
        goals.add(
            2,
            RangedBowAttack::new(
                0.5,
                |_| BOW_INTERVAL,
                BOW_RANGE,
                SoundEvent::EntitySkeletonShoot,
            ),
        );
        goals.add(8, RandomStroll::water_avoiding(0.6));
        goals.add(9, LookAtPlayer::new(3.0));
        goals.add(9, RandomLookAround::default());
        raider::add_goals(&mut goals);
        core.goals = goals;
        raider::make_raider(core, true, SoundEvent::EntityIllusionerAmbient);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::mint(Item::Bow, 1, MintReason::Spawn)),
        );
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
            SoundEvent::EntityIllusionerAmbient,
            SoundEvent::EntityIllusionerHurt,
            SoundEvent::EntityIllusionerDeath,
            None,
        )
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Illusioner, super::load::<Illusioner>);
    registry.register_spawn_rule(EntityType::Illusioner, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn illusioner_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let byte = MetadataValue::Byte(0).type_id() as u8;
        let layout = EntityType::Illusioner.data_layout();
        assert_eq!(layout.len(), super::super::data::AGE_LOCKED as usize + 1);
        assert_eq!(layout[illager::CELEBRATING as usize..], [boolean, byte]);
    }
}
