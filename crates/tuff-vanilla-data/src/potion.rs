//! Potions and mob effects as the game defines them in code: each potion's
//! effects, and each effect's category, color and particle.

use crate::item::EffectInstance;
use crate::registry::{MobEffect, ParticleType, Potion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCategory {
    Beneficial,
    Harmful,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectInfo {
    pub category: EffectCategory,
    /// RGB.
    pub color: i32,
    pub instantaneous: bool,
    /// The particle an entity with this effect shows; `None` is the entity
    /// effect particle tinted with `color`.
    pub particle: Option<ParticleType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PotionInfo {
    /// The suffix of the potion item's translation keys.
    pub name: &'static str,
    pub effects: &'static [EffectInstance],
}

impl MobEffect {
    pub fn info(self) -> &'static EffectInfo {
        &EFFECTS[self.id() as usize]
    }

    pub fn is_beneficial(self) -> bool {
        self.info().category == EffectCategory::Beneficial
    }

    pub fn is_instantaneous(self) -> bool {
        self.info().instantaneous
    }
}

impl Potion {
    pub fn info(self) -> &'static PotionInfo {
        &POTIONS[self.id() as usize]
    }

    pub fn effects(self) -> &'static [EffectInstance] {
        self.info().effects
    }
}

include!(concat!(env!("OUT_DIR"), "/potions.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn potions_carry_their_effects_and_effects_their_colors() {
        let fire = Potion::LongFireResistance.effects();
        assert_eq!(fire.len(), 1);
        assert_eq!(fire[0].effect, MobEffect::FireResistance);
        assert_eq!(fire[0].duration, 9600);
        assert_eq!(Potion::StrongHealing.effects()[0].amplifier, 1);
        assert_eq!(Potion::LongFireResistance.info().name, "fire_resistance");
        assert!(Potion::Water.effects().is_empty());
        assert!(Potion::Awkward.effects().is_empty());
        assert_eq!(MobEffect::Speed.info().color, 3402751);
        assert!(MobEffect::InstantHealth.is_instantaneous());
        assert!(!MobEffect::Poison.is_beneficial());
        assert_eq!(MobEffect::Speed.info().particle, None);
        assert_eq!(
            MobEffect::TrialOmen.info().particle,
            Some(ParticleType::TrialOmen)
        );
        assert_eq!(AMBIENT_ALPHA, 38);
    }
}
