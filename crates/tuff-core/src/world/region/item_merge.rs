//! Dropped items merging with items in a neighbour region. The item due
//! to merge probes each neighbour its merge reach meets; the neighbour
//! answers with its own candidate, and the smaller stack gives its items
//! to the bigger one in one message, taking them out before sending.

use super::RegionActor;
use crate::world::conservation::Transfer;
use crate::world::experience;
use crate::world::items::{BorderMerge, ItemEntity};
use crate::world::messages::{RegionHandle, RegionInternal};
use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{REGION_CHUNKS, RegionPos};

const REGION_BLOCKS: f64 = (REGION_CHUNKS * 16) as f64;

fn region_box(pos: RegionPos) -> [f64; 6] {
    let (x, z) = (pos.x as f64 * REGION_BLOCKS, pos.z as f64 * REGION_BLOCKS);
    [
        x,
        f64::NEG_INFINITY,
        z,
        x + REGION_BLOCKS,
        f64::INFINITY,
        z + REGION_BLOCKS,
    ]
}

fn room<M: tuff_item::item::Mode>(stack: &ItemStack<M>) -> u8 {
    stack.max_stack_size().min(64).saturating_sub(stack.count())
}

impl RegionActor {
    fn item(&self, id: i32) -> Option<&ItemEntity> {
        self.ents
            .entities
            .get(&id)?
            .entity
            .downcast_ref::<ItemEntity>()
    }

    fn item_mut(&mut self, id: i32) -> Option<&mut ItemEntity> {
        self.ents
            .entities
            .get_mut(&id)?
            .entity
            .downcast_mut::<ItemEntity>()
    }

    /// Asks every neighbour the merge reach of item `id` meets for an item
    /// to merge with.
    pub(super) fn probe_border_merges(&mut self, id: i32) {
        if self.quarantined() {
            return;
        }
        let Some(item) = self.item(id).filter(|item| item.mergeable()) else {
            return;
        };
        let reach = item.merge_reach();
        let (stack, owner) = (
            ItemSnapshot::of(&item.stack),
            item.base.extra.get("Owner").cloned(),
        );
        for neighbour in self.pos.neighbors() {
            if !experience::intersects(region_box(neighbour), reach) {
                continue;
            }
            if let Some(handle) = self.border.neighbors.get(&neighbour) {
                handle.internal(RegionInternal::BorderMerge(BorderMerge::Probe {
                    from: self.pos,
                    item: id,
                    stack: stack.clone(),
                    owner: owner.clone(),
                    reach,
                }));
            }
        }
    }

    /// Moves `moved` items out of item `from` to item `to` in the region
    /// behind `handle`. A refused message hands the items back, so both
    /// sides keep what they had.
    fn send_merge(&mut self, handle: &RegionHandle, from: i32, to: i32, moved: u8) {
        let Some(own) = self.item(from) else {
            return;
        };
        if moved == 0 {
            return;
        }
        let at = own.base.position;
        let pos = BlockPos::containing(at);
        if !self.may_transfer(Transfer::BorderMerge, pos, Some(handle.pos)) {
            return;
        }
        let own = self.item_mut(from).expect("looked up above");
        let (pickup_delay, age) = (own.pickup_delay, own.age);
        let before = u64::from(own.stack.count());
        let stack = own.stack.take(moved);
        let (after, sent) = (u64::from(own.stack.count()), u64::from(stack.count()));
        if sent != u64::from(moved)
            || !self.transfer_balanced(Transfer::BorderMerge, pos, before, 0, after, sent)
        {
            let own = self.item_mut(from).expect("taken from above");
            if let Err(stack) = own.stack.join(stack) {
                self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
            }
            return;
        }
        match handle.try_internal(RegionInternal::BorderMerge(BorderMerge::Take {
            item: to,
            stack,
            pickup_delay,
            age,
            at,
        })) {
            Ok(()) => {
                if self.item(from).is_some_and(|own| own.stack.is_empty()) {
                    self.remove_entity(from, &[]);
                } else {
                    self.refresh_item_stack(from);
                }
            }
            Err(refused) => {
                let RegionInternal::BorderMerge(BorderMerge::Take { stack, .. }) = *refused else {
                    unreachable!("the message just sent was a take");
                };
                let own = self.item_mut(from).expect("taken from above");
                if let Err(stack) = own.stack.join(stack) {
                    self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
                }
            }
        }
    }

    pub(super) fn border_merge(&mut self, step: BorderMerge) {
        match step {
            BorderMerge::Probe {
                from,
                item,
                stack,
                owner,
                reach,
            } => {
                let owner = owner.as_ref();
                let mut candidates: Vec<i32> = self
                    .ents
                    .entities
                    .values()
                    .filter_map(|tracked| tracked.entity.downcast_ref::<ItemEntity>())
                    .filter(|other| {
                        other.mergeable()
                            && self.pos.contains(other.base.chunk())
                            && experience::intersects(other.bounds(), reach)
                            && owner == other.base.extra.get("Owner")
                            && u32::from(stack.count()) + u32::from(other.stack.count())
                                <= u32::from(other.stack.max_stack_size())
                            && stack.stacks_with(&other.stack)
                    })
                    .map(|other| other.base.id)
                    .collect();
                candidates.sort_unstable();
                let Some(target) = candidates.first().copied() else {
                    return;
                };
                let Some(handle) = self.border.neighbors.get(&from).cloned() else {
                    return;
                };
                let other = self.item(target).expect("a candidate");
                if other.stack.count() < stack.count() {
                    let moved = room(&stack).min(other.stack.count());
                    self.send_merge(&handle, target, item, moved);
                } else {
                    handle.internal(RegionInternal::BorderMerge(BorderMerge::Give {
                        to: self.pos,
                        item,
                        count: stack.count(),
                        target,
                        room: room(&other.stack),
                    }));
                }
            }
            BorderMerge::Give {
                to,
                item,
                count,
                target,
                room,
            } => {
                let Some(own) = self
                    .item(item)
                    .filter(|own| own.mergeable() && own.stack.count() == count)
                else {
                    return;
                };
                let Some(handle) = self.border.neighbors.get(&to).cloned() else {
                    return;
                };
                let moved = room.min(own.stack.count());
                self.send_merge(&handle, item, target, moved);
            }
            BorderMerge::Take {
                item,
                stack,
                pickup_delay,
                age,
                at,
            } => {
                let pos = BlockPos::containing(at);
                let received = u64::from(stack.count());
                let Some(own) = self.item_mut(item) else {
                    self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
                    return;
                };
                let before = u64::from(own.stack.count());
                if !own.stack.stacks_with(&stack) || room(&own.stack) < stack.count() {
                    self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
                    return;
                }
                own.pickup_delay = own.pickup_delay.max(pickup_delay);
                own.age = own.age.min(age);
                let spilled = match own.stack.join(stack) {
                    Ok(()) => 0,
                    Err(stack) => {
                        let spilled = u64::from(stack.count());
                        self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
                        spilled
                    }
                };
                let after = self
                    .item(item)
                    .map_or(0, |own| u64::from(own.stack.count()));
                self.transfer_balanced(
                    Transfer::BorderMerge,
                    pos,
                    before,
                    received,
                    after,
                    spilled,
                );
                self.refresh_item_stack(item);
            }
        }
    }
}
