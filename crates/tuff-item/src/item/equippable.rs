use super::*;
use tuff_vanilla_data::item::{EquipmentSlot, Equippable};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

const KEY: &str = "minecraft:equippable";

fn slot(name: &str) -> Option<EquipmentSlot> {
    Some(match name {
        "mainhand" => EquipmentSlot::Mainhand,
        "offhand" => EquipmentSlot::Offhand,
        "feet" => EquipmentSlot::Feet,
        "legs" => EquipmentSlot::Legs,
        "chest" => EquipmentSlot::Chest,
        "head" => EquipmentSlot::Head,
        "body" => EquipmentSlot::Body,
        "saddle" => EquipmentSlot::Saddle,
        _ => return None,
    })
}

fn flag(fields: &Compound, name: &str, default: bool) -> bool {
    fields.get(name).and_then(Tag::as_bool).unwrap_or(default)
}

fn parse(fields: &Compound) -> Option<Equippable> {
    let sound = |name: &str, default: SoundEvent| match fields.get(name) {
        Some(tag) => tag.as_str().and_then(SoundEvent::from_name),
        None => Some(default),
    };
    Some(Equippable {
        slot: slot(fields.get("slot")?.as_str()?)?,
        equip_sound: sound("equip_sound", SoundEvent::ItemArmorEquipGeneric)?,
        asset_id: None,
        swappable: flag(fields, "swappable", true),
        damage_on_hurt: flag(fields, "damage_on_hurt", true),
        equip_on_interact: flag(fields, "equip_on_interact", false),
        dispensable: flag(fields, "dispensable", true),
        restricted: fields.contains_key("allowed_entities"),
        can_be_sheared: flag(fields, "can_be_sheared", false),
        shearing_sound: sound("shearing_sound", SoundEvent::ItemShearsSnip)?,
    })
}

impl<M: super::Mode> ItemStack<M> {
    /// What this stack can be worn as: its own `equippable` component when
    /// it carries one, else its item's, none when the component is removed.
    pub fn equippable(&self) -> Option<Equippable> {
        if self.components.is_removed(KEY) {
            return None;
        }
        match self.components.other.get(KEY) {
            Some(tag) => tag.as_compound().and_then(parse),
            None => self.item.equippable().copied(),
        }
    }

    /// Whether a dispenser may not put this on an entity of `kind`.
    pub fn restricted_for(&self, kind: EntityType) -> bool {
        match self.equippable() {
            Some(equippable) if equippable.restricted => {
                !self.components.other.contains_key(KEY) || !self.allows_wearer(kind)
            }
            _ => false,
        }
    }

    /// Whether an entity of `kind` may wear this stack; a stack with its
    /// own `allowed_entities` list names the types, and a stack without
    /// one fits any entity.
    pub fn allows_wearer(&self, kind: EntityType) -> bool {
        let Some(allowed) = self
            .components
            .other
            .get(KEY)
            .and_then(Tag::as_compound)
            .and_then(|fields| fields.get("allowed_entities"))
        else {
            return true;
        };
        let matches = |tag: &Tag| tag.as_str().is_some_and(|name| name == kind.name());
        match allowed.as_list() {
            Some(list) => list.iter().any(matches),
            None => matches(allowed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack_with(equippable: Compound) -> ItemStack {
        let mut stack = ItemStack::mint(Item::Stick, 1, MintReason::Test);
        stack.components.other.insert(KEY, equippable);
        stack
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stack_without_the_component_uses_its_items() {
        let helmet = ItemStack::mint(Item::IronHelmet, 1, MintReason::Test);
        assert_eq!(
            helmet.equippable().map(|e| e.slot),
            Some(EquipmentSlot::Head)
        );
        assert!(
            ItemStack::mint(Item::Stick, 1, MintReason::Test)
                .equippable()
                .is_none()
        );
        assert!(helmet.allows_wearer(EntityType::Zombie));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stack_component_overrides_the_item_and_defaults_its_flags() {
        let stack = stack_with(Compound::new().with("slot", "head"));
        let eq = stack.equippable().unwrap();
        assert_eq!(eq.slot, EquipmentSlot::Head);
        assert!(eq.swappable && eq.dispensable && eq.damage_on_hurt);
        assert!(!eq.restricted && !eq.equip_on_interact && !eq.can_be_sheared);
        let off = stack_with(
            Compound::new()
                .with("slot", "chest")
                .with("dispensable", false)
                .with("swappable", false),
        );
        let eq = off.equippable().unwrap();
        assert!(!eq.dispensable && !eq.swappable);
    }

    #[tuff_core::watchdog]
    #[test]
    fn allowed_entities_restrict_the_wearers() {
        let stack = stack_with(
            Compound::new()
                .with("slot", "head")
                .with("allowed_entities", "minecraft:zombie"),
        );
        assert!(stack.equippable().unwrap().restricted);
        assert!(stack.allows_wearer(EntityType::Zombie));
        assert!(!stack.allows_wearer(EntityType::Skeleton));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_malformed_component_yields_nothing_and_a_removed_one_hides_the_item_default() {
        assert!(
            stack_with(Compound::new().with("slot", "elbow"))
                .equippable()
                .is_none()
        );
        let mut helmet = ItemStack::mint(Item::IronHelmet, 1, MintReason::Test);
        helmet.components.removed.push(KEY.to_owned());
        assert!(helmet.equippable().is_none());
    }
}
