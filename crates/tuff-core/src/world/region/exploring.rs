use tokio::sync::oneshot;

use super::RegionActor;
use crate::world::drops::EXPLORATION_PENDING;
use crate::world::region_api::RegionWorld;
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;

#[derive(Clone, Copy)]
pub(super) enum ExploreTarget {
    Block(BlockPos),
    Entity(i32),
}

pub(super) struct Exploring {
    target: ExploreTarget,
    slot: usize,
    answer: oneshot::Receiver<Option<ItemStack>>,
}

fn waiting(stack: Option<&ItemStack>) -> bool {
    stack.is_some_and(|stack| stack.components.other.get(EXPLORATION_PENDING).is_some())
}

impl RegionActor {
    /// Starts finishing the explorer maps among freshly rolled loot.
    pub(super) fn explore_loot(
        &mut self,
        target: ExploreTarget,
        origin: BlockPos,
        items: &[Option<ItemStack>],
    ) {
        let loot = self.shared.block_loot();
        for (slot, stack) in items.iter().enumerate() {
            let Some(stack) = stack.as_ref().filter(|_| waiting(stack.as_ref())) else {
                continue;
            };
            if let Some(answer) = loot.explore(&self.shared, self.dimension, origin, stack) {
                self.ents.exploring.push(Exploring {
                    target,
                    slot,
                    answer,
                });
            }
        }
    }

    /// Writes maps that came back into the slots waiting for them; a slot
    /// whose structure was not found is emptied.
    pub(super) fn poll_exploring(&mut self) {
        if self.ents.exploring.is_empty() {
            return;
        }
        let mut done = Vec::new();
        self.ents
            .exploring
            .retain_mut(|job| match job.answer.try_recv() {
                Err(oneshot::error::TryRecvError::Empty) => true,
                Ok(found) => {
                    done.push((job.target, job.slot, found));
                    false
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    done.push((job.target, job.slot, None));
                    false
                }
            });
        for (target, slot, found) in done {
            match target {
                ExploreTarget::Block(pos) => {
                    let still_waiting = self
                        .block_entity(pos)
                        .and_then(|entity| entity.items.get(slot))
                        .is_some_and(|stack| waiting(stack.as_ref()));
                    if still_waiting {
                        let replaced = self.set_container_slot(pos, slot, found);
                        if let Some(replaced) = replaced {
                            replaced.burn(tuff_item::item::BurnReason::Transform);
                        }
                        self.block_entity_changed(pos);
                        self.broadcast_slots(pos, &[slot]);
                    }
                }
                ExploreTarget::Entity(id) => {
                    let Some(container) = self
                        .ents
                        .entities
                        .get_mut(&id)
                        .and_then(|tracked| tracked.entity.container_mut())
                    else {
                        continue;
                    };
                    if container.get(slot).is_some_and(|s| waiting(s.as_ref())) {
                        if let Some(replaced) = std::mem::replace(&mut container[slot], found) {
                            replaced.burn(tuff_item::item::BurnReason::Transform);
                        }
                        self.entity_slots_changed(id, &[slot], None);
                    }
                }
            }
        }
    }
}
