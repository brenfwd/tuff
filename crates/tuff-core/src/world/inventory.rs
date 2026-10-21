//! The survival player inventory window (window ID 0) and its click handling
//! (https://minecraft.wiki/w/Java_Edition_protocol/Inventory).

use std::ops::RangeInclusive;

use rand::Rng;
use rand::seq::SliceRandom;

use tuff_item::item::{Hurt, ItemStack};

fn prevents_equipment_drop(stack: &ItemStack) -> bool {
    let registry = tuff_item::enchantment::current();
    tuff_item::enchantment::leveled(&registry, stack.components.enchantments.as_ref())
        .any(|(_, def, _)| def.effects.prevent_equipment_drop)
}
use crate::world::menu::{self, BundleSound, Click, Drag, MenuKind, ResultHook};

pub const SLOTS: usize = 46;
const RESULT: usize = 0;
const CRAFT_GRID: RangeInclusive<usize> = 1..=4;
const MAIN: RangeInclusive<usize> = 9..=35;
const HOTBAR: RangeInclusive<usize> = 36..=44;
const OFFHAND: usize = 45;

#[derive(tuff_item::item::DupClone, Debug)]
pub struct Inventory {
    slots: Box<[Option<ItemStack>]>,
    carried: Option<ItemStack>,
    selected: usize,
    state_id: i32,
    drag: Option<Drag>,
    bundle_sounds: Vec<BundleSound>,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: std::iter::repeat_with(|| None).take(SLOTS).collect(),
            carried: None,
            selected: 0,
            state_id: 0,
            drag: None,
            bundle_sounds: Vec::new(),
        }
    }
}

/// Moves as much of `from` into `to` as fits, returning what is left.
fn merge(to: &mut Option<ItemStack>, from: ItemStack, limit: u8) -> Option<ItemStack> {
    match to {
        None => {
            let (placed, leftover) = from.split(limit.max(1));
            *to = Some(placed);
            leftover
        }
        Some(existing) if existing.stacks_with(&from) => {
            let room = limit
                .min(existing.max_stack_size())
                .saturating_sub(existing.count());
            if room == 0 {
                return Some(from);
            }
            existing.absorb(from, existing.count() + room)
        }
        Some(_) => Some(from),
    }
}

impl Inventory {
    pub const OFFHAND_SLOT: usize = OFFHAND;

