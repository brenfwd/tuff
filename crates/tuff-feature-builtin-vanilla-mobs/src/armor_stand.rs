//! Armor stands: living entities that wear and hold what players give
//! them. They are not mobs (no AI, nothing pushes them), only a few damage
//! types hurt them, and a second player hit within 5 ticks breaks one.

use crate::{Damage, MobCore, MobSounds, Species, data};
use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::entity::Size;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use tuff_core::protocol::play::{self, MetadataValue, metadata};
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, TickContext, number, take, take_bool, take_number,
};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_core::world::messages::HandEffect;
use tuff_core::world::region_api::{DispenseOutcome, SlotEdit};
use tuff_core::world::{framed, packets};
use tuff_item::item::{BurnReason, ItemSnapshot, ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub const CLIENT_FLAGS: u8 = 15;
pub const HEAD_POSE: u8 = 16;
const POSE_COUNT: usize = 6;

const FLAG_SMALL: i8 = 1;
const FLAG_SHOW_ARMS: i8 = 4;
const FLAG_NO_BASE_PLATE: i8 = 8;
const FLAG_MARKER: i8 = 16;

const WOBBLE_TICKS: u64 = 5;
const HIT_EVENT: i8 = 32;
const REMOVE_ITEM_BIT: i32 = 8;
const PUT_ITEM_BIT: i32 = 16;

const POSE_KEYS: [&str; POSE_COUNT] =
    ["Head", "Body", "LeftArm", "RightArm", "LeftLeg", "RightLeg"];
const DEFAULT_POSES: [[f32; 3]; POSE_COUNT] = [
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [-10.0, 0.0, -10.0],
    [-15.0, 0.0, 10.0],
    [-1.0, 0.0, -1.0],
    [1.0, 0.0, 1.0],
];

#[derive(Debug)]
pub struct ArmorStand {
    pub small: bool,
    pub show_arms: bool,
    pub no_base_plate: bool,
    pub marker: bool,
    pub invisible: bool,
    pub disabled_slots: i32,
    pub poses: [[f32; 3]; POSE_COUNT],
    last_hit: Option<u64>,
    changed: Vec<u8>,
}

fn legacy_filter_flag(slot: EquipmentSlot) -> i32 {
    match slot {
        EquipmentSlot::Mainhand => 0,
        EquipmentSlot::Feet => 1,
        EquipmentSlot::Legs => 2,
        EquipmentSlot::Chest => 3,
        EquipmentSlot::Head => 4,
        EquipmentSlot::Offhand => 5,
        EquipmentSlot::Body => 6,
        EquipmentSlot::Saddle => 7,
    }
}

fn is_hand(slot: EquipmentSlot) -> bool {
    matches!(slot, EquipmentSlot::Mainhand | EquipmentSlot::Offhand)
}

fn usable(slot: EquipmentSlot) -> bool {
    !matches!(slot, EquipmentSlot::Body | EquipmentSlot::Saddle)
}

fn slot_for(stack: &ItemStack) -> EquipmentSlot {
    stack
        .item()
        .equippable()
        .map(|e| e.slot)
        .filter(|slot| usable(*slot))
        .unwrap_or(EquipmentSlot::Mainhand)
}

impl ArmorStand {
    pub fn size_for(small: bool, marker: bool, standing: Size) -> Size {
        if marker {
            return Size {
                width: 0.0,
                height: 0.0,
                eye_height: 0.0,
                ..standing
            };
        }
        if small {
            return Size {
                width: standing.width * 0.5,
                height: standing.height * 0.5,
                eye_height: standing.eye_height * 0.5,
                ..standing
            };
        }
        standing
    }

    fn flags(&self) -> i8 {
        let mut flags = 0;
        for (set, bit) in [
            (self.small, FLAG_SMALL),
            (self.show_arms, FLAG_SHOW_ARMS),
            (self.no_base_plate, FLAG_NO_BASE_PLATE),
            (self.marker, FLAG_MARKER),
        ] {
            if set {
                flags |= bit;
            }
        }
        flags
    }

    fn is_disabled(&self, slot: EquipmentSlot) -> bool {
        self.disabled_slots & (1 << legacy_filter_flag(slot)) != 0
            || (is_hand(slot) && !self.show_arms)
    }

    fn bit(&self, slot: EquipmentSlot, offset: i32) -> bool {
        self.disabled_slots & (1 << (legacy_filter_flag(slot) + offset)) != 0
    }

    fn data_value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            CLIENT_FLAGS => MetadataValue::Byte(self.flags()),
            i if (HEAD_POSE..HEAD_POSE + POSE_COUNT as u8).contains(&i) => {
                MetadataValue::Rotations(self.poses[usize::from(i - HEAD_POSE)])
            }
            _ => return None,
        })
    }

    /// The slot a click `y` blocks above the stand's feet (in its own
    /// scale) lands on.
    fn clicked_slot(&self, y: f64, core: &MobCore) -> EquipmentSlot {
        let has = |slot| core.equipment.get(slot).is_some();
        let small = self.small;
        if y >= 0.1 && y < 0.1 + if small { 0.8 } else { 0.45 } && has(EquipmentSlot::Feet) {
            EquipmentSlot::Feet
        } else if y >= 0.9 + if small { 0.3 } else { 0.0 }
            && y < 0.9 + if small { 1.0 } else { 0.7 }
            && has(EquipmentSlot::Chest)
        {
            EquipmentSlot::Chest
        } else if y >= 0.4 && y < 0.4 + if small { 1.0 } else { 0.8 } && has(EquipmentSlot::Legs) {
            EquipmentSlot::Legs
        } else if y >= 1.6 && has(EquipmentSlot::Head) {
            EquipmentSlot::Head
        } else if !has(EquipmentSlot::Mainhand) && has(EquipmentSlot::Offhand) {
            EquipmentSlot::Offhand
        } else {
            EquipmentSlot::Mainhand
        }
    }

    fn equip_sound(&self, core: &MobCore, stack: &ItemStack, cx: &mut TickContext) {
        if let Some(equippable) = stack.equippable() {
            core.sound(equippable.equip_sound, 1.0, 1.0, cx);
        }
    }

    fn swap(
        &mut self,
        core: &mut MobCore,
        slot: EquipmentSlot,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let old = core.equipment.get(slot).map(ItemSnapshot::of);
        let outcome = self.swap_slot(core, slot, interaction, cx);
        let new = core.equipment.get(slot);
        let same = match (&old, new) {
            (None, None) => true,
            (Some(old), Some(new)) => old == new,
            _ => false,
        };
        if !same {
            cx.effects.push(Effect::HeldItems {
                player: Some(interaction.player),
                slots: vec![SlotEdit {
                    slot: slot as usize,
                    old,
                    new: new.map(ItemSnapshot::of),
                }],
            });
        }
        outcome
    }

    fn swap_slot(
        &mut self,
        core: &mut MobCore,
        slot: EquipmentSlot,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let current = core.equipment.get(slot).is_some();
        if current && self.bit(slot, REMOVE_ITEM_BIT) {
            return None;
        }
        if !current && self.bit(slot, PUT_ITEM_BIT) {
            return None;
        }
        let held = interaction.stack.as_deref();
        if interaction.creative
            && !current
            && let Some(held) = held
        {
            let _ = held;
            let one = interaction.hand.take(1)?;
            self.equip_sound(core, &one, cx);
            core.equipment.set(slot, Some(one));
            return Some(HandEffect::None);
        }
        if let Some(held) = held
            && held.count() > 1
        {
            if current {
                return None;
            }
            let one = interaction.hand.take(1)?;
            self.equip_sound(core, &one, cx);
            core.equipment.set(slot, Some(one));
            return Some(HandEffect::None);
        }
        let given = match held {
            Some(_) => Some(interaction.hand.take_all()?),
            None => None,
        };
        if let Some(given) = &given {
            self.equip_sound(core, given, cx);
        }
        if let Some(current) = core.equipment.replace(slot, given)
            && let Some(left) = interaction.hand.give_back(current)
        {
            core.drop_item(left, 0.0, cx);
        }
        Some(HandEffect::None)
    }

    fn pop(&self, core: &MobCore, at: BlockPos, stack: ItemStack, cx: &mut TickContext) {
        let _ = core;
        if !cx.shared.rule_bool("block_drops") {
            stack.burn(BurnReason::Destroyed);
            return;
        }
        let spread = |rng: &mut StdRng| rng.gen_range(-0.25..0.25);
        let position = [
            f64::from(at.x) + 0.5 + spread(cx.rng),
            f64::from(at.y) + 0.5 + spread(cx.rng) - 0.125,
            f64::from(at.z) + 0.5 + spread(cx.rng),
        ];
        let velocity = [
            cx.rng.r#gen::<f64>() * 0.2 - 0.1,
            0.2,
            cx.rng.r#gen::<f64>() * 0.2 - 0.1,
        ];
        cx.effects.push(Effect::DropItem {
            stack,
            position,
            velocity,
            pickup_delay: BLOCK_DROP_PICKUP_DELAY,
        });
    }

    fn feet_block(core: &MobCore) -> BlockPos {
        let [x, y, z] = core.base.position;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    }

    fn broken_sound(core: &MobCore, cx: &mut TickContext) {
        core.sound(SoundEvent::EntityArmorStandBreak, 1.0, 1.0, cx);
    }

    fn broken_by_anything(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        Self::broken_sound(core, cx);
        let above = Self::feet_block(core);
        let above = BlockPos::new(above.x, above.y + 1, above.z);
        for slot in [
            EquipmentSlot::Mainhand,
            EquipmentSlot::Offhand,
            EquipmentSlot::Feet,
            EquipmentSlot::Legs,
            EquipmentSlot::Chest,
            EquipmentSlot::Head,
        ] {
            if let Some(stack) = core.equipment.take(slot) {
                if prevents_drop(&stack) {
                    stack.burn(BurnReason::Destroyed);
                } else {
                    self.pop(core, above, stack, cx);
                }
            }
        }
    }

    fn broken_by_player(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let mut item = ItemStack::mint(Item::ArmorStand, 1, MintReason::Spawn);
        if let Some(name) = core
            .base
            .custom_name
            .as_ref()
            .and_then(tuff_item::text::TextComponent::from_nbt)
        {
            item.components.custom_name = Some(Box::new(name));
        }
        let feet = Self::feet_block(core);
        self.pop(core, feet, item, cx);
        self.broken_by_anything(core, cx);
    }

    fn cause_damage(&mut self, core: &mut MobCore, amount: f32, cx: &mut TickContext) {
        let health = core.health - amount;
        if health <= 0.5 {
            self.broken_by_anything(core, cx);
            core.discarded = true;
        } else {
            core.health = health;
            core.changed(data::HEALTH);
        }
    }
}

