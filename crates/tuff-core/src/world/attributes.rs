//! Attribute values from equipment, enchantments and effects, and the
//! enchantment data helpers they share with the combat rules.

use std::collections::HashMap;

use serde_json::Value;
use tuff_vanilla_data::item::{EquipmentSlot, EquipmentSlotGroup, ModifierOperation};
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect};

use tuff_item::item::ItemStack;

const MAX_KNOCKBACK_Y: f64 = 0.4;

/// A modifier added by `/attribute`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandModifier {
    pub attribute: Attribute,
    pub id: String,
    pub amount: f64,
    pub operation: ModifierOperation,
}

/// The bases and modifiers a player carries on top of its gear and
/// effects; saved with the player.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttributeOverrides {
    bases: Vec<(Attribute, f64)>,
    modifiers: Vec<CommandModifier>,
}

impl AttributeOverrides {
    pub fn is_empty(&self) -> bool {
        self.bases.is_empty() && self.modifiers.is_empty()
    }

    pub fn set_base(&mut self, attribute: Attribute, value: f64) {
        self.bases.retain(|(a, _)| *a != attribute);
        self.bases.push((attribute, value));
    }

    pub fn base(&self, attribute: Attribute) -> f64 {
        self.bases
            .iter()
            .find(|(a, _)| *a == attribute)
            .map(|(_, v)| *v)
            .or_else(|| EntityType::Player.base_attribute(attribute))
            .unwrap_or_else(|| attribute.range().default)
    }

    pub fn extras(&self, attribute: Attribute) -> Vec<(&str, f64, ModifierOperation)> {
        self.modifiers
            .iter()
            .filter(|m| m.attribute == attribute)
            .map(|m| (m.id.as_str(), m.amount, m.operation))
            .collect()
    }

    /// Runs `command`, reading full values through `value`.
    pub fn apply(
        &mut self,
        command: &crate::world::messages::AttributeCommand,
        value: impl Fn(&Self, Attribute) -> f64,
    ) -> crate::world::messages::AttributeAnswer {
        use crate::world::messages::{AttributeAnswer, AttributeOp};
        let attribute = command.attribute;
        if EntityType::Player.base_attribute(attribute).is_none() {
            return AttributeAnswer::NoAttribute;
        }
        let has = |this: &Self, id: &str| {
            this.modifiers
                .iter()
                .any(|m| m.attribute == attribute && m.id == id)
        };
        match &command.op {
            AttributeOp::Value => AttributeAnswer::Number(value(self, attribute)),
            AttributeOp::Base => AttributeAnswer::Number(self.base(attribute)),
            AttributeOp::SetBase(base) => {
                self.bases.retain(|(a, _)| *a != attribute);
                self.bases.push((attribute, *base));
                AttributeAnswer::Done
            }
            AttributeOp::ResetBase => {
                self.bases.retain(|(a, _)| *a != attribute);
                AttributeAnswer::Number(self.base(attribute))
            }
            AttributeOp::AddModifier {
                id,
                amount,
                operation,
            } => {
                if has(self, id) {
                    return AttributeAnswer::DuplicateModifier;
                }
                self.modifiers.push(CommandModifier {
                    attribute,
                    id: id.clone(),
                    amount: *amount,
                    operation: *operation,
                });
                AttributeAnswer::Done
            }
            AttributeOp::RemoveModifier(id) => {
                if !has(self, id) {
                    return AttributeAnswer::NoModifier;
                }
                self.modifiers
                    .retain(|m| !(m.attribute == attribute && m.id == *id));
                AttributeAnswer::Done
            }
            AttributeOp::ModifierValue(id) => self
                .modifiers
                .iter()
                .find(|m| m.attribute == attribute && m.id == *id)
                .map_or(AttributeAnswer::NoModifier, |m| {
                    AttributeAnswer::Number(m.amount)
                }),
        }
    }