    pub fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots
    }

    /// Sets a slot directly, e.g. when loading saved data. Out-of-range
    /// slots are ignored.
    pub fn set_slot(&mut self, slot: usize, stack: Option<ItemStack>) {
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = stack;
        }
    }

    pub fn carried(&self) -> Option<&ItemStack> {
        self.carried.as_ref()
    }

    pub fn set_carried(&mut self, stack: Option<ItemStack>) {
        self.carried = stack;
    }

    pub fn carried_mut(&mut self) -> &mut Option<ItemStack> {
        &mut self.carried
    }

    pub fn slot_mut(&mut self, slot: usize) -> &mut Option<ItemStack> {
        &mut self.slots[slot]
    }

    /// Forgets an unfinished drag, e.g. when another window opens.
    pub fn reset_drag(&mut self) {
        self.drag = None;
    }

    pub fn state_id(&self) -> i32 {
        self.state_id
    }

    /// Bumps the state ID; call whenever the server sends inventory contents.
    pub fn next_state_id(&mut self) -> i32 {
        self.state_id = self.state_id.wrapping_add(1) & 0x7FFF;
        self.state_id
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The first main-inventory or hotbar window slot holding `item`.
    pub fn find_item(&self, item: tuff_vanilla_data::Item) -> Option<usize> {
        HOTBAR
            .chain(MAIN)
            .find(|slot| self.slots[*slot].as_ref().is_some_and(|s| s.item == item))
    }

    /// The first hotbar or main-inventory window slot whose stack
    /// matches, in the game's inventory order.
    pub fn find_matching(&self, matches: impl Fn(&ItemStack) -> bool) -> Option<usize> {
        HOTBAR
            .chain(MAIN)
            .find(|slot| self.slots[*slot].as_ref().is_some_and(&matches))
    }

    /// Vanilla's pick-block hotbar choice, scanning from the selected slot
    /// and wrapping: the first empty slot, else the first whose stack is
    /// not enchanted, else the selected slot.
    pub fn suitable_hotbar(&self) -> usize {
        let order = || (0..9).map(|i| (self.selected + i) % 9);
        order()
            .find(|hotbar| self.slots[HOTBAR.start() + hotbar].is_none())
            .or_else(|| {
                order().find(|hotbar| {
                    self.slots[HOTBAR.start() + hotbar]
                        .as_ref()
                        .is_none_or(|stack| !stack.is_enchanted())
                })
            })
            .unwrap_or(self.selected)
    }

    /// Pick block: selects `stack` if a hotbar slot holds the same item
    /// with the same components, else swaps it in from the main inventory,
    /// else (only with `create`) puts a copy in a suitable hotbar slot.
    /// Returns whether anything was picked.
    pub fn pick(&mut self, stack: ItemStack, create: bool) -> bool {
        match self.find_matching(|held| held.stacks_with(&stack)) {
            Some(slot) if HOTBAR.contains(&slot) => {
                self.selected = slot - HOTBAR.start();
            }
            Some(slot) => self.pick_slot(slot),
            None if create => self.add_and_pick(stack),
            None => return false,
        }
        true
    }

    /// Brings a main-inventory slot into the hotbar by swapping it with a
    /// suitable hotbar slot, which becomes selected.
    pub fn pick_slot(&mut self, slot: usize) {
        self.selected = self.suitable_hotbar();
        self.slots.swap(HOTBAR.start() + self.selected, slot);
    }

    /// Creative pick block for an item not in the inventory: selects a
    /// suitable hotbar slot, moving whatever was there to a free slot.
    pub fn add_and_pick(&mut self, stack: ItemStack) {
        self.selected = self.suitable_hotbar();
        let hand = HOTBAR.start() + self.selected;
        let free = HOTBAR.chain(MAIN).find(|slot| self.slots[*slot].is_none());
        if let (Some(existing), Some(free)) = (self.slots[hand].take(), free) {
            self.slots[free] = Some(existing);
        }
        self.slots[hand] = Some(stack);
    }

    /// Window slot of a hotbar index.
    pub fn hotbar_slot(hotbar: usize) -> usize {
        HOTBAR.start() + hotbar
    }

    /// The armor window slot an equippable item goes in.
    pub fn armor_slot(slot: tuff_vanilla_data::item::EquipmentSlot) -> Option<usize> {
        use tuff_vanilla_data::item::EquipmentSlot;
        Some(match slot {
            EquipmentSlot::Head => 5,
            EquipmentSlot::Chest => 6,
            EquipmentSlot::Legs => 7,
            EquipmentSlot::Feet => 8,
            EquipmentSlot::Offhand => OFFHAND,
            _ => return None,
        })
    }

    pub fn swap_slots(&mut self, a: usize, b: usize) {
        self.slots.swap(a, b);
    }

    pub fn set_selected(&mut self, hotbar: usize) -> bool {
        if hotbar > 8 {
            return false;
        }
        self.selected = hotbar;
        true
    }

    /// Uses up one death-protecting item, the main hand's before the off
    /// hand's, returning the used copy and its effects.
    pub fn take_death_protection(
        &mut self,
    ) -> Option<(ItemStack, Vec<tuff_item::item::DeathEffect>)> {
        for off_hand in [false, true] {
            let slot = self.hand_slot(off_hand);
            let Some(stack) = self.slots[slot].as_mut() else {
                continue;
            };
            let Some(effects) = stack.death_protection() else {
                continue;
            };
            let used = stack.take(1);
            if stack.count() == 0 {
                self.slots[slot] = None;
            }
            return Some((used, effects));
        }
        None
    }

    pub fn hand_mut(&mut self, off_hand: bool) -> Option<&mut ItemStack> {
        let slot = self.hand_slot(off_hand);
        self.slots[slot].as_mut()
    }

    pub(crate) fn hand_slot(&self, off_hand: bool) -> usize {
        if off_hand {
            OFFHAND
        } else {
            HOTBAR.start() + self.selected
        }
    }

    /// The stack in an equipment slot (hands and armor).
    pub fn equipped(&self, slot: tuff_vanilla_data::item::EquipmentSlot) -> Option<&ItemStack> {
        use tuff_vanilla_data::item::EquipmentSlot;
        match slot {
            EquipmentSlot::Mainhand => self.hand(false),
            other => self.slots[Self::armor_slot(other)?].as_ref(),
        }
    }

    pub fn set_hand(&mut self, off_hand: bool, stack: Option<ItemStack>) {
        let slot = self.hand_slot(off_hand);
        self.slots[slot] = stack;
    }

    pub fn hand(&self, off_hand: bool) -> Option<&ItemStack> {
        self.slots[self.hand_slot(off_hand)].as_ref()
    }

    /// Every equipment slot Mending may draw from: main hand, off hand and
    /// worn armor.
    fn mending_slots(&self) -> [usize; 6] {
        [self.hand_slot(false), OFFHAND, 5, 6, 7, 8]
    }

    /// Spends orb experience repairing a random damaged Mending item, the
    /// way the game splits an orb between mending and normal experience:
    /// a new random eligible slot is picked each time the current one runs
    /// out of damage to repair. Returns the experience left over once no
    /// eligible item remains, to award as normal experience.
    pub fn mend(&mut self, mut xp: i32, rng: &mut impl Rng) -> i32 {
        while xp > 0 {
            let candidates: Vec<usize> = self
                .mending_slots()
                .into_iter()
                .filter(|&slot| {
                    self.slots[slot]
                        .as_ref()
                        .is_some_and(ItemStack::wants_mending)
                })
                .collect();
            let Some(&slot) = candidates.choose(rng) else {
                break;
            };
            let stack = self.slots[slot].as_mut().expect("filtered above");
            let spent = stack.repair_with_experience(xp);
            if spent == 0 {
                break;
            }
            xp -= spent;
        }
        xp
    }

    /// Removes up to `count` items from a hand, returning what was removed.
    pub fn take_from_hand(&mut self, off_hand: bool, count: u8) -> Option<ItemStack> {
        let slot = self.hand_slot(off_hand);
        self.take(slot, count)
    }

    /// Applies durability loss to the stack in a hand; a broken stack
    /// leaves the slot empty.
    pub fn hurt_hand(&mut self, off_hand: bool, amount: u32, rng: &mut impl Rng) -> Hurt {
        let slot = self.hand_slot(off_hand);
        self.hurt_slot(slot, amount, rng)
    }

    /// Applies durability loss to the stack in a window slot.
    pub fn hurt_slot(&mut self, slot: usize, amount: u32, rng: &mut impl Rng) -> Hurt {
        let Some(stack) = self.slots.get_mut(slot).and_then(Option::as_mut) else {
            return Hurt::Unchanged;
        };
        let outcome = stack.hurt(amount, rng);
        if stack.count() == 0 {
            self.slots[slot] = None;
        }
        outcome
    }

    fn take(&mut self, slot: usize, count: u8) -> Option<ItemStack> {
        let stack = self.slots[slot].as_mut()?;
        let taken = count.min(stack.count());
        if stack.count() > taken {
            Some(stack.take(taken))
        } else {
            self.slots[slot].take()
        }
    }

    pub fn swap_hands(&mut self) {
        let hand = self.hand_slot(false);
        self.slots.swap(hand, OFFHAND);
    }

    /// Adds a stack in vanilla pickup order: stacks with room (selected slot,
    /// off hand, hotbar, main), then empty slots (hotbar, main). Returns the
    /// part that did not fit.
    pub fn add(&mut self, stack: ItemStack) -> Option<ItemStack> {
        let hand = self.hand_slot(false);
        let order: Vec<usize> = [hand, OFFHAND]
            .into_iter()
            .chain(HOTBAR)
            .chain(MAIN)
            .collect();
        let remaining = self.fill(stack, order.iter().copied(), |_, slot, left| {
            slot.as_ref().is_some_and(|s| s.stacks_with(left))
        });
        self.fill(remaining?, HOTBAR.chain(MAIN), |_, slot, _| slot.is_none())
    }

    /// How many of `stack` `add` would take, without adding them.
    pub fn room_for(&self, stack: &ItemStack) -> u8 {
        self.total_room_for(stack).min(u32::from(stack.count())) as u8
    }

    /// How many items like `stack` `add` could take in total.
    pub fn total_room_for(&self, stack: &ItemStack) -> u32 {
        let limit = stack.max_stack_size();
        let mut room = 0u32;
        for slot in [OFFHAND].into_iter().chain(HOTBAR).chain(MAIN) {
            room += match &self.slots[slot] {
                None if slot != OFFHAND => u32::from(limit),
                None => 0,
                Some(s) if s.stacks_with(stack) => {
                    u32::from(limit.min(s.max_stack_size()).saturating_sub(s.count()))
                }
                Some(_) => 0,
            };
        }
        room
    }

    /// Merges `stack` into each slot of `targets` accepted by `accepts`,
    /// returning what is left.
    fn fill(
        &mut self,
        stack: ItemStack,
        targets: impl Iterator<Item = usize>,
        accepts: impl Fn(usize, &Option<ItemStack>, &ItemStack) -> bool,
    ) -> Option<ItemStack> {
        let mut remaining = Some(stack);
        for slot in targets {
            let Some(left) = remaining.take() else { break };
            let limit = left.max_stack_size();
            remaining = if accepts(slot, &self.slots[slot], &left) {
                merge(&mut self.slots[slot], left, limit)
            } else {
                Some(left)
            };
        }
        remaining
    }

    /// Empties the whole inventory, including the cursor, e.g. on death.
    /// The crafting result is not an item the player owns.
    pub fn clear(&mut self) -> Vec<ItemStack> {
        self.drag = None;
        self.slots[RESULT] = None;
        let mut items: Vec<ItemStack> = self.slots.iter_mut().filter_map(Option::take).collect();
        items.extend(self.carried.take());
        items
    }

    /// Like `clear`, but items with a vanishing enchantment are destroyed.
    pub fn clear_dropping_survivors(&mut self) -> Vec<ItemStack> {
        let mut items = self.clear();
        items.retain(|stack| !prevents_equipment_drop(stack));
        items
    }

    /// Closing the window returns crafting grid items and the cursor stack,
    /// dropping whatever does not fit; the crafting result is forgotten.
    pub fn close(&mut self) -> Vec<ItemStack> {
        self.drag = None;
        self.slots[RESULT] = None;
        let mut returned: Vec<ItemStack> = CRAFT_GRID
            .filter_map(|slot| self.slots[slot].take())
            .collect();
        returned.extend(self.carried.take());
        returned
            .into_iter()
            .filter_map(|stack| self.add(stack))
            .collect()
    }

    /// Applies a Click Container action in the inventory window, returning
    /// the stacks to drop into the world. `hook` runs when the crafting
    /// result (slot 0) is taken.
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        mode: i32,
        creative: bool,
        hook: &mut dyn ResultHook,
        recipes: &crate::world::crafting::RecipeSet,
    ) -> Vec<ItemStack> {
        let mut drag = self.drag.take();
        let drops = menu::click(
            MenuKind::Player,
            &mut [],
            self,
            &mut drag,
            Click { slot, button, mode },
            menu::ClickEnv {
                creative,
                hook,
                recipes,
            },
        );
        self.drag = drag;
        drops
    }

    pub fn note_bundle_sound(&mut self, sound: BundleSound) {
        self.bundle_sounds.push(sound);
    }

    /// The bundle sounds clicks have made since the last call.
    pub fn take_bundle_sounds(&mut self) -> Vec<BundleSound> {
        std::mem::take(&mut self.bundle_sounds)
    }

    /// The crafting grid slots of the inventory window.
    pub fn craft_grid(&self) -> &[Option<ItemStack>] {
        &self.slots[CRAFT_GRID]
    }

    /// Puts a stack back as the game does when a menu closes: onto stacks
    /// with room, then free slots, returning what does not fit.
    pub fn place_back(&mut self, stack: ItemStack) -> Option<ItemStack> {
        self.add(stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::menu::{NoResult, OUTSIDE};
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    fn dirt(count: u8) -> Option<ItemStack> {
        Some(ItemStack::mint(Item::Dirt, count, MintReason::Test))
    }

    fn pickaxe(damage: u32) -> ItemStack {
        let mut stack = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
        stack.set_damage(damage);
        stack
    }

    #[tuff_core::watchdog]
    #[test]
    fn death_clear_destroys_vanishing_items() {
        let mut inv = Inventory::default();
        let mut cursed = ItemStack::mint(Item::Stone, 1, MintReason::Test);
        cursed.components.enchantments = Some(tuff_item::item::Enchantments::new(vec![(
            "minecraft:vanishing_curse".to_string(),
            1,
        )]));
        inv.slots[20] = Some(cursed);
        inv.slots[21] = dirt(3);
        let drops = inv.clear_dropping_survivors();
        assert_eq!(
            drops,
            vec![ItemStack::mint(Item::Dirt, 3, MintReason::Test)]
        );
        assert!(inv.slots[20].is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn add_prefers_existing_stacks_then_hotbar() {
        let mut inv = Inventory::default();
        inv.slots[20] = dirt(60);
        assert_eq!(
            inv.add(ItemStack::mint(Item::Dirt, 10, MintReason::Test)),
            None
        );
        assert_eq!(inv.slots[20], dirt(64));
        assert_eq!(inv.slots[36], dirt(6));
    }

    #[tuff_core::watchdog]
    #[test]
    fn add_reports_overflow_when_full() {
        let mut inv = Inventory::default();
        for slot in HOTBAR.chain(MAIN) {
            inv.slots[slot] = dirt(64);
        }
        assert_eq!(
            inv.add(ItemStack::mint(Item::Dirt, 3, MintReason::Test)),
            dirt(3)
        );
        inv.slots[OFFHAND] = dirt(62);
        assert_eq!(
            inv.add(ItemStack::mint(Item::Dirt, 3, MintReason::Test)),
            dirt(1)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn stacks_with_different_components_do_not_merge() {
        let mut named = ItemStack::mint(Item::Dirt, 10, MintReason::Test);
        named.components.custom_name = Some(Box::new("Named".into()));
        let mut inv = Inventory::default();
        inv.slots[36] = dirt(10);
        assert_eq!(inv.add(named.dup_unchecked()), None);
        assert_eq!(inv.slots[36], dirt(10));
        assert_eq!(inv.slots[37], Some(named.dup_unchecked()));
        assert_eq!(inv.add(named.dup_count_unchecked(5)), None);
        assert_eq!(inv.slots[37], Some(named.dup_count_unchecked(15)));
        inv.carried = Some(named.dup_count_unchecked(1));
        inv.click(
            36,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[36], Some(named.dup_count_unchecked(1)));
        assert_eq!(inv.carried, dirt(10));
        inv.carried = Some(pickaxe(3));
        inv.slots[38] = Some(pickaxe(0));
        inv.click(
            38,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[38], Some(pickaxe(3)));
        assert_eq!(inv.carried, Some(pickaxe(0)));
        inv.carried = Some(pickaxe(3));
        inv.click(
            39,
            0,
            6,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.carried, Some(pickaxe(3)));
        assert_eq!(inv.slots[38], Some(pickaxe(3)));
    }

    fn enchanted(item: Item) -> Option<ItemStack> {
        let mut stack = ItemStack::mint(item, 1, MintReason::Test);
        stack.components.enchantments = Some(tuff_item::item::Enchantments::new([(
            "minecraft:sharpness".to_string(),
            1,
        )]));
        Some(stack)
    }

    #[tuff_core::watchdog]
    #[test]
    fn suitable_hotbar_prefers_empty_then_unenchanted_from_the_selection() {
        let mut inv = Inventory {
            selected: 7,
            ..Inventory::default()
        };
        for slot in 36..=44 {
            inv.slots[slot] = enchanted(Item::DiamondSword);
        }
        inv.slots[36 + 2] = None;
        assert_eq!(inv.suitable_hotbar(), 2);
        inv.slots[36 + 2] = enchanted(Item::DiamondSword);
        assert_eq!(inv.suitable_hotbar(), 7);
        inv.slots[36 + 1] = dirt(1);
        inv.slots[36 + 4] = dirt(1);
        assert_eq!(inv.suitable_hotbar(), 1);
        inv.slots[36 + 8] = dirt(1);
        assert_eq!(inv.suitable_hotbar(), 8);
    }

    #[tuff_core::watchdog]
    #[test]
    fn pick_selects_swaps_or_creates_matching_components() {
        let mut named = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
        named.components.custom_name = Some(Box::new("Named".into()));
        let mut inv = Inventory::default();
        inv.slots[36] = Some(named.dup_unchecked());
        inv.slots[40] = dirt(5);
        assert!(inv.pick(ItemStack::mint(Item::Dirt, 1, MintReason::Test), false));
        assert_eq!(inv.selected, 4);

        inv.selected = 0;
        inv.slots[20] = Some(ItemStack::mint(Item::Stone, 3, MintReason::Test));
        assert!(inv.pick(ItemStack::mint(Item::Stone, 1, MintReason::Test), false));
        assert_eq!(inv.selected, 1);
        assert_eq!(
            inv.slots[37],
            Some(ItemStack::mint(Item::Stone, 3, MintReason::Test))
        );
        assert_eq!(inv.slots[20], None);

        assert!(!inv.pick(ItemStack::mint(Item::Glass, 1, MintReason::Test), false));
        assert_eq!(inv.selected, 1);

        for slot in 36..=44 {
            inv.slots[slot].get_or_insert(ItemStack::mint(Item::Sand, 1, MintReason::Test));
        }
        assert!(inv.pick(ItemStack::mint(Item::Glass, 1, MintReason::Test), true));
        assert_eq!(inv.selected, 1);
        assert_eq!(
            inv.slots[37],
            Some(ItemStack::mint(Item::Glass, 1, MintReason::Test))
        );
        assert_eq!(
            inv.slots[9],
            Some(ItemStack::mint(Item::Stone, 3, MintReason::Test))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn hurting_a_hand_removes_broken_tools() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut inv = Inventory::default();
        inv.slots[36] = Some(pickaxe(1559));
        assert_eq!(inv.hurt_hand(false, 1, &mut rng), Hurt::Damaged);
        assert_eq!(inv.hand(false).unwrap().damage(), 1560);
        assert_eq!(inv.hurt_hand(false, 1, &mut rng), Hurt::Broken);
        assert_eq!(inv.hand(false), None);
        assert_eq!(inv.hurt_hand(false, 1, &mut rng), Hurt::Unchanged);
        inv.slots[OFFHAND] = dirt(1);
        assert_eq!(inv.hurt_hand(true, 1, &mut rng), Hurt::Unchanged);
    }

    #[tuff_core::watchdog]
    #[test]
    fn pick_block_prefers_hotbar_then_swaps_into_it() {
        let mut inv = Inventory::default();
        inv.slots[20] = dirt(3);
        inv.slots[36] = Some(ItemStack::mint(Item::Stone, 1, MintReason::Test));
        inv.set_selected(0);
        assert_eq!(inv.find_item(Item::Dirt), Some(20));
        inv.pick_slot(20);
        assert_eq!(inv.selected(), 1);
        assert_eq!(inv.slots[37], dirt(3));
        assert_eq!(inv.slots[20], None);
        for slot in HOTBAR {
            inv.slots[slot] = Some(ItemStack::mint(Item::Stone, 1, MintReason::Test));
        }
        inv.set_selected(4);
        inv.add_and_pick(ItemStack::mint(Item::Torch, 1, MintReason::Test));
        assert_eq!(inv.selected(), 4);
        assert_eq!(
            inv.slots[40],
            Some(ItemStack::mint(Item::Torch, 1, MintReason::Test))
        );
        assert_eq!(
            inv.slots[9],
            Some(ItemStack::mint(Item::Stone, 1, MintReason::Test))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn left_and_right_click_pick_up_and_place() {
        let mut inv = Inventory::default();
        inv.slots[36] = dirt(7);
        inv.click(
            36,
            1,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.carried, dirt(4));
        assert_eq!(inv.slots[36], dirt(3));
        inv.click(
            37,
            1,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[37], dirt(1));
        assert_eq!(inv.carried, dirt(3));
        inv.click(
            36,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[36], dirt(6));
        assert_eq!(inv.carried, None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn clicking_different_item_swaps_and_armor_slots_reject_blocks() {
        let mut inv = Inventory::default();
        inv.slots[36] = dirt(5);
        inv.slots[37] = Some(ItemStack::mint(Item::Stone, 2, MintReason::Test));
        inv.click(
            36,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        inv.click(
            37,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[37], dirt(5));
        assert_eq!(
            inv.carried,
            Some(ItemStack::mint(Item::Stone, 2, MintReason::Test))
        );
        inv.click(
            5,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[5], None);
        inv.carried = Some(ItemStack::mint(Item::IronHelmet, 1, MintReason::Test));
        inv.click(
            5,
            0,
            0,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(
            inv.slots[5],
            Some(ItemStack::mint(Item::IronHelmet, 1, MintReason::Test))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn shift_click_moves_between_hotbar_and_main() {
        let mut inv = Inventory::default();
        inv.slots[40] = dirt(10);
        inv.slots[12] = dirt(60);
        inv.click(
            40,
            0,
            1,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[40], None);
        assert_eq!(inv.slots[12], dirt(64));
        assert_eq!(inv.slots[9], dirt(6));
        inv.click(
            12,
            0,
            1,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[36], dirt(64));
    }

    #[tuff_core::watchdog]
    #[test]
    fn number_key_swaps_with_hotbar_and_drop_key_drops() {
        let mut inv = Inventory::default();
        inv.slots[10] = dirt(3);
        inv.click(
            10,
            2,
            2,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[38], dirt(3));
        assert_eq!(inv.slots[10], None);
        assert_eq!(
            inv.click(
                38,
                0,
                4,
                false,
                &mut NoResult,
                &crate::world::crafting::RecipeSet::default()
            ),
            vec![ItemStack::mint(Item::Dirt, 1, MintReason::Test)]
        );
        assert_eq!(
            inv.click(
                38,
                1,
                4,
                false,
                &mut NoResult,
                &crate::world::crafting::RecipeSet::default()
            ),
            vec![ItemStack::mint(Item::Dirt, 2, MintReason::Test)]
        );
        assert_eq!(inv.slots[38], None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn left_drag_splits_evenly() {
        let mut inv = Inventory {
            carried: dirt(10),
            ..Default::default()
        };
        inv.click(
            OUTSIDE,
            0,
            5,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        for slot in [36, 37, 38] {
            inv.click(
                slot,
                1,
                5,
                false,
                &mut NoResult,
                &crate::world::crafting::RecipeSet::default(),
            );
        }
        inv.click(
            OUTSIDE,
            2,
            5,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[36], dirt(3));
        assert_eq!(inv.slots[38], dirt(3));
        assert_eq!(inv.carried, dirt(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn right_drag_places_one_each() {
        let mut inv = Inventory {
            carried: dirt(5),
            ..Default::default()
        };
        inv.click(
            OUTSIDE,
            4,
            5,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        inv.click(
            9,
            5,
            5,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        inv.click(
            10,
            5,
            5,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        inv.click(
            OUTSIDE,
            6,
            5,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[9], dirt(1));
        assert_eq!(inv.slots[10], dirt(1));
        assert_eq!(inv.carried, dirt(3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn double_click_collects_partial_stacks_first() {
        let mut inv = Inventory {
            carried: dirt(1),
            ..Default::default()
        };
        inv.slots[9] = dirt(64);
        inv.slots[10] = dirt(5);
        inv.click(
            11,
            0,
            6,
            false,
            &mut NoResult,
            &crate::world::crafting::RecipeSet::default(),
        );
        assert_eq!(inv.slots[10], None);
        assert_eq!(inv.carried, dirt(64));
        assert_eq!(inv.slots[9], dirt(6));
    }

    #[tuff_core::watchdog]
    #[test]
    fn outside_click_drops_cursor_and_close_returns_grid() {
        let mut inv = Inventory {
            carried: dirt(4),
            ..Default::default()
        };
        assert_eq!(
            inv.click(
                OUTSIDE,
                1,
                0,
                false,
                &mut NoResult,
                &crate::world::crafting::RecipeSet::default()
            ),
            vec![ItemStack::mint(Item::Dirt, 1, MintReason::Test)]
        );
        inv.slots[2] = dirt(2);
        assert!(inv.close().is_empty());
        assert_eq!(inv.slots[36], dirt(5));
        assert_eq!(inv.carried, None);
    }
}