fn prevents_drop(stack: &ItemStack) -> bool {
    stack.has_enchantment_effect(|effects| effects.prevent_equipment_drop)
}

fn rotations(tag: &Tag) -> Option<[f32; 3]> {
    let list = tag.as_list()?;
    if list.len() != 3 {
        return None;
    }
    let mut out = [0.0; 3];
    for (slot, tag) in out.iter_mut().zip(list) {
        *slot = number(tag)? as f32;
    }
    Some(out)
}

impl Species for ArmorStand {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            small: false,
            show_arms: false,
            no_base_plate: false,
            marker: false,
            invisible: false,
            disabled_slots: 0,
            poses: DEFAULT_POSES,
            last_hit: None,
            changed: Vec::new(),
        }
    }

    fn configure(core: &mut MobCore) {
        core.not_a_mob = true;
        core.unpushable = true;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.invisible = take_bool(nbt, "Invisible").unwrap_or(false);
        self.small = take_bool(nbt, "Small").unwrap_or(false);
        self.show_arms = take_bool(nbt, "ShowArms").unwrap_or(false);
        self.disabled_slots = take_number(nbt, "DisabledSlots").map_or(0, |v| v as i32);
        self.no_base_plate = take_bool(nbt, "NoBasePlate").unwrap_or(false);
        self.marker = take_bool(nbt, "Marker").unwrap_or(false);
        if let Some(Tag::Compound(mut pose)) = nbt.remove("Pose") {
            for (key, slot) in POSE_KEYS.iter().zip(&mut self.poses) {
                if let Some(angles) = take(&mut pose, key, rotations) {
                    *slot = angles;
                }
            }
            if !pose.is_empty() {
                nbt.insert("Pose", pose);
            }
        }
        core.size = Self::size_for(self.small, self.marker, core.base.kind.size());
        core.no_physics = self.marker || core.base.no_gravity;
        core.head_yaw = core.base.yaw;
        core.body_yaw = core.base.yaw;
        core.equipment.take_changed();
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Invisible", self.invisible);
        out.insert("Small", self.small);
        out.insert("ShowArms", self.show_arms);
        out.insert("DisabledSlots", self.disabled_slots);
        out.insert("NoBasePlate", self.no_base_plate);
        if self.marker {
            out.insert("Marker", true);
        }
        let mut pose = Compound::new();
        for ((key, angles), default) in POSE_KEYS.iter().zip(self.poses).zip(DEFAULT_POSES) {
            if angles != default {
                pose.insert(*key, Tag::List(angles.map(Tag::Float).to_vec()));
            }
        }
        out.insert("Pose", pose);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        if self.invisible {
            values.push((
                metadata::ENTITY_FLAGS,
                MetadataValue::Byte(core.base.flags() | metadata::FLAG_INVISIBLE),
            ));
        }
        values.push((CLIENT_FLAGS, MetadataValue::Byte(self.flags())));
        for (offset, angles) in self.poses.iter().enumerate() {
            values.push((HEAD_POSE + offset as u8, MetadataValue::Rotations(*angles)));
        }
        values
    }

    fn changed_metadata(&self, _core: &MobCore, index: u8) -> Option<MetadataValue> {
        self.data_value(index)
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: None,
            hurt: Some(SoundEvent::EntityArmorStandHit),
            death: Some(SoundEvent::EntityArmorStandBreak),
            step: None,
            volume: 1.0,
        }
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn tick(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.no_physics = self.marker || core.base.no_gravity;
        core.head_yaw = core.base.yaw;
        core.body_yaw = core.base.yaw;
        core.base.velocity[0] = 0.0;
        core.base.velocity[2] = 0.0;
        for index in std::mem::take(&mut self.changed) {
            core.changed(index);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let held = interaction.stack.as_deref();
        if self.marker || held.is_some_and(|s| s.item() == Item::NameTag) {
            return None;
        }
        let for_item = held.map_or(EquipmentSlot::Mainhand, slot_for);
        match held {
            None => {
                let scale = f64::from(core.size_scale).max(f64::MIN_POSITIVE);
                let clicked = self.clicked_slot(interaction.location[1] / scale, core);
                let target = if self.is_disabled(clicked) {
                    for_item
                } else {
                    clicked
                };
                if core.equipment.get(target).is_some() {
                    return self.swap(core, target, interaction, cx);
                }
                None
            }
            Some(_) => {
                if self.is_disabled(for_item) || (is_hand(for_item) && !self.show_arms) {
                    return None;
                }
                self.swap(core, for_item, interaction, cx)
            }
        }
    }

    fn dispensed_on(
        &mut self,
        core: &mut MobCore,
        stack: &tuff_core::world::region_api::Fired,
        cx: &mut TickContext,
    ) -> Option<DispenseOutcome> {
        let equippable = stack.equippable()?;
        if !equippable.dispensable
            || stack.restricted_for(core.base.kind)
            || !usable(equippable.slot)
            || is_hand(equippable.slot)
            || self.is_disabled(equippable.slot)
            || core.equipment.get(equippable.slot).is_some()
        {
            return None;
        }
        core.equipment.set(equippable.slot, Some(stack.take()?));
        core.sound(equippable.equip_sound, 1.0, 1.0, cx);
        Some(DispenseOutcome::Dispensed)
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) -> bool {
        if core.discarded {
            return false;
        }
        if let Some((_, kind)) = damage.attacker
            && kind != EntityType::Player
            && kind.mob().category != tuff_vanilla_data::entity::MobCategory::Misc
            && !cx.shared.rule_bool("mob_griefing")
        {
            return false;
        }
        if damage.is("bypasses_invulnerability") {
            core.discarded = true;
            return false;
        }
        if self.invisible || self.marker {
            return false;
        }
        if damage.is("is_explosion") {
            self.broken_by_anything(core, cx);
            core.discarded = true;
            return false;
        }
        if damage.is("ignites_armor_stands") {
            if core.base.on_fire() {
                self.cause_damage(core, 0.15, cx);
            } else {
                core.base.fire = core.base.fire.max(100);
            }
            return false;
        }
        if damage.is("burns_armor_stands") && core.health > 0.5 {
            self.cause_damage(core, 4.0, cx);
            return false;
        }
        let can_break = damage.is("can_break_armor_stand");
        let always_kills = damage.is("always_kills_armor_stands");
        if !can_break && !always_kills {
            return false;
        }
        let player = damage
            .attacker
            .filter(|(_, kind)| *kind == EntityType::Player)
            .and_then(|(id, _)| cx.players.iter().find(|p| p.id == id));
        if player.is_some_and(|p| p.creative) {
            Self::broken_sound(core, cx);
            core.discarded = true;
            return false;
        }
        let recent = self
            .last_hit
            .is_some_and(|at| cx.tick.saturating_sub(at) <= WOBBLE_TICKS);
        if recent || always_kills {
            self.broken_by_player(core, cx);
            core.discarded = true;
            return false;
        }
        cx.effects.push(Effect::EntityPackets(packets([framed(
            &play::EntityEvent {
                entity_id: core.base.id,
                status: HIT_EVENT,
            },
        )])));
        self.last_hit = Some(cx.tick);
        false
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::ArmorStand, crate::load::<ArmorStand>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mob;
    use tuff_core::world::entity::{EntityBase, RegionEntity};

    fn stand(nbt: &mut Compound) -> Mob<ArmorStand> {
        let base = EntityBase::new(EntityType::ArmorStand, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut mob = Mob::<ArmorStand>::new(base, &mut rng);
        mob.species.load(&mut mob.core, nbt);
        mob
    }

    #[tuff_core::watchdog]
    #[test]
    fn data_indices_match_the_dumped_layout() {
        let layout = EntityType::ArmorStand.data_layout();
        let entity = stand(&mut Compound::new());
        for (index, value) in entity.metadata() {
            assert_eq!(
                layout[usize::from(index)],
                value.type_id() as u8,
                "index {index}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_damage_tags_the_stand_cares_about_exist() {
        for tag in [
            "can_break_armor_stand",
            "always_kills_armor_stands",
            "ignites_armor_stands",
            "burns_armor_stands",
        ] {
            let found = tuff_vanilla_data::tag::damage_type::ALL
                .iter()
                .find(|(name, _)| *name == format!("minecraft:{tag}"))
                .map(|(_, kinds)| kinds.len());
            assert!(found.is_some_and(|n| n > 0), "{tag}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_click_height_picks_the_slot_dressed_there() {
        let mut mob = stand(&mut Compound::new());
        for (slot, item) in [
            (EquipmentSlot::Head, Item::IronHelmet),
            (EquipmentSlot::Chest, Item::IronChestplate),
            (EquipmentSlot::Legs, Item::IronLeggings),
            (EquipmentSlot::Feet, Item::IronBoots),
        ] {
            mob.core
                .equipment
                .set(slot, Some(ItemStack::mint(item, 1, MintReason::Test)));
        }
        let at = |y| mob.species.clicked_slot(y, &mob.core);
        assert_eq!(at(1.8), EquipmentSlot::Head);
        assert_eq!(at(1.2), EquipmentSlot::Chest);
        assert_eq!(at(0.6), EquipmentSlot::Legs);
        assert_eq!(at(0.2), EquipmentSlot::Feet);
        assert_eq!(at(-0.5), EquipmentSlot::Mainhand);
    }

    #[tuff_core::watchdog]
    #[test]
    fn disabled_slot_bits_follow_the_legacy_layout() {
        let mut mob = stand(&mut Compound::new());
        mob.species.show_arms = true;
        mob.species.disabled_slots = 1 << 4;
        assert!(mob.species.is_disabled(EquipmentSlot::Head));
        assert!(!mob.species.is_disabled(EquipmentSlot::Chest));
        mob.species.disabled_slots = 1 << (3 + 8);
        assert!(mob.species.bit(EquipmentSlot::Chest, 8));
        assert!(!mob.species.bit(EquipmentSlot::Chest, 16));
        mob.species.show_arms = false;
        assert!(mob.species.is_disabled(EquipmentSlot::Mainhand));
    }

    #[tuff_core::watchdog]
    #[test]
    fn markers_have_no_size_and_never_fall() {
        tuff_test_support::with_tick_context(|cx| {
            let mut mob = stand(&mut Compound::new().with("Marker", true));
            assert_eq!(mob.core.size.width, 0.0);
            assert_eq!(mob.core.size.height, 0.0);
            let y = mob.core.base.position[1];
            mob.base_mut().position[1] = y + 5.0;
            for _ in 0..10 {
                mob.tick(cx);
            }
            assert_eq!(mob.base().position[1], y + 5.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_standing_stand_falls_onto_the_ground() {
        tuff_test_support::with_tick_context(|cx| {
            let mut mob = stand(&mut Compound::new());
            mob.base_mut().position[1] = 70.0;
            for _ in 0..60 {
                mob.tick(cx);
            }
            assert!((mob.base().position[1] - 64.0).abs() < 0.01);
        });
    }
}
