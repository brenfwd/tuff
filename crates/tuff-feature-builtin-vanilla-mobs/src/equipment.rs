//! What a mob wears and holds, the chance each piece drops when it dies,
//! and how both are saved (`equipment` and `drop_chances`, keyed by slot).

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::EquipmentSlot;

use tuff_core::protocol::play::equipment_slot;
use tuff_core::world::entity::take;
use tuff_item::item::{ItemStack, ItemView};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_common::combat::Equipped;

/// The slots a mob's equipment is saved and sent in, with their saved
/// names and protocol ids. `Body` (horse armor, wolf armor) and `Saddle`
/// (a mount's saddle) are here too: nothing about them is mob-specific.
const SLOTS: [(EquipmentSlot, &str, u8); 8] = [
    (
        EquipmentSlot::Mainhand,
        "mainhand",
        equipment_slot::MAIN_HAND,
    ),
    (EquipmentSlot::Offhand, "offhand", equipment_slot::OFF_HAND),
    (EquipmentSlot::Feet, "feet", equipment_slot::FEET),
    (EquipmentSlot::Legs, "legs", equipment_slot::LEGS),
    (EquipmentSlot::Chest, "chest", equipment_slot::CHEST),
    (EquipmentSlot::Head, "head", equipment_slot::HEAD),
    (EquipmentSlot::Body, "body", equipment_slot::BODY),
    (EquipmentSlot::Saddle, "saddle", equipment_slot::SADDLE),
];

/// The chance a piece drops unless set otherwise.
pub const DEFAULT_DROP_CHANCE: f32 = 0.085;
/// A drop chance above 1 marks a piece that always drops undamaged (one
/// the mob picked up).
pub const PRESERVED_DROP_CHANCE: f32 = 2.0;

#[derive(Debug)]
pub struct Equipment {
    items: [Option<ItemStack>; 8],
    drop_chances: [f32; 8],
    changed: bool,
}

impl Default for Equipment {
    fn default() -> Self {
        Self {
            items: Default::default(),
            drop_chances: [DEFAULT_DROP_CHANCE; 8],
            changed: false,
        }
    }
}

const _: () = assert!(
    matches!(SLOTS[6].0, EquipmentSlot::Body) && matches!(SLOTS[7].0, EquipmentSlot::Saddle)
);

fn index(slot: EquipmentSlot) -> Option<usize> {
    SLOTS.iter().position(|(s, ..)| *s == slot)
}

pub(crate) fn prevents_drop(stack: &ItemStack) -> bool {
    stack.has_enchantment_effect(|effects| effects.prevent_equipment_drop)
}

impl Equipment {
    pub fn is_empty(&self) -> bool {
        self.items.iter().all(Option::is_none)
    }

    pub fn get(&self, slot: EquipmentSlot) -> Option<&ItemStack> {
        self.items[index(slot)?].as_ref()
    }

    /// Every slot's item, by slot number (`EquipmentSlot as usize`).
    pub fn by_number(&self) -> Vec<Option<&ItemStack>> {
        let len = SLOTS
            .iter()
            .map(|(s, ..)| *s as usize)
            .max()
            .map_or(0, |m| m + 1);
        let mut out: Vec<Option<&ItemStack>> = vec![None; len];
        for (slot, ..) in &SLOTS {
            out[*slot as usize] = self.get(*slot);
        }
        out
    }

    /// Puts `stack` in the slot with this number and hands back what it
    /// held; `Err` gives `stack` back if there is no such slot.
    pub fn replace_number(
        &mut self,
        number: usize,
        stack: Option<ItemStack>,
    ) -> Result<Option<ItemStack>, Option<ItemStack>> {
        let Some((slot, ..)) = SLOTS.iter().find(|(s, ..)| *s as usize == number) else {
            return Err(stack);
        };
        Ok(self.replace(*slot, stack))
    }