    pub fn to_nbt(&self) -> Vec<tuff_nbt::Tag> {
        use tuff_nbt::{Compound, Tag};
        let mut attributes: Vec<Attribute> = self.bases.iter().map(|(a, _)| *a).collect();
        for modifier in &self.modifiers {
            if !attributes.contains(&modifier.attribute) {
                attributes.push(modifier.attribute);
            }
        }
        attributes
            .into_iter()
            .map(|attribute| {
                let mut entry = Compound::new().with("id", attribute.name());
                if let Some((_, base)) = self.bases.iter().find(|(a, _)| *a == attribute) {
                    entry.insert("base", *base);
                } else {
                    entry.insert("base", self.base(attribute));
                }
                let modifiers: Vec<Tag> = self
                    .modifiers
                    .iter()
                    .filter(|m| m.attribute == attribute)
                    .map(|m| {
                        Tag::Compound(
                            Compound::new()
                                .with("id", m.id.as_str())
                                .with("amount", m.amount)
                                .with("operation", m.operation.name()),
                        )
                    })
                    .collect();
                if !modifiers.is_empty() {
                    entry.insert("modifiers", Tag::List(modifiers));
                }
                Tag::Compound(entry)
            })
            .collect()
    }

    pub fn from_nbt(list: &[tuff_nbt::Tag]) -> Self {
        use tuff_nbt::Tag;
        let number = |tag: &Tag| match *tag {
            Tag::Double(v) => Some(v),
            Tag::Float(v) => Some(v.into()),
            _ => None,
        };
        let mut out = Self::default();
        for compound in list.iter().filter_map(Tag::as_compound) {
            let Some(attribute) = compound.get_str("id").and_then(Attribute::from_name) else {
                continue;
            };
            if let Some(base) = compound.get("base").and_then(number)
                && EntityType::Player.base_attribute(attribute) != Some(base)
            {
                out.bases.push((attribute, base));
            }
            let Some(Tag::List(modifiers)) = compound.get("modifiers") else {
                continue;
            };
            for modifier in modifiers.iter().filter_map(Tag::as_compound) {
                let (Some(id), Some(amount), Some(operation)) = (
                    modifier.get_str("id"),
                    modifier.get("amount").and_then(number),
                    modifier
                        .get_str("operation")
                        .and_then(ModifierOperation::from_name),
                ) else {
                    continue;
                };
                out.modifiers.push(CommandModifier {
                    attribute,
                    id: id.to_owned(),
                    amount,
                    operation,
                });
            }
        }
        out
    }
}

/// A worn or held item and the slot it is in.
pub type Equipped<'a> = (EquipmentSlot, &'a ItemStack);

/// What changes a player's attribute values.
#[derive(Clone, Copy)]
pub struct Modifiers<'a> {
    pub equipment: &'a [Equipped<'a>],
    /// Active effects and their amplifiers.
    pub effects: &'a [(MobEffect, u8)],
}

impl<'a> Modifiers<'a> {
    pub fn equipment(equipment: &'a [Equipped<'a>]) -> Self {
        Self {
            equipment,
            effects: &[],
        }
    }

    /// The value of a player's `attribute`: its base, then every modifier
    /// by id (a later one replaces an earlier one with the same id), in
    /// the game's operation order, clamped to the attribute's range.
    pub fn value(&self, attribute: Attribute) -> f64 {
        let base = EntityType::Player
            .base_attribute(attribute)
            .unwrap_or(attribute.range().default);
        self.value_from(attribute, base, &[])
    }

    /// The value of `attribute` from `base`, with `extra` modifiers (id,
    /// amount, operation) applied before the equipment's and effects'.
    pub fn value_from(
        &self,
        attribute: Attribute,
        base: f64,
        extra: &[(&str, f64, ModifierOperation)],
    ) -> f64 {
        let mut by_id: HashMap<String, (f64, ModifierOperation)> = extra
            .iter()
            .map(|(id, amount, op)| (id.to_string(), (*amount, *op)))
            .collect();
        let registry = tuff_item::enchantment::current();
        for (slot, stack) in self.equipment {
            for modifier in stack.attribute_modifiers().iter() {
                if modifier.attribute == attribute && modifier.slot.contains(*slot) {
                    by_id.insert(
                        modifier.id.to_string(),
                        (modifier.amount, modifier.operation),
                    );
                }
            }
            for (_, def, level) in
                tuff_item::enchantment::leveled(&registry, stack.components.enchantments.as_ref())
            {
                if !applies_in(def, *slot) {
                    continue;
                }
                for effect in &def.effects.attributes {
                    if let Some((id, amount, operation)) =
                        attribute_effect(effect, attribute, level, *slot)
                    {
                        by_id.insert(id, (amount, operation));
                    }
                }
            }
        }
        for (effect, amplifier) in self.effects {
            for modifier in effect.attribute_modifiers() {
                if modifier.attribute == attribute {
                    by_id.insert(
                        modifier.id.to_string(),
                        (modifier.amount_at(*amplifier), modifier.operation),
                    );
                }
            }
        }
        compute(attribute, base, by_id.values().copied())
    }
}

