use tuff_vanilla_data::item::{
    ConsumeEffect, EffectInstance, KineticCondition, KineticWeapon, PiercingWeapon, UseAnimation,
};
use tuff_vanilla_data::registry::{MobEffect, SoundEvent};

use super::{ItemStack, as_i64};
use tuff_nbt::{Compound, Tag};

const CONSUMABLE: &str = "minecraft:consumable";
const KINETIC_WEAPON: &str = "minecraft:kinetic_weapon";
const PIERCING_WEAPON: &str = "minecraft:piercing_weapon";

/// A stack's effective `consumable` component.
#[derive(Debug, Clone, PartialEq)]
pub struct StackConsumable {
    pub consume_seconds: f32,
    pub animation: UseAnimation,
    pub sound: SoundEvent,
    pub has_consume_particles: bool,
    pub on_consume_effects: Vec<StackConsumeEffect>,
}

impl StackConsumable {
    pub fn consume_ticks(&self) -> u32 {
        (self.consume_seconds * 20.0) as u32
    }

    fn from_default(consumable: &tuff_vanilla_data::item::Consumable) -> Self {
        Self {
            consume_seconds: consumable.consume_seconds,
            animation: consumable.animation,
            sound: consumable.sound,
            has_consume_particles: consumable.has_consume_particles,
            on_consume_effects: consumable
                .on_consume_effects
                .iter()
                .map(StackConsumeEffect::from)
                .collect(),
        }
    }

    fn from_nbt(nbt: &Compound) -> Option<Self> {
        let consume_seconds = match nbt.get("consume_seconds") {
            Some(tag) => tag.as_float().filter(|s| *s >= 0.0)?,
            None => 1.6,
        };
        let animation = match nbt.get("animation") {
            Some(tag) => UseAnimation::from_name(strip(tag.as_str()?))?,
            None => UseAnimation::Eat,
        };
        let sound = match nbt.get("sound") {
            Some(tag) => sound(tag).unwrap_or(SoundEvent::EntityGenericEat),
            None => SoundEvent::EntityGenericEat,
        };
        let has_consume_particles = flag(nbt, "has_consume_particles", true)?;
        let on_consume_effects = match nbt.get("on_consume_effects") {
            Some(tag) => tag
                .as_list()?
                .iter()
                .map(StackConsumeEffect::from_nbt)
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Self {
            consume_seconds,
            animation,
            sound,
            has_consume_particles,
            on_consume_effects,
        })
    }
}

/// One entry of a stack's `consumable.on_consume_effects`.
#[derive(Debug, Clone, PartialEq)]
pub enum StackConsumeEffect {
    ApplyEffects {
        effects: Vec<EffectInstance>,
        probability: f32,
    },
    RemoveEffects(Vec<MobEffect>),
    ClearAllEffects,
    TeleportRandomly {
        diameter: f32,
        directional_particles: bool,
    },
    PlaySound(SoundEvent),
}

impl From<&ConsumeEffect> for StackConsumeEffect {
    fn from(effect: &ConsumeEffect) -> Self {
        match *effect {
            ConsumeEffect::ApplyEffects {
                effects,
                probability,
            } => Self::ApplyEffects {
                effects: effects.to_vec(),
                probability,
            },
            ConsumeEffect::RemoveEffects(kinds) => Self::RemoveEffects(kinds.to_vec()),
            ConsumeEffect::ClearAllEffects => Self::ClearAllEffects,
            ConsumeEffect::TeleportRandomly {
                diameter,
                directional_particles,
            } => Self::TeleportRandomly {
                diameter,
                directional_particles,
            },
            ConsumeEffect::PlaySound(sound) => Self::PlaySound(sound),
        }
    }
}

