//! The data pack's `minecraft:sulfur_cube_archetype` entries: how the block
//! a sulfur cube swallowed changes it.

use crate::damage_type::DamageType;
use crate::item::ModifierOperation;
use crate::registry::{Attribute, SoundEvent};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Archetype {
    pub name: &'static str,
    pub items: Items,
    pub attribute_modifiers: &'static [ArchetypeModifier],
    /// It floats up in water and lava.
    pub buoyant: bool,
    pub explosion: Option<ExplosionData>,
    pub contact_damage: Option<ContactDamage>,
    pub knockback: KnockbackModifiers,
    pub sounds: SoundSettings,
}

/// The items an archetype applies to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Items {
    /// An item tag, by name.
    Tag(&'static str),
    List(&'static [crate::Item]),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArchetypeModifier {
    pub attribute: Attribute,
    pub id: &'static str,
    pub amount: f64,
    pub operation: ModifierOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplosionData {
    pub power: i64,
    pub causes_fire: bool,
    pub fuse: i64,
}

/// Damage dealt to what it touches; `min..max` is the uniform amount.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactDamage {
    pub damage_type: &'static DamageType,
    pub min: f32,
    pub max: f32,
    /// Blames the cube even when a non-player is hurt.
    pub attribute_to_source: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnockbackModifiers {
    pub horizontal_power: f32,
    pub vertical_power: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundSettings {
    pub hit: SoundEvent,
    pub push: SoundEvent,
    /// The push speed above which the push sound plays.
    pub push_threshold: f32,
    /// Seconds between push sounds.
    pub push_cooldown: f32,
}

pub const DEFAULT_KNOCKBACK: KnockbackModifiers = KnockbackModifiers {
    horizontal_power: 0.33,
    vertical_power: 0.06,
};

pub const DEFAULT_SOUNDS: SoundSettings = SoundSettings {
    hit: SoundEvent::EntitySulfurCubeRegularHit,
    push: SoundEvent::EntitySulfurCubeRegularPush,
    push_threshold: 0.2,
    push_cooldown: 0.5,
};

include!(concat!(env!("OUT_DIR"), "/sulfur_cube.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tnt_makes_an_explosive_cube_and_magma_a_hot_one() {
        let explosive = ALL
            .iter()
            .find(|a| a.name == "minecraft:explosive")
            .unwrap();
        assert_eq!(
            explosive.explosion,
            Some(ExplosionData {
                power: 3,
                causes_fire: false,
                fuse: 120
            })
        );
        assert_eq!(
            explosive.items,
            Items::Tag("minecraft:sulfur_cube_archetype/explosive")
        );
        let hot = ALL.iter().find(|a| a.name == "minecraft:hot").unwrap();
        let contact = hot.contact_damage.unwrap();
        assert_eq!(contact.damage_type.name, "minecraft:sulfur_cube_hot");
        assert_eq!((contact.min, contact.max), (1.0, 1.0));
        assert!(ALL.iter().all(|a| a.attribute_modifiers.len() == 5));
    }
}
