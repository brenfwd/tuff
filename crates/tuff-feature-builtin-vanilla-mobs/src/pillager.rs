//! Pillagers: crossbow-wielding illagers that keep their distance and
//! reload once out of range or out of sight.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::goal::GoalSelector;
use super::goals::{AvoidPlayer, Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::ranged::RangedCrossbowAttack;
use super::target::{HurtByTarget, NearestAttackablePlayer};
use super::{MobCore, MobSounds, Species};
use super::{illager, raider};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::TickContext;
use tuff_item::item::ItemStack;

const BOW_RANGE: f32 = 8.0;

const IS_CHARGING: u8 = super::data::AGE_LOCKED;

const INVENTORY_SLOTS: usize = 5;
const SPAWN_CROSSBOW_ENCHANT_ODDS: i32 = 300;

#[derive(Debug, Default)]
pub struct Pillager {
    inventory: Vec<ItemStack>,
}

impl Pillager {
    fn add(&mut self, stack: ItemStack) -> Option<ItemStack> {
        let mut stack = stack;
        for held in &mut self.inventory {
            if held.item == stack.item && held.components == stack.components {
                let room = held.max_stack_size().saturating_sub(held.count);
                let moved = room.min(stack.count);
                held.count += moved;
                stack.count -= moved;
            }
        }
        if stack.count == 0 {
            return None;
        }
        if self.inventory.len() < INVENTORY_SLOTS {
            self.inventory.push(stack);
            return None;
        }
        Some(stack)
    }

    fn can_add(&self, item: Item) -> bool {
        self.inventory.len() < INVENTORY_SLOTS
            || self
                .inventory
                .iter()
                .any(|held| held.item == item && held.count < held.max_stack_size())
    }

    fn takeable_banner(&self, core: &MobCore, cx: &mut TickContext) -> Option<(i32, u8)> {
        if !raider::has_active_raid(core, cx) || !self.can_add(Item::WhiteBanner) {
            return None;
        }
        let reach = super::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        cx.entities.iter().find_map(|e| {
            let (item, count, takeable) = e.item?;
            (takeable
                && count > 0
                && item == Item::WhiteBanner
                && !e.ominous_banner
                && super::physics::intersects(e.bounds, reach))
            .then_some((e.id, count))
        })
    }
}

impl Species for Pillager {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        if let Some(tuff_nbt::Tag::List(list)) = nbt.remove("Inventory") {
            self.inventory = list
                .iter()
                .filter_map(|tag| ItemStack::from_nbt(tag.as_compound()?))
                .take(INVENTORY_SLOTS)
                .collect();
        }
    }

    fn save(&self, _core: &MobCore, out: &mut tuff_nbt::Compound) {
        out.insert(
            "Inventory",
            tuff_nbt::Tag::List(
                self.inventory
                    .iter()
                    .map(|stack| tuff_nbt::Tag::Compound(stack.to_nbt()))
                    .collect(),
            ),
        );
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.is_alive() || !core.can_pick_up_loot {
            return;
        }
        if let Some((id, count)) = self.takeable_banner(core, cx) {
            cx.effects
                .push(tuff_core::world::entity::Effect::TakeItem { item: id, count });
        }
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        if let Some(rest) = self.add(stack) {
            self.inventory.push(rest);
        }
        core.persistence_required = true;
    }

    fn died(&mut self, core: &mut MobCore, _damage: &super::Damage, cx: &mut TickContext) {
        for stack in std::mem::take(&mut self.inventory) {
            core.drop_item(stack, 0.0, cx);
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, AvoidPlayer::entities(illager::CREAKING, 8.0, 1.0, 1.2));
        goals.add(2, raider::HoldGroundAttack::new(10.0));
        goals.add(1, HurtByTarget::new(&[]).alerting(illager::RAIDERS));
        goals.add(2, NearestAttackablePlayer::new(true));
        illager::add_villager_targets(&mut goals, 3, false, true);
        goals.add(
            3,
            RangedCrossbowAttack::new(
                1.0,
                BOW_RANGE,
                Item::Crossbow,
                SoundEvent::ItemCrossbowShoot,
            ),
        );
        goals.add(8, RandomStroll::with_interval(0.6, RandomStroll::INTERVAL));
        goals.add(9, LookAtPlayer::new(15.0));
        goals.add(9, RandomLookAround::default());
        raider::add_goals(&mut goals);
        core.goals = goals;
        raider::make_raider(core, true, SoundEvent::EntityPillagerCelebrate);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (illager::CELEBRATING, illager::celebrating(core)),
            (IS_CHARGING, MetadataValue::Boolean(core.signal != 0)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        super::monster::sounds(
            SoundEvent::EntityPillagerAmbient,
            SoundEvent::EntityPillagerHurt,
            SoundEvent::EntityPillagerDeath,
            None,
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.can_pick_up_loot = true;
        let in_raid = core.raider.as_ref().is_some_and(|r| r.raid.is_some());
        if !in_raid || core.equipment.get(EquipmentSlot::Mainhand).is_none() {
            core.equipment.set(
                EquipmentSlot::Mainhand,
                Some(ItemStack::new(Item::Crossbow, 1)),
            );
        }
        let difficulty = super::monster::LocalDifficulty::at(cx, core.base.position);
        core.populate_spawn_enchantments(difficulty, cx.rng);
        enchant_spawn_crossbow(core, cx.rng);
    }
}

fn enchant_spawn_crossbow(core: &mut MobCore, rng: &mut StdRng) {
    if rng.gen_range(0..SPAWN_CROSSBOW_ENCHANT_ODDS) != 0 {
        return;
    }
    let Some(weapon) = core.equipment.get(EquipmentSlot::Mainhand).cloned() else {
        return;
    };
    if weapon.item == Item::Crossbow {
        let enchanted = tuff_item::item::enchanting::enchant_from_provider(
            weapon,
            "minecraft:pillager_spawn_crossbow",
            0.0,
            rng,
        );
        core.equipment.set(EquipmentSlot::Mainhand, Some(enchanted));
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Pillager, super::load::<Pillager>);
    registry.register_spawn_rule(EntityType::Pillager, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_inventory_holds_five_stacks_and_merges_banners() {
        let mut pillager = Pillager::default();
        assert!(pillager.add(ItemStack::new(Item::WhiteBanner, 1)).is_none());
        assert!(pillager.add(ItemStack::new(Item::WhiteBanner, 1)).is_none());
        assert_eq!(pillager.inventory.len(), 1);
        assert_eq!(pillager.inventory[0].count, 2);
        for item in [Item::Stick, Item::Apple, Item::Bread, Item::Coal] {
            assert!(pillager.add(ItemStack::new(item, 1)).is_none());
        }
        assert!(pillager.add(ItemStack::new(Item::Bone, 1)).is_some());
        assert!(!pillager.can_add(Item::Bone));
        assert!(pillager.can_add(Item::WhiteBanner));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_spawn_crossbow_is_pierced_one_time_in_three_hundred() {
        crate::tests::with_context(|core, _cx| {
            let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(7);
            let mut pierced = 0;
            for _ in 0..30_000 {
                core.equipment.set(
                    EquipmentSlot::Mainhand,
                    Some(ItemStack::new(Item::Crossbow, 1)),
                );
                enchant_spawn_crossbow(core, &mut rng);
                let stack = core.equipment.get(EquipmentSlot::Mainhand).unwrap();
                pierced += usize::from(stack.components.enchantments.is_some());
            }
            assert!((60..150).contains(&pierced), "{pierced}");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn pillager_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::Pillager.data_layout();
        assert_eq!(layout.len(), IS_CHARGING as usize + 1);
        assert_eq!(layout[illager::CELEBRATING as usize..], [boolean, boolean]);
    }
}