impl StackConsumeEffect {
    fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        Some(match strip(map.get_str("type")?) {
            "apply_effects" => Self::ApplyEffects {
                effects: map
                    .get("effects")?
                    .as_list()?
                    .iter()
                    .map(effect_instance)
                    .collect::<Option<Vec<_>>>()?,
                probability: match map.get("probability") {
                    Some(p) => p.as_float().filter(|p| (0.0..=1.0).contains(p))?,
                    None => 1.0,
                },
            },
            "remove_effects" => Self::RemoveEffects(match map.get("effects")? {
                Tag::List(list) => list
                    .iter()
                    .map(|t| MobEffect::from_name(t.as_str()?))
                    .collect::<Option<Vec<_>>>()?,
                other => vec![MobEffect::from_name(other.as_str()?)?],
            }),
            "clear_all_effects" => Self::ClearAllEffects,
            "teleport_randomly" => Self::TeleportRandomly {
                diameter: match map.get("diameter") {
                    Some(d) => d.as_float().filter(|d| *d > 0.0)?,
                    None => 16.0,
                },
                directional_particles: flag(map, "directional_particles", true)?,
            },
            "play_sound" => Self::PlaySound(sound(map.get("sound")?)?),
            _ => return None,
        })
    }
}

fn effect_instance(tag: &Tag) -> Option<EffectInstance> {
    let map = tag.as_compound()?;
    let effect = MobEffect::from_name(map.get_str("id")?)?;
    let num = |key: &str, default: i64| map.get(key).map_or(Some(default), as_i64);
    let show_particles = flag(map, "show_particles", true)?;
    Some(EffectInstance {
        effect,
        duration: num("duration", 0)?,
        amplifier: u64::try_from(num("amplifier", 0)?).ok()?.min(255),
        ambient: flag(map, "ambient", false)?,
        show_particles,
        show_icon: flag(map, "show_icon", show_particles)?,
    })
}

fn condition(tag: &Tag) -> Option<KineticCondition> {
    let map = tag.as_compound()?;
    Some(KineticCondition {
        max_duration_ticks: u64::try_from(as_i64(map.get("max_duration_ticks")?)?).ok()?,
        min_speed: float(map, "min_speed", 0.0)?,
        min_relative_speed: float(map, "min_relative_speed", 0.0)?,
    })
}

fn optional<T>(map: &Compound, key: &str, parse: fn(&Tag) -> Option<T>) -> Option<Option<T>> {
    map.get(key).map_or(Some(None), |tag| parse(tag).map(Some))
}

fn flag(map: &Compound, key: &str, default: bool) -> Option<bool> {
    map.get(key).map_or(Some(default), Tag::as_bool)
}

fn float(map: &Compound, key: &str, default: f32) -> Option<f32> {
    map.get(key).map_or(Some(default), Tag::as_float)
}

fn ticks(map: &Compound, key: &str, default: u64) -> Option<u64> {
    map.get(key)
        .map_or(Some(default), |t| u64::try_from(as_i64(t)?).ok())
}

fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// A sound event given by id or as `{sound_id, range}`; `None` for one the
/// server does not know.
fn sound(tag: &Tag) -> Option<SoundEvent> {
    let id = match tag {
        Tag::Compound(map) => map.get_str("sound_id")?,
        other => other.as_str()?,
    };
    SoundEvent::from_name(id).or_else(|| SoundEvent::from_name(strip(id)))
}

impl ItemStack {
    /// The stack's own raw component, `Err` when the stack removes it.
    fn own_component(&self, id: &str) -> Result<Option<&Compound>, ()> {
        match self.components.other.get(id) {
            Some(Tag::Compound(nbt)) => Ok(Some(nbt)),
            _ if self.components.is_removed(id) => Err(()),
            _ => Ok(None),
        }
    }

    /// The consumable component in effect: the stack's own, else the
    /// item's default unless the stack removes it.
    pub fn consumable(&self) -> Option<StackConsumable> {
        match self.own_component(CONSUMABLE).ok()? {
            Some(nbt) => StackConsumable::from_nbt(nbt),
            None => self.item.consumable().map(StackConsumable::from_default),
        }
    }

    /// The kinetic weapon component in effect (see [`Self::consumable`]).
    pub fn kinetic_weapon(&self) -> Option<KineticWeapon> {
        let Some(map) = self.own_component(KINETIC_WEAPON).ok()? else {
            return self.item.kinetic_weapon_data().copied();
        };
        Some(KineticWeapon {
            contact_cooldown_ticks: ticks(map, "contact_cooldown_ticks", 10)?,
            delay_ticks: ticks(map, "delay_ticks", 0)?,
            dismount_conditions: optional(map, "dismount_conditions", condition)?,
            knockback_conditions: optional(map, "knockback_conditions", condition)?,
            damage_conditions: optional(map, "damage_conditions", condition)?,
            forward_movement: float(map, "forward_movement", 0.0)?,
            damage_multiplier: float(map, "damage_multiplier", 1.0)?,
            sound: map.get("sound").and_then(sound),
            hit_sound: map.get("hit_sound").and_then(sound),
        })
    }