    /// Puts `stack` in `slot` and hands back what it held (or `stack`
    /// itself for a slot mobs do not have).
    #[must_use]
    pub fn replace(&mut self, slot: EquipmentSlot, stack: Option<ItemStack>) -> Option<ItemStack> {
        let Some(i) = index(slot) else {
            return stack;
        };
        self.changed = true;
        std::mem::replace(&mut self.items[i], stack)
    }

    /// The item in `slot` to change in place; marks the equipment changed.
    pub fn get_mut(&mut self, slot: EquipmentSlot) -> Option<&mut ItemStack> {
        let item = self.items[index(slot)?].as_mut()?;
        self.changed = true;
        Some(item)
    }

    /// The saddle and body slots, in the order a mount's screen shows them.
    pub fn mount_slots(&self) -> [&Option<ItemStack>; 2] {
        let [.., body, saddle] = &self.items;
        [saddle, body]
    }

    /// [`Self::mount_slots`] to change; marks the equipment changed.
    pub fn mount_slots_mut(&mut self) -> [&mut Option<ItemStack>; 2] {
        self.changed = true;
        let [.., body, saddle] = &mut self.items;
        [saddle, body]
    }

    /// Takes every item out.
    pub fn take_all(&mut self) -> Vec<ItemStack> {
        let taken: Vec<_> = self.items.iter_mut().filter_map(Option::take).collect();
        self.changed |= !taken.is_empty();
        taken
    }

    /// Takes up to `n` items out of `slot`, emptying it when none are left.
    pub fn take_some(&mut self, slot: EquipmentSlot, n: u8) -> Option<ItemStack> {
        let taken = ItemStack::take_from(&mut self.items[index(slot)?], n);
        self.changed |= taken.is_some();
        taken
    }

    /// Takes the item out of `slot`.
    pub fn take(&mut self, slot: EquipmentSlot) -> Option<ItemStack> {
        let i = index(slot)?;
        let taken = self.items[i].take();
        self.changed |= taken.is_some();
        taken
    }

    /// Puts `stack` in `slot`, burning what it replaces (or `stack` when
    /// the slot already holds the same).
    pub fn set(&mut self, slot: EquipmentSlot, stack: Option<ItemStack>) {
        let discarded = match index(slot) {
            Some(i) if self.items[i] != stack => {
                self.changed = true;
                std::mem::replace(&mut self.items[i], stack)
            }
            _ => stack,
        };
        if let Some(discarded) = discarded {
            discarded.burn(tuff_item::item::BurnReason::Discarded);
        }
    }

    pub fn drop_chance(&self, slot: EquipmentSlot) -> f32 {
        index(slot).map_or(0.0, |i| self.drop_chances[i])
    }

    pub fn set_drop_chance(&mut self, slot: EquipmentSlot, chance: f32) {
        if let Some(i) = index(slot) {
            self.drop_chances[i] = chance;
        }
    }

    pub fn main_hand(&self) -> Option<&ItemStack> {
        self.get(EquipmentSlot::Mainhand)
    }

