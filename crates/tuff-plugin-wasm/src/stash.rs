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
///
/// Moves between stashes are checked (`tuff.items.conservationChecks`):
/// a failed check refuses the move and quarantines the stashes, which
/// then refuse every operation that would hand items out, so nothing from
/// an inconsistent stash reaches a player.
#[derive(Default)]
pub struct Stashes {
    last: u64,
    held: HashMap<u64, Vec<ItemStack>>,
    quarantined: bool,
    fail_next: bool,
    violations: Vec<StashViolation>,
}

/// A refused stash move, for the audit log.
#[derive(Debug, PartialEq)]
pub struct StashViolation {
    pub operation: &'static str,
    pub detail: String,
}

fn total(stacks: &[ItemStack]) -> u64 {
    stacks.iter().map(|stack| u64::from(stack.count())).sum()
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

    /// Takes a stash and everything in it; `None` while quarantined.
    pub fn remove(&mut self, stash: u64) -> Option<Vec<ItemStack>> {
        if self.quarantined {
            return None;
        }
        self.held.remove(&stash)
    }

    /// Whether a failed check froze these stashes.
    pub fn quarantined(&self) -> bool {
        self.quarantined
    }

    /// Failure injection for tests: the next checked move fails before
    /// anything moves.
    pub fn fail_next_check(&mut self) {
        self.fail_next = true;
    }

    /// Whether a move may start; an injected failure is a violation.
    fn may_move(&mut self, operation: &'static str) -> bool {
        if self.quarantined {
            return false;
        }
        if std::mem::take(&mut self.fail_next) {
            self.violated(operation, format_args!("the check was made to fail"));
            return false;
        }
        true
    }

    fn violated(&mut self, operation: &'static str, detail: std::fmt::Arguments<'_>) {
        tuff_item::item::conservation::violation(operation, detail);
        self.violations.push(StashViolation {
            operation,
            detail: detail.to_string(),
        });
        self.quarantined = true;
    }

    /// The violations found since the last call, for the audit log.
    pub fn take_violations(&mut self) -> Vec<StashViolation> {
        std::mem::take(&mut self.violations)
    }

    /// Moves everything in `source` onto `target`, checked; `None` for
    /// an unknown stash or a refused move.
    pub fn merge(&mut self, target: u64, source: u64) -> Option<()> {
        if target == source || !self.held.contains_key(&target) || !self.held.contains_key(&source)
        {
            return None;
        }
        if !self.may_move("plugin stash merge") {
            return None;
        }
        let checks = tuff_item::item::conservation::checks();
        let before = checks.then(|| total(&self.held[&target]) + total(&self.held[&source]));
        let moved = self.held.remove(&source)?;
        self.fill(target, moved);
        if let Some(before) = before {
            let after = total(&self.held[&target]);
            if before != after {
                self.violated(
                    "plugin stash merge",
                    format_args!("held {before} before and {after} after"),
                );
                return None;
            }
        }
        Some(())
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
        if !self.held.contains_key(&stash) || !self.may_move("plugin stash split") {
            return None;
        }
        let checks = tuff_item::item::conservation::checks();
        let held = self.held.get_mut(&stash)?;
        let before = checks.then(|| total(held));
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
        if let Some(before) = before {
            let (after, moved) = (total(held), total(&taken));
            if before != after + moved || moved > u64::from(count) {
                self.fill(stash, taken);
                self.violated(
                    "plugin stash split",
                    format_args!("held {before}, kept {after} and moved {moved} of {count}"),
                );
                return None;
            }
        }
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

    #[tuff_core::watchdog]
    #[test]
    fn a_merge_moves_everything_and_keeps_the_total() {
        let mut stashes = Stashes::default();
        let (a, b) = (stashes.create().unwrap(), stashes.create().unwrap());
        stashes.fill(a, vec![stack(Item::Stone, 40)]);
        stashes.fill(b, vec![stack(Item::Stone, 30), stack(Item::Dirt, 2)]);
        assert_eq!(stashes.merge(a, b), Some(()));
        assert!(!stashes.contains(b));
        assert_eq!(
            counts(&stashes, a),
            vec![(Item::Stone, 64), (Item::Stone, 6), (Item::Dirt, 2)]
        );
        assert_eq!(stashes.merge(a, a), None);
        assert!(!stashes.quarantined());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_failed_check_moves_nothing_and_freezes_the_stashes() {
        let mut stashes = Stashes::default();
        let (a, b) = (stashes.create().unwrap(), stashes.create().unwrap());
        stashes.fill(a, vec![stack(Item::Stone, 10)]);
        stashes.fill(b, vec![stack(Item::Stone, 5)]);
        stashes.fail_next_check();
        assert!(stashes.split(a, None, 4).is_none());
        assert!(stashes.quarantined());
        assert_eq!(counts(&stashes, a), vec![(Item::Stone, 10)]);
        assert!(stashes.merge(a, b).is_none());
        assert_eq!(counts(&stashes, b), vec![(Item::Stone, 5)]);
        assert!(stashes.split(a, None, 4).is_none());
        assert!(stashes.remove(a).is_none(), "nothing leaves a frozen stash");
        assert_eq!(counts(&stashes, a), vec![(Item::Stone, 10)]);
        let violations = stashes.take_violations();
        assert_eq!(violations.len(), 1, "one row for the audit log");
        assert_eq!(violations[0].operation, "plugin stash split");
        assert!(stashes.take_violations().is_empty());
    }
}