    /// The piercing weapon component in effect (see [`Self::consumable`]).
    pub fn piercing_weapon(&self) -> Option<PiercingWeapon> {
        let Some(map) = self.own_component(PIERCING_WEAPON).ok()? else {
            return self.item.piercing_weapon().copied();
        };
        Some(PiercingWeapon {
            deals_knockback: flag(map, "deals_knockback", true)?,
            dismounts: flag(map, "dismounts", false)?,
            sound: map.get("sound").and_then(sound),
            hit_sound: map.get("hit_sound").and_then(sound),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::registry::DataComponentType;

    fn with(item: Item, id: &str, nbt: Compound) -> ItemStack {
        let mut stack = ItemStack::new(item, 1);
        stack.components.other.insert(id, Tag::Compound(nbt));
        stack
    }

    #[tuff_core::watchdog]
    #[test]
    fn consumable_falls_back_to_the_item_default() {
        let apple = ItemStack::new(Item::Apple, 1).consumable().unwrap();
        assert_eq!(apple.consume_ticks(), 32);
        assert!(ItemStack::new(Item::Stone, 1).consumable().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn stack_consumable_overrides_the_default() {
        let nbt = Compound::new()
            .with("consume_seconds", 0.5f32)
            .with("animation", "drink")
            .with(
                "on_consume_effects",
                Tag::List(vec![Tag::Compound(
                    Compound::new()
                        .with("type", "minecraft:apply_effects")
                        .with(
                            "effects",
                            Tag::List(vec![Tag::Compound(
                                Compound::new()
                                    .with("id", "minecraft:speed")
                                    .with("duration", 100),
                            )]),
                        ),
                )]),
            );
        let stack = with(Item::Stone, CONSUMABLE, nbt);
        let consumable = stack.consumable().unwrap();
        assert_eq!(consumable.consume_ticks(), 10);
        assert_eq!(consumable.animation, UseAnimation::Drink);
        assert_eq!(consumable.sound, SoundEvent::EntityGenericEat);
        let [StackConsumeEffect::ApplyEffects { effects, .. }] =
            consumable.on_consume_effects.as_slice()
        else {
            panic!("{consumable:?}");
        };
        assert_eq!(effects[0].effect, MobEffect::Speed);
        assert_eq!(effects[0].duration, 100);
    }

    #[tuff_core::watchdog]
    #[test]
    fn removed_behavior_components_are_absent() {
        let mut apple = ItemStack::new(Item::Apple, 1);
        apple
            .components
            .remove_network(DataComponentType::Consumable);
        assert!(apple.consumable().is_none());
        let mut spear = ItemStack::new(Item::IronSpear, 1);
        spear
            .components
            .remove_network(DataComponentType::KineticWeapon);
        spear
            .components
            .remove_network(DataComponentType::PiercingWeapon);
        assert!(spear.kinetic_weapon().is_none());
        assert!(spear.piercing_weapon().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn weapon_components_read_per_stack_values() {
        let stack = with(
            Item::Stick,
            KINETIC_WEAPON,
            Compound::new().with("damage_multiplier", 3.0f32).with(
                "damage_conditions",
                Tag::Compound(Compound::new().with("max_duration_ticks", 40)),
            ),
        );
        let kinetic = stack.kinetic_weapon().unwrap();
        assert_eq!(kinetic.damage_multiplier, 3.0);
        assert_eq!(kinetic.contact_cooldown_ticks, 10);
        assert_eq!(kinetic.damage_conditions.unwrap().max_duration_ticks, 40);
        assert!(stack.piercing_weapon().is_none());

        let stack = with(
            Item::IronSpear,
            PIERCING_WEAPON,
            Compound::new().with("deals_knockback", false),
        );
        let piercing = stack.piercing_weapon().unwrap();
        assert!(!piercing.deals_knockback && !piercing.dismounts);
        assert_eq!(
            ItemStack::new(Item::IronSpear, 1).kinetic_weapon(),
            Item::IronSpear.kinetic_weapon_data().copied()
        );
    }
}