    /// Every piece with its slot, for attribute modifiers and armor.
    pub fn worn(&self) -> Vec<Equipped<'_>> {
        SLOTS
            .iter()
            .zip(&self.items)
            .filter_map(|((slot, ..), stack)| Some((*slot, stack.as_ref()?)))
            .collect()
    }

    /// Every slot by protocol id, empty ones included.
    pub fn visible(&self) -> Vec<(u8, Option<ItemView<'_>>)> {
        SLOTS
            .iter()
            .zip(&self.items)
            .map(|((_, _, id), stack)| (*id, stack.as_ref().map(ItemStack::view)))
            .collect()
    }

    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// One to three experience per piece that does not always drop.
    pub fn experience_bonus(&self, rng: &mut StdRng) -> i32 {
        self.items
            .iter()
            .zip(&self.drop_chances)
            .filter(|(stack, chance)| stack.is_some() && **chance <= 1.0)
            .map(|_| 1 + rng.gen_range(0..3))
            .sum()
    }

    /// What drops when it dies: each piece by its chance (only for a
    /// player's kill unless preserved), worn pieces randomly damaged.
    pub fn death_drops(
        &mut self,
        killed_by_player: bool,
        rng: &mut StdRng,
        mut boost: impl FnMut(f32, &mut StdRng) -> f32,
    ) -> Vec<ItemStack> {
        let mut drops = Vec::new();
        for (stack, chance) in self.items.iter_mut().zip(self.drop_chances) {
            let Some(item) = stack else {
                continue;
            };
            let preserve = chance > 1.0;
            if prevents_drop(item) || !(killed_by_player || preserve) {
                continue;
            }
            let chance = if preserve { chance } else { boost(chance, rng) };
            if rng.r#gen::<f32>() >= chance {
                continue;
            }
            let mut item = stack.take().expect("checked above");
            if !preserve && let Some(max) = item.max_damage().filter(|_| item.is_damageable()) {
                let max = max as i32;
                let bound = (max - 3).max(1);
                let spread = 1 + rng.gen_range(0..bound);
                let damage = max - rng.gen_range(0..spread);
                item.set_damage(damage.max(0) as u32);
            }
            drops.push(item);
            self.changed = true;
        }
        drops
    }

    pub fn load(&mut self, nbt: &mut Compound) {
        if let Some(saved) = take(nbt, "equipment", |tag| tag.as_compound().cloned()) {
            let mut kept = Compound::new();
            for (key, value) in saved.iter() {
                let slot = SLOTS.iter().position(|(_, name, _)| *name == key);
                let stack = value.as_compound().and_then(ItemStack::from_storage);
                match (slot, stack) {
                    (Some(i), Some(stack)) => self.items[i] = Some(stack),
                    _ => {
                        kept.insert(key, value.clone());
                    }
                }
            }
            if !kept.is_empty() {
                nbt.insert("equipment", kept);
            }
        }
        if let Some(saved) = take(nbt, "drop_chances", |tag| tag.as_compound().cloned()) {
            let mut kept = Compound::new();
            for (key, value) in saved.iter() {
                let slot = SLOTS.iter().position(|(_, name, _)| *name == key);
                match (slot, value) {
                    (Some(i), Tag::Float(chance)) => self.drop_chances[i] = *chance,
                    _ => {
                        kept.insert(key, value.clone());
                    }
                }
            }
            if !kept.is_empty() {
                nbt.insert("drop_chances", kept);
            }
        }
        self.changed = false;
    }

    /// Writes the pieces and the chances that differ from the default,
    /// merging into what `load` kept of slots this server does not model.
    pub fn save(&self, out: &mut Compound) {
        let mut equipment = Compound::new();
        let mut chances = Compound::new();
        for (((_, name, _), stack), chance) in SLOTS.iter().zip(&self.items).zip(self.drop_chances)
        {
            if let Some(stack) = stack {
                equipment.insert(*name, stack.to_nbt());
            }
            if chance != DEFAULT_DROP_CHANCE {
                chances.insert(*name, chance);
            }
        }
        if !equipment.is_empty() {
            out.insert("equipment", equipment);
        }
        if !chances.is_empty() {
            out.insert("drop_chances", chances);
        }
    }
}

