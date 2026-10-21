//! Attribute data from game code: each attribute's default value and
//! range, the base values living entity types start with, and the
//! modifiers mob effects apply.

use crate::item::{EquipmentSlotGroup, ModifierOperation};
use crate::registry::{Attribute, EntityType, MobEffect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttributeRange {
    pub default: f64,
    pub min: f64,
    pub max: f64,
}

/// A modifier a mob effect applies while active; `amount` is for
/// amplifier 0 and scales with `amplifier + 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectModifier {
    pub attribute: Attribute,
    pub id: &'static str,
    pub amount: f64,
    pub operation: ModifierOperation,
}

impl EffectModifier {
    pub fn amount_at(&self, amplifier: u8) -> f64 {
        self.amount * (amplifier as f64 + 1.0)
    }
}

impl Attribute {
    pub fn range(self) -> AttributeRange {
        RANGES[self.id() as usize]
    }

    /// Clamps a computed value into the attribute's range.
    pub fn sanitize(self, value: f64) -> f64 {
        let range = self.range();
        if value.is_nan() {
            range.min
        } else {
            value.clamp(range.min, range.max)
        }
    }
}

impl EntityType {
    /// The base value of `attribute` for this living entity type, or `None`
    /// if the type does not have it.
    pub fn base_attribute(self, attribute: Attribute) -> Option<f64> {
        let value = dense_bases()[self.id() as usize][attribute.id() as usize];
        (!value.is_nan()).then_some(value)
    }
}

fn dense_bases() -> &'static [[f64; Attribute::COUNT]] {
    static DENSE: std::sync::OnceLock<Box<[[f64; Attribute::COUNT]]>> = std::sync::OnceLock::new();
    DENSE.get_or_init(|| {
        BASES
            .iter()
            .map(|bases| {
                let mut row = [f64::NAN; Attribute::COUNT];
                for (attribute, value) in bases.iter() {
                    row[attribute.id() as usize] = *value;
                }
                row
            })
            .collect()
    })
}

impl MobEffect {
    pub fn attribute_modifiers(self) -> &'static [EffectModifier] {
        EFFECT_MODIFIERS[self.id() as usize]
    }
}

impl EquipmentSlotGroup {
    /// The id the network uses; the build checks it is the variant's index.
    pub const fn network_id(self) -> u32 {
        self as u32
    }

    pub fn from_network_id(id: u32) -> Option<Self> {
        Self::ALL.get(id as usize).copied()
    }
}

impl ModifierOperation {
    /// The id the network uses; the build checks it is the variant's index.
    pub const fn network_id(self) -> u32 {
        self as u32
    }

    pub fn from_network_id(id: u32) -> Option<Self> {
        Self::ALL.get(id as usize).copied()
    }
}

include!(concat!(env!("OUT_DIR"), "/attributes.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_bases_ranges_and_effect_modifiers() {
        let player = EntityType::Player;
        assert_eq!(player.base_attribute(Attribute::AttackDamage), Some(1.0));
        assert_eq!(player.base_attribute(Attribute::AttackSpeed), Some(4.0));
        assert_eq!(
            player.base_attribute(Attribute::SweepingDamageRatio),
            Some(0.0)
        );
        assert_eq!(
            EntityType::Tnt.base_attribute(Attribute::AttackDamage),
            None
        );
        assert_eq!(Attribute::KnockbackResistance.sanitize(3.0), 1.0);
        assert_eq!(Attribute::Armor.range().max, 30.0);
        let strength = MobEffect::Strength.attribute_modifiers();
        assert_eq!(strength.len(), 1);
        assert_eq!(strength[0].attribute, Attribute::AttackDamage);
        assert_eq!(strength[0].amount_at(1), 6.0);
        assert_eq!(strength[0].operation, ModifierOperation::AddValue);
        assert!(MobEffect::Regeneration.attribute_modifiers().is_empty());
        assert_eq!(EquipmentSlotGroup::Saddle.network_id(), 10);
        assert_eq!(
            ModifierOperation::from_network_id(2),
            Some(ModifierOperation::AddMultipliedTotal)
        );
        assert_eq!(MODIFIER_DISPLAYS, ["default", "hidden", "override"]);
    }
}
