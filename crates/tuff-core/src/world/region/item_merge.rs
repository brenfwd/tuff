//! Dropped items merging with items in a neighbour region. The item due
//! to merge probes each neighbour its merge reach meets; the neighbour
//! answers with its own candidate, and the smaller stack gives its items
//! to the bigger one in one message, taking them out before sending.

use super::RegionActor;
use crate::world::experience;
use crate::world::items::{BorderMerge, ItemEntity};
use crate::world::messages::RegionInternal;
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

fn room(item: &ItemEntity) -> u8 {
    item.stack
        .max_stack_size()
        .min(64)
        .saturating_sub(item.stack.count)
}

impl RegionActor {
    fn item(&self, id: i32) -> Option<&ItemEntity> {
        self.ents
            .entities
            .get(&id)?
            .entity
            .downcast_ref::<ItemEntity>()
    }

    /// Asks every neighbour the merge reach of item `id` meets for an item
    /// to merge with.
    pub(super) fn probe_border_merges(&mut self, id: i32) {
        let Some(item) = self.item(id).filter(|item| item.mergeable()) else {
            return;
        };
        let reach = item.merge_reach();
        let (stack, owner) = (item.stack.clone(), item.base.extra.get("Owner").cloned());
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

    pub(super) fn border_merge(&mut self, step: BorderMerge) {
        match step {
            BorderMerge::Probe {
                from,
                item,
                stack,
                owner,
                reach,
            } => {
                let mut probe = ItemEntity::new(item, 0, stack, [0.0; 3], [0.0; 3], 0);
                if let Some(owner) = owner {
                    probe.base.extra.insert("Owner", owner);
                }
                let mut candidates: Vec<i32> = self
                    .ents
                    .entities
                    .values()
                    .filter_map(|tracked| tracked.entity.downcast_ref::<ItemEntity>())
                    .filter(|other| {
                        other.mergeable()
                            && self.pos.contains(other.base.chunk())
                            && experience::intersects(other.bounds(), reach)
                            && probe.merges_with(other)
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
                if probe.absorbs(other) {
                    let moved = room(&probe).min(other.stack.count);
                    let (pickup_delay, age, at) =
                        (other.pickup_delay, other.age, other.base.position);
                    let mut stack = other.stack.clone();
                    let left = stack.count - moved;
                    stack.count = moved;
                    if handle.internal(RegionInternal::BorderMerge(BorderMerge::Take {
                        item,
                        stack,
                        pickup_delay,
                        age,
                        at,
                    })) {
                        self.give_up(target, left);
                    }
                } else {
                    handle.internal(RegionInternal::BorderMerge(BorderMerge::Give {
                        to: self.pos,
                        item,
                        count: probe.stack.count,
                        target,
                        room: room(other),
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
                    .filter(|own| own.mergeable() && own.stack.count == count)
                else {
                    return;
                };
                let Some(handle) = self.border.neighbors.get(&to).cloned() else {
                    return;
                };
                let moved = room.min(own.stack.count);
                if moved == 0 {
                    return;
                }
                let (pickup_delay, age, at) = (own.pickup_delay, own.age, own.base.position);
                let mut stack = own.stack.clone();
                stack.count = moved;
                if handle.internal(RegionInternal::BorderMerge(BorderMerge::Take {
                    item: target,
                    stack,
                    pickup_delay,
                    age,
                    at,
                })) {
                    self.give_up(item, count - moved);
                }
            }
            BorderMerge::Take {
                item,
                stack,
                pickup_delay,
                age,
                at,
            } => {
                let fits = self
                    .item(item)
                    .is_some_and(|own| own.stack.stacks_with(&stack) && room(own) >= stack.count);
                if !fits {
                    self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
                    return;
                }
                let Some(own) = self
                    .ents
                    .entities
                    .get_mut(&item)
                    .and_then(|tracked| tracked.entity.downcast_mut::<ItemEntity>())
                else {
                    return;
                };
                own.pickup_delay = own.pickup_delay.max(pickup_delay);
                own.age = own.age.min(age);
                let mut merged = own.stack.clone();
                merged.count += stack.count;
                self.set_item_stack(item, merged);
            }
        }
    }

    /// Leaves item `id` holding `left` of its stack, removing it at 0.
    fn give_up(&mut self, id: i32, left: u8) {
        if left == 0 {
            self.remove_entity(id, &[]);
            return;
        }
        if let Some(mut stack) = self.item(id).map(|item| item.stack.clone()) {
            stack.count = left;
            self.set_item_stack(id, stack);
        }
    }
}