/// A dispenser puts one of `stack` on a mob that picks up loot, into the
/// empty slot the item belongs in.
pub fn dispense_equip(
    core: &mut super::MobCore,
    stack: &ItemStack,
    cx: &mut tuff_core::world::entity::TickContext,
) -> Option<tuff_core::world::region_api::DispenseOutcome> {
    let equippable = stack.equippable()?;
    if !equippable.dispensable
        || stack.restricted_for(core.base.kind)
        || !core.can_pick_up_loot
        || core.equipment.get(equippable.slot).is_some()
        || index(equippable.slot).is_none()
    {
        return None;
    }
    core.equipment.set(
        equippable.slot,
        Some(ItemStack::mint_like(
            stack,
            1,
            tuff_item::item::MintReason::Transform,
        )),
    );
    core.equipment
        .set_drop_chance(equippable.slot, PRESERVED_DROP_CHANCE);
    core.persistence_required = true;
    core.sound(equippable.equip_sound, 1.0, 1.0, cx);
    Some(tuff_core::world::region_api::DispenseOutcome::Dispensed)
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn slots_are_read_and_set_by_number() {
        let mut equipment = Equipment::default();
        let head = EquipmentSlot::Head as usize;
        let bow = ItemStack::mint(Item::Bow, 1, MintReason::Test);
        assert!(matches!(
            equipment.replace_number(head, Some(bow)),
            Ok(None)
        ));
        assert!(equipment.replace_number(99, None).is_err());
        assert_eq!(equipment.by_number()[head].map(|s| s.item), Some(Item::Bow));
        assert!(equipment.take_changed());
    }

    #[tuff_core::watchdog]
    #[test]
    fn equipment_round_trips_and_keeps_unmodeled_slots() {
        let mut nbt = Compound::new()
            .with(
                "equipment",
                Compound::new()
                    .with(
                        "mainhand",
                        ItemStack::mint(Item::Bow, 1, MintReason::Test).to_nbt(),
                    )
                    .with(
                        "saddle",
                        ItemStack::mint(Item::Saddle, 1, MintReason::Test).to_nbt(),
                    )
                    .with(
                        "reins",
                        ItemStack::mint(Item::Stick, 1, MintReason::Test).to_nbt(),
                    ),
            )
            .with(
                "drop_chances",
                Compound::new()
                    .with("mainhand", 2.0f32)
                    .with("body", 0.5f32),
            );
        let mut equipment = Equipment::default();
        equipment.load(&mut nbt);
        assert_eq!(equipment.main_hand().map(|s| s.item), Some(Item::Bow));
        assert_eq!(
            equipment.get(EquipmentSlot::Saddle).map(|s| s.item),
            Some(Item::Saddle)
        );
        assert!(nbt.get("equipment").is_some(), "the unknown slot stays raw");
        let mut out = Compound::new();
        equipment.save(&mut out);
        let saved = out.get("equipment").and_then(Tag::as_compound).unwrap();
        assert!(saved.get("mainhand").is_some());
        assert!(saved.get("saddle").is_some());
        let chances = out.get("drop_chances").and_then(Tag::as_compound).unwrap();
        assert_eq!(chances.get("mainhand"), Some(&Tag::Float(2.0)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_boost_makes_unlikely_pieces_drop_for_a_player_kill() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut equipment = Equipment::default();
        equipment.set(
            EquipmentSlot::Head,
            Some(ItemStack::mint(Item::GoldIngot, 1, MintReason::Test)),
        );
        equipment.set_drop_chance(EquipmentSlot::Head, 0.0);
        assert!(equipment.death_drops(true, &mut rng, |c, _| c).is_empty());
        assert_eq!(
            equipment.death_drops(true, &mut rng, |c, _| c + 2.0).len(),
            1
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_preserved_pieces_drop_without_a_player() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut equipment = Equipment::default();
        equipment.set(
            EquipmentSlot::Head,
            Some(ItemStack::mint(Item::IronHelmet, 1, MintReason::Test)),
        );
        equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::mint(Item::GoldIngot, 1, MintReason::Test)),
        );
        equipment.set_drop_chance(EquipmentSlot::Mainhand, PRESERVED_DROP_CHANCE);
        let drops = equipment.death_drops(false, &mut rng, |chance, _| chance);
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].item, Item::GoldIngot);
        assert!(equipment.get(EquipmentSlot::Head).is_some());
    }
}
