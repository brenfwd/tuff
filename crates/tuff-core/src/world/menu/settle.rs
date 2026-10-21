//! Settling a menu click by moving real stacks. A click is worked out on
//! pictures of the slots; each slot it changed then gives up what it loses
//! into a pool and takes what it gains out of that pool, so the items that
//! end up anywhere are the items that were taken from somewhere.

use std::collections::HashMap;

use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_vanilla_data::Item;

/// How a slot changes from `before` to `after`: how many of what it held
/// leave it, and a picture of what then arrives (its count the number
/// that arrive).
pub fn delta(before: Option<&ItemStack>, after: Option<&ItemStack>) -> (u8, Option<ItemSnapshot>) {
    match (before, after) {
        (None, None) => (0, None),
        (Some(before), None) => (before.count(), None),
        (None, Some(after)) => (0, Some(ItemSnapshot::of(after))),
        (Some(before), Some(after)) if before.stacks_with(after) => {
            if after.count() >= before.count() {
                let gained = after.count() - before.count();
                (
                    0,
                    (gained > 0).then(|| ItemSnapshot::of(after).with_count(gained)),
                )
            } else {
                (before.count() - after.count(), None)
            }
        }
        (Some(before), Some(after)) => (before.count(), Some(ItemSnapshot::of(after))),
    }
}

/// Moves the stacks `gain` pictures out of `pool` into `slot`; returns how
/// many could not be found.
pub fn gain(slot: &mut Option<ItemStack>, gain: &ItemStack, pool: &mut Vec<ItemStack>) -> u8 {
    let mut missing = gain.count();
    let mut index = 0;
    while missing > 0 && index < pool.len() {
        if !pool[index].stacks_with(gain) {
            index += 1;
            continue;
        }
        let part = if pool[index].count() <= missing {
            pool.remove(index)
        } else {
            let part = pool[index].take(missing);
            index += 1;
            part
        };
        let count = part.count();
        match slot {
            None => {
                *slot = Some(part);
                missing -= count;
            }
            Some(held) => match held.join(part) {
                Ok(()) => missing -= count,
                Err(part) => {
                    pool.push(part);
                    break;
                }
            },
        }
    }
    missing
}

/// Takes the stacks `wanted` pictures out of `pool`, as many as it holds.
pub fn take_out(pool: &mut Vec<ItemStack>, wanted: &ItemStack) -> Vec<ItemStack> {
    let mut taken = Vec::new();
    let mut missing = wanted.count();
    let mut index = 0;
    while missing > 0 && index < pool.len() {
        if !pool[index].stacks_with(wanted) {
            index += 1;
            continue;
        }
        let part = if pool[index].count() <= missing {
            pool.remove(index)
        } else {
            let part = pool[index].take(missing);
            index += 1;
            part
        };
        missing -= part.count();
        taken.push(part);
    }
    taken
}

/// Whether `available` holds enough of each picture in `wanted`.
pub fn covers<'a>(
    available: impl Iterator<Item = &'a ItemStack> + Clone,
    wanted: impl Iterator<Item = &'a ItemStack>,
) -> bool {
    let mut needs: Vec<(&ItemStack, u32)> = Vec::new();
    for want in wanted {
        match needs.iter_mut().find(|(kind, _)| kind.stacks_with(want)) {
            Some((_, need)) => *need += u32::from(want.count()),
            None => needs.push((want, u32::from(want.count()))),
        }
    }
    needs.into_iter().all(|(kind, need)| {
        available
            .clone()
            .filter(|stack| stack.stacks_with(kind))
            .map(|stack| u32::from(stack.count()))
            .sum::<u32>()
            >= need
    })
}

/// How [`apply`] went.
#[derive(Debug, PartialEq, Eq)]
pub enum Settled {
    Applied,
    /// The pool could not cover the gains: nothing changed.
    Refused,
    /// The items before and after differ. With `applied` false nothing
    /// changed; otherwise the stacks moved and the actors involved must
    /// not be saved.
    Violated {
        applied: bool,
        detail: String,
    },
}

/// Counts of each item across `slots` and `pool`.
fn census<'a>(stacks: impl Iterator<Item = &'a ItemStack>) -> Vec<(Item, u64)> {
    let mut counts: Vec<(Item, u64)> = Vec::new();
    for stack in stacks.filter(|stack| !stack.is_empty()) {
        match counts.iter_mut().find(|(item, _)| *item == stack.item) {
            Some((_, count)) => *count += u64::from(stack.count()),
            None => counts.push((stack.item, u64::from(stack.count()))),
        }
    }
    counts.sort_unstable_by_key(|(item, _)| *item as u32);
    counts
}