/// An attribute's value from its base and modifiers.
fn compute(
    attribute: Attribute,
    base: f64,
    modifiers: impl Iterator<Item = (f64, ModifierOperation)> + Clone,
) -> f64 {
    let sum = |operation| {
        modifiers
            .clone()
            .filter(move |(_, op)| *op == operation)
            .map(|(amount, _)| amount)
    };
    let base = base + sum(ModifierOperation::AddValue).sum::<f64>();
    let mut result = base;
    for amount in sum(ModifierOperation::AddMultipliedBase) {
        result += base * amount;
    }
    for amount in sum(ModifierOperation::AddMultipliedTotal) {
        result *= 1.0 + amount;
    }
    attribute.sanitize(result)
}

/// An enchantment's `attributes` effect entry for `attribute`, as the
/// modifier it adds in `slot`.
fn attribute_effect(
    effect: &tuff_item::enchantment::AttributeEffect,
    attribute: Attribute,
    level: u32,
    slot: EquipmentSlot,
) -> Option<(String, f64, ModifierOperation)> {
    if Attribute::from_name(&effect.attribute) != Some(attribute) {
        return None;
    }
    let operation = modifier_operation(effect.operation);
    let id = format!("{}/{}", effect.id, slot_name(slot));
    Some((id, effect.amount.at(level) as f64, operation))
}

fn slot_name(slot: EquipmentSlot) -> &'static str {
    match slot {
        EquipmentSlot::Mainhand => "mainhand",
        EquipmentSlot::Offhand => "offhand",
        EquipmentSlot::Feet => "feet",
        EquipmentSlot::Legs => "legs",
        EquipmentSlot::Chest => "chest",
        EquipmentSlot::Head => "head",
        EquipmentSlot::Body => "body",
        EquipmentSlot::Saddle => "saddle",
    }
}

/// Attributes the network protocol never syncs to clients (they only
/// matter server-side: melee damage/knockback and mob-only attributes a
/// player never has).
pub const NOT_CLIENT_SYNCED: &[Attribute] = &[
    Attribute::AttackDamage,
    Attribute::AttackKnockback,
    Attribute::FollowRange,
    Attribute::KnockbackResistance,
    Attribute::SpawnReinforcements,
    Attribute::TemptRange,
    Attribute::WaypointTransmitRange,
    Attribute::WaypointReceiveRange,
];

/// Every attribute a player has that the client needs synced to render
/// itself correctly (armor icons, health, movement, mining speed, ...).
pub fn synced_attributes() -> impl Iterator<Item = Attribute> {
    Attribute::ALL.into_iter().filter(|attribute| {
        EntityType::Player.base_attribute(*attribute).is_some()
            && !NOT_CLIENT_SYNCED.contains(attribute)
    })
}

/// Pushes `motion` away from the direction `(dir_x, dir_z)`, which points
/// from the pushed entity towards what pushes it: halves the current
/// motion, and lifts it if it stands on the ground. `resistance` is its
/// knockback resistance.
pub fn knockback(
    motion: [f64; 3],
    strength: f64,
    dir_x: f64,
    dir_z: f64,
    on_ground: bool,
    resistance: f64,
) -> [f64; 3] {
    let strength = strength * (1.0 - resistance);
    let len = (dir_x * dir_x + dir_z * dir_z).sqrt();
    if strength <= 0.0 || len < 1.0e-5 {
        return motion;
    }
    let (nx, nz) = (dir_x / len * strength, dir_z / len * strength);
    let y = if on_ground {
        (motion[1] / 2.0 + strength).min(MAX_KNOCKBACK_Y)
    } else {
        motion[1]
    };
    [motion[0] / 2.0 - nx, y, motion[2] / 2.0 - nz]
}

