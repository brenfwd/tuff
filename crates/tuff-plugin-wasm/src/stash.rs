//! Items plugins hold. The host keeps each plugin's stashes; a plugin
//! names one by handle and moves its stacks only through host calls, so
//! it can make, move and use up items but never copy one.

use std::collections::HashMap;

use tuff_item::item::{BurnReason, ItemStack};
use tuff_vanilla_data::Item;

/// Stashes one plugin may hold at once.
pub const MAX_STASHES: usize = 1024;
/// Stacks one stash may hold.
pub const MAX_STASH_STACKS: usize = 256;

/// One plugin's stashes, by handle. Handles count from 1 and are never
/// reused. Whatever is left when the plugin goes is used up.
#[derive(Default)]
pub struct Stashes {
    last: u64,
    held: HashMap<u64, Vec<ItemStack>>,
}

impl Stashes {
    /// A new, empty stash; `None` at the limit.
    pub fn create(&mut self) -> Option<u64> {
        if self.held.len() >= MAX_STASHES {
            return None;
        }
        self.last += 1;
        self.held.insert(self.last, Vec::new());
        Some(self.last)
    }

    pub fn get(&self, stash: u64) -> Option<&[ItemStack]> {
        self.held.get(&stash).map(Vec::as_slice)
    }

    pub fn contains(&self, stash: u64) -> bool {
        self.held.contains_key(&stash)
    }

    /// Takes a stash and everything in it.
    pub fn remove(&mut self, stash: u64) -> Option<Vec<ItemStack>> {
        self.held.remove(&stash)
    }

    /// Adds `stacks` to `stash`, onto stacks they stack with first. A
    /// stash that went in the meantime holds them again.
    pub fn fill(&mut self, stash: u64, stacks: Vec<ItemStack>) {
        let held = self.held.entry(stash).or_default();
        for stack in stacks {
            add(held, stack);
        }
    }

    /// Moves up to `count` of `item` (of anything when `None`) out of
    /// `stash`; `None` for an unknown stash.
    pub fn split(&mut self, stash: u64, item: Option<Item>, count: u32) -> Option<Vec<ItemStack>> {
        let held = self.held.get_mut(&stash)?;
        let mut left = count;
        let mut taken = Vec::new();
        for slot in held.iter_mut() {
            if left == 0 {
                break;
            }
            if item.is_some_and(|item| slot.item != item) {
                continue;
            }
            let part = slot.take(left.min(u32::from(slot.count())) as u8);
            left -= u32::from(part.count());
            taken.push(part);
        }
        held.retain(|stack| !stack.is_empty());
        Some(taken)
    }

    /// Whether `stash` has room for `more` stacks.
    pub fn has_room(&self, stash: u64, more: usize) -> bool {
        self.held
            .get(&stash)
            .is_some_and(|held| held.len() + more <= MAX_STASH_STACKS)
    }
}

/// Adds `stack` to `held`, topping up stacks it stacks with first.
fn add(held: &mut Vec<ItemStack>, mut stack: ItemStack) {
    for existing in held.iter_mut() {
        if stack.is_empty() {
            return;
        }
        let room = existing.max_stack_size().saturating_sub(existing.count());
        existing.transfer(&mut stack, room);
    }
    if !stack.is_empty() {
        held.push(stack);
    }
}

impl Drop for Stashes {
    fn drop(&mut self) {
        let left: Vec<ItemStack> = self.held.drain().flat_map(|(_, held)| held).collect();
        if !left.is_empty() {
            log::warn!(
                "a plugin went holding {} stacks in stashes; they are used up",
                left.len()
            );
        }
        for stack in left {
            stack.burn(BurnReason::Plugin);
        }
    }
}

/// Items taken from a player for a plugin's stash, on their way to the
/// plugin's host.
pub struct StashFill {
    pub plugin: String,
    pub generation: u64,
    pub stash: u64,
    pub stacks: Vec<ItemStack>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;

    fn stack(item: Item, count: u8) -> ItemStack {
        ItemStack::mint(item, count, MintReason::Test)
    }

    fn counts(stashes: &Stashes, stash: u64) -> Vec<(Item, u8)> {
        stashes
            .get(stash)
            .unwrap()
            .iter()
            .map(|s| (s.item, s.count()))
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn stashes_move_items_without_copying_them() {
        let mut stashes = Stashes::default();
        let a = stashes.create().unwrap();
        stashes.fill(a, vec![stack(Item::Stone, 40), stack(Item::Stone, 40)]);
        stashes.fill(a, vec![stack(Item::Dirt, 3)]);
        assert_eq!(
            counts(&stashes, a),
            vec![(Item::Stone, 64), (Item::Stone, 16), (Item::Dirt, 3)]
        );
        let taken = stashes.split(a, Some(Item::Stone), 70).unwrap();
        assert_eq!(taken.iter().map(|s| u32::from(s.count())).sum::<u32>(), 70);
        assert_eq!(
            counts(&stashes, a),
            vec![(Item::Stone, 10), (Item::Dirt, 3)]
        );
        let b = stashes.create().unwrap();
        assert_ne!(a, b);
        stashes.fill(b, taken);
        let moved = stashes.remove(a).unwrap();
        stashes.fill(b, moved);
        assert!(!stashes.contains(a));
        assert_eq!(
            counts(&stashes, b),
            vec![(Item::Stone, 64), (Item::Stone, 16), (Item::Dirt, 3)]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn handles_are_never_reused_and_capped() {
        let mut stashes = Stashes::default();
        let first = stashes.create().unwrap();
        stashes.remove(first);
        assert_ne!(stashes.create(), Some(first));
        while stashes.create().is_some() {}
        assert_eq!(stashes.held.len(), MAX_STASHES);
    }
}