fn census_of(slots: &[Option<ItemStack>], pool: &[ItemStack]) -> Vec<(Item, u64)> {
    census(slots.iter().flatten().chain(pool))
}

/// [`apply_moves`] with the conservation check: the items in `slots` and
/// `pool` together are the same before and after. `refuse_check` makes
/// the check fail before anything moves (failure injection in tests).
pub fn apply(
    slots: &mut [Option<ItemStack>],
    new: &[Option<ItemSnapshot>],
    pool: &mut Vec<ItemStack>,
    refuse_check: bool,
) -> Settled {
    if refuse_check {
        return Settled::Violated {
            applied: false,
            detail: "the transfer check was made to fail".to_owned(),
        };
    }
    if !tuff_item::item::conservation::checks() {
        return match apply_moves(slots, new, pool) {
            true => Settled::Applied,
            false => Settled::Refused,
        };
    }
    let before = census_of(slots, pool);
    if !apply_moves(slots, new, pool) {
        return Settled::Refused;
    }
    let after = census_of(slots, pool);
    if before == after {
        Settled::Applied
    } else {
        Settled::Violated {
            applied: true,
            detail: format!("held {before:?} before and {after:?} after"),
        }
    }
}

/// Applies a transaction to `slots`, each holding what the sender
/// expected: what each loses goes into `pool`, then what each gains comes
/// out of it. When the pool cannot cover the gains nothing changes and
/// the answer is false.
fn apply_moves(
    slots: &mut [Option<ItemStack>],
    new: &[Option<ItemSnapshot>],
    pool: &mut Vec<ItemStack>,
) -> bool {
    let deltas: Vec<(u8, Option<ItemSnapshot>)> = slots
        .iter()
        .zip(new)
        .map(|(before, after)| delta(before.as_ref(), after.as_deref()))
        .collect();
    let mut taken = Vec::new();
    for (index, (lose, _)) in deltas.iter().enumerate() {
        if let Some(stack) = ItemStack::take_from(&mut slots[index], *lose) {
            taken.push((index, stack));
        }
    }
    let available = pool.iter().chain(taken.iter().map(|(_, stack)| stack));
    if !covers(
        available,
        deltas.iter().filter_map(|(_, gain)| gain.as_deref()),
    ) {
        for (index, stack) in taken {
            ItemStack::put_back(&mut slots[index], stack);
        }
        return false;
    }
    pool.extend(taken.into_iter().map(|(_, stack)| stack));
    for (index, (_, wanted)) in deltas.iter().enumerate() {
        if let Some(wanted) = wanted {
            gain(&mut slots[index], wanted, pool);
        }
    }
    true
}

/// What a set of gains asks for beyond a set of losses (`short`), and what
/// of the losses no gain uses (`spare`), as pictures with counts.
#[derive(Debug, Default)]
pub struct Balance {
    pub short: Vec<(ItemSnapshot, u32)>,
    pub spare: Vec<(ItemSnapshot, u32)>,
}

impl Balance {
    pub fn of<'a>(
        losses: impl Iterator<Item = &'a ItemSnapshot>,
        gains: impl Iterator<Item = &'a ItemSnapshot>,
    ) -> Self {
        let mut net: Vec<(&ItemSnapshot, i64)> = Vec::new();
        let mut add = |stack: &'a ItemSnapshot, sign: i64| match net
            .iter_mut()
            .find(|(kind, _)| kind.stacks_with(stack))
        {
            Some((_, total)) => *total += sign * i64::from(stack.count()),
            None => net.push((stack, sign * i64::from(stack.count()))),
        };
        for loss in losses {
            add(loss, 1);
        }
        for gain in gains {
            add(gain, -1);
        }
        let mut balance = Self::default();
        for (kind, total) in net {
            if total > 0 {
                balance.spare.push((kind.clone(), total as u32));
            } else if total < 0 {
                balance.short.push((kind.clone(), (-total) as u32));
            }
        }
        balance
    }

    pub fn is_even(&self) -> bool {
        self.short.is_empty() && self.spare.is_empty()
    }

    /// Whether what is short holds the same items, nested contents
    /// included, as what is spare: the click only rearranged items inside
    /// containers such as bundles.
    pub fn rearranges(&self) -> bool {
        tally(&self.short) == tally(&self.spare)
    }

    /// New stacks for everything short, minted for `reason`.
    pub fn mint_short(&self, reason: tuff_item::item::MintReason) -> Vec<ItemStack> {
        let mut minted = Vec::new();
        for (kind, mut count) in self.short.iter().map(|(kind, count)| (kind, *count)) {
            let limit = u32::from(kind.max_stack_size().max(1));
            while count > 0 {
                let part = count.min(limit);
                minted.push(ItemStack::mint_like(kind, part as u8, reason));
                count -= part;
            }
        }
        minted
    }
}