pub fn applies_in(
    enchantment: &tuff_item::enchantment::EnchantmentDef,
    slot: EquipmentSlot,
) -> bool {
    enchantment
        .slots
        .iter()
        .any(|group| EquipmentSlotGroup::from_name(group).is_some_and(|group| group.contains(slot)))
}

/// A level-based value, computed in single precision as the game does.
pub fn level_based(value: &Value, level: i32) -> Option<f32> {
    if let Some(constant) = value.as_f64() {
        return Some(constant as f32);
    }
    let field = |name: &str| value[name].as_f64().map(|v| v as f32);
    match value["type"].as_str()? {
        "minecraft:linear" => {
            Some(field("base")? + field("per_level_above_first")? * (level - 1) as f32)
        }
        "minecraft:levels_squared" => Some((level * level) as f32 + field("added")?),
        "minecraft:clamped" => {
            let inner = level_based(&value["value"], level)?;
            Some(inner.clamp(field("min")?, field("max")?))
        }
        "minecraft:fraction" => {
            let denominator = level_based(&value["denominator"], level)?;
            if denominator == 0.0 {
                return Some(0.0);
            }
            Some(level_based(&value["numerator"], level)? / denominator)
        }
        "minecraft:exponent" => {
            let base = level_based(&value["base"], level)?;
            let power = level_based(&value["power"], level)?;
            Some((base as f64).powf(power as f64) as f32)
        }
        "minecraft:lookup" => {
            let values = value["values"].as_array()?;
            match usize::try_from(level - 1).ok().and_then(|i| values.get(i)) {
                Some(entry) => entry.as_f64().map(|v| v as f32),
                None => level_based(&value["fallback"], level),
            }
        }
        _ => None,
    }
}

/// The attribute modifier operation an enchantment's attribute effect uses.
pub fn modifier_operation(
    operation: tuff_item::enchantment::AttributeOperation,
) -> ModifierOperation {
    match operation {
        tuff_item::enchantment::AttributeOperation::AddValue => ModifierOperation::AddValue,
        tuff_item::enchantment::AttributeOperation::AddMultipliedBase => {
            ModifierOperation::AddMultipliedBase
        }
        tuff_item::enchantment::AttributeOperation::AddMultipliedTotal => {
            ModifierOperation::AddMultipliedTotal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::messages::{AttributeAnswer, AttributeCommand, AttributeOp};

    #[tuff_core::watchdog]
    #[test]
    fn overrides_round_trip_through_nbt() {
        let mut overrides = AttributeOverrides::default();
        let run = |overrides: &mut AttributeOverrides, op| {
            overrides.apply(
                &AttributeCommand {
                    attribute: Attribute::MovementSpeed,
                    op,
                },
                |_, _| 0.0,
            )
        };
        assert_eq!(
            run(&mut overrides, AttributeOp::SetBase(0.3)),
            AttributeAnswer::Done
        );
        run(
            &mut overrides,
            AttributeOp::AddModifier {
                id: "test:m".into(),
                amount: 0.25,
                operation: ModifierOperation::AddMultipliedBase,
            },
        );
        let loaded = AttributeOverrides::from_nbt(&overrides.to_nbt());
        assert_eq!(loaded, overrides);
        assert_eq!(
            run(&mut overrides, AttributeOp::ModifierValue("test:m".into())),
            AttributeAnswer::Number(0.25)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn head_items_hide_the_player_from_the_locator_bar() {
        use tuff_vanilla_data::Item;
        use tuff_vanilla_data::item::EquipmentSlot;
        for item in [Item::CarvedPumpkin, Item::ZombieHead, Item::DragonHead] {
            let stack = ItemStack::new(item, 1);
            let worn = [(EquipmentSlot::Head, &stack)];
            let hidden = Modifiers::equipment(&worn).value(Attribute::WaypointTransmitRange);
            assert_eq!(hidden, 0.0, "{item:?}");
            let carried = [(EquipmentSlot::Mainhand, &stack)];
            assert!(Modifiers::equipment(&carried).value(Attribute::WaypointTransmitRange) > 0.0);
        }
    }
}