fn tally(stacks: &[(ItemSnapshot, u32)]) -> HashMap<Item, u64> {
    let mut counts = HashMap::new();
    for (stack, count) in stacks {
        count_into(stack, u64::from(*count), &mut counts);
    }
    counts
}

fn count_into(stack: &ItemStack, times: u64, counts: &mut HashMap<Item, u64>) {
    *counts.entry(stack.item).or_default() += times;
    let components = &stack.components;
    if let Some(contents) = &components.bundle_contents {
        for inner in contents.items() {
            count_into(inner, times * u64::from(inner.count()), counts);
        }
    }
    for inner in components.container.iter().flatten().flatten() {
        count_into(inner, times * u64::from(inner.count()), counts);
    }
    for inner in components.charged_projectiles.iter().flatten() {
        count_into(inner, times * u64::from(inner.count()), counts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;

    fn stack(item: Item, count: u8) -> ItemStack {
        ItemStack::mint(item, count, MintReason::Test)
    }

    fn picture(item: Item, count: u8) -> Option<ItemSnapshot> {
        Some(ItemSnapshot::describe(item, count, Default::default()))
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_transaction_moves_what_slots_lose_into_what_they_gain() {
        let mut slots = vec![Some(stack(Item::Stone, 10)), None];
        let mut pool = vec![stack(Item::Dirt, 3)];
        let new = vec![picture(Item::Stone, 4), picture(Item::Dirt, 3)];
        assert_eq!(apply(&mut slots, &new, &mut pool, false), Settled::Applied);
        assert_eq!(slots[0].as_ref().map(ItemStack::count), Some(4));
        assert_eq!(
            slots[1].as_ref().map(|s| (s.item, s.count())),
            Some((Item::Dirt, 3))
        );
        assert_eq!(pool.len(), 1);
        assert_eq!((pool[0].item, pool[0].count()), (Item::Stone, 6));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_transaction_the_pool_cannot_cover_changes_nothing() {
        let mut slots = vec![Some(stack(Item::Stone, 10)), None];
        let mut pool = vec![stack(Item::Dirt, 2)];
        let new = vec![None, picture(Item::Dirt, 3)];
        assert_eq!(apply(&mut slots, &new, &mut pool, false), Settled::Refused);
        assert_eq!(slots[0].as_ref().map(ItemStack::count), Some(10));
        assert!(slots[1].is_none());
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].count(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_failed_transfer_check_moves_nothing() {
        let mut slots = vec![Some(stack(Item::Stone, 10)), None];
        let mut pool = vec![stack(Item::Dirt, 3)];
        let new = vec![picture(Item::Stone, 4), picture(Item::Dirt, 3)];
        assert!(matches!(
            apply(&mut slots, &new, &mut pool, true),
            Settled::Violated { applied: false, .. }
        ));
        assert_eq!(slots[0].as_ref().map(ItemStack::count), Some(10));
        assert!(slots[1].is_none());
        assert_eq!((pool[0].item, pool[0].count()), (Item::Dirt, 3));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_balance_finds_what_is_short_and_spare() {
        let losses = [
            picture(Item::Stone, 5).unwrap(),
            picture(Item::Dirt, 2).unwrap(),
        ];
        let gains = [
            picture(Item::Stone, 3).unwrap(),
            picture(Item::Apple, 1).unwrap(),
        ];
        let balance = Balance::of(losses.iter(), gains.iter());
        assert_eq!(balance.short.len(), 1);
        assert_eq!(
            (balance.short[0].0.item, balance.short[0].1),
            (Item::Apple, 1)
        );
        assert_eq!(balance.spare.len(), 2);
        assert!(!balance.rearranges());
        assert!(Balance::of(gains.iter(), gains.iter()).is_even());
    }
}
