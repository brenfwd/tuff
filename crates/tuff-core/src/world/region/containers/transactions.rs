//! Transactions on block container menus: clicks a session worked out
//! settle here by moving stacks between the slots and the pool the
//! session sent.

use super::*;
use crate::world::menu::settle;
use crate::world::messages::holds;
use crate::world::region_api::{ContainerHolder, SlotEdit};

/// Slots a transaction moved, by container, with the edits to report.
type MovedSlots = Vec<(BlockPos, Vec<usize>, Vec<SlotEdit>)>;

impl RegionActor {
    pub(in crate::world::region) fn container_transaction(
        &mut self,
        player: PlayerHandle,
        window_id: i32,
        changes: Vec<ContainerChange>,
        mut pool: Vec<ItemStack>,
        took_result: bool,
    ) {
        let moved = if self.transaction_valid(player.id, &changes) && !self.quarantined() {
            self.move_container_changes(&player, &changes, &mut pool)
        } else {
            None
        };
        let Some(moved) = moved else {
            let contents = self.contents_of(&changes);
            self.answer_transaction(&player, window_id, false, contents, pool, &changes);
            return;
        };
        self.answer_transaction(&player, window_id, true, Vec::new(), pool, &changes);
        self.tell_moved_slots(moved);
        if !took_result {
            return;
        }
        let mut positions: Vec<BlockPos> = Vec::new();
        for change in &changes {
            if !positions.contains(&change.pos) {
                positions.push(change.pos);
            }
        }
        for pos in positions {
            let Some(used) = self
                .block_entity_mut(pos)
                .and_then(|entity| entity.furnace_mut())
                .map(|furnace| std::mem::take(&mut furnace.recipes_used))
            else {
                continue;
            };
            let at = self
                .ents
                .presence
                .get(&player.id)
                .map_or_else(|| pos.center(), |p| p.position);
            let awarded = self.pop_furnace_experience(&used, at);
            if !awarded.is_empty() {
                player.send(PlayerEvent::RecipesAwarded(awarded));
            }
        }
    }

    /// Hands the pool back to the player; if their session is gone, what
    /// it holds spills at the container.
    fn answer_transaction(
        &mut self,
        player: &PlayerHandle,
        window_id: i32,
        accepted: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemSnapshot>>)>,
        pool: Vec<ItemStack>,
        changes: &[ContainerChange],
    ) {
        let answer = PlayerEvent::Transaction {
            window_id,
            accepted,
            contents,
            pool,
        };
        if let Err(answer) = player.try_send(answer)
            && let PlayerEvent::Transaction { pool, .. } = *answer
        {
            self.spill_pool(changes.first().map(|change| change.pos), player.id, pool);
        }
    }

    /// Spills stacks no one can take back at `pos`, or where the player
    /// stands.
    pub(in crate::world::region) fn spill_pool(
        &mut self,
        pos: Option<BlockPos>,
        player: PlayerId,
        pool: Vec<ItemStack>,
    ) {
        let at = pos.map(BlockPos::center).or_else(|| {
            self.ents
                .presence
                .get(&player)
                .map(|presence| presence.position)
        });
        for stack in pool {
            match at {
                Some(at) => self.spawn_item_at(stack, at, [0.0; 3], items::BLOCK_DROP_PICKUP_DELAY),
                None => {
                    log::warn!("no place to spill {stack:?} of a container transaction");
                    stack.burn(tuff_item::item::BurnReason::Discarded);
                }
            }
        }
    }

    /// Whether `player` views every container `changes` touch, none of them
    /// is locked, every slot holds what the change expects, and nothing
    /// goes into a disabled crafter slot.
    fn transaction_valid(&self, player: PlayerId, changes: &[ContainerChange]) -> bool {
        changes.iter().enumerate().all(|(index, change)| {
            !self.menus.container_locks.contains_key(&change.pos)
                && !changes[..index]
                    .iter()
                    .any(|other| other.pos == change.pos && other.slot == change.slot)
                && !(change.new.is_some() && self.crafter_slot_disabled(change.pos, change.slot))
                && self
                    .menus
                    .viewers
                    .get(&change.pos)
                    .is_some_and(|viewers| viewers.iter().any(|v| v.id == player))
                && self
                    .block_entity(change.pos)
                    .and_then(|entity| entity.items.get(change.slot))
                    .is_some_and(|current| holds(current.as_ref(), change.expected.as_ref()))
        })
    }

    fn crafter_slot_disabled(&self, pos: BlockPos, slot: usize) -> bool {
        self.block_entity(pos)
            .and_then(|entity| entity.crafter())
            .is_some_and(|crafter| slot < 16 && crafter.disabled & (1 << slot) != 0)
    }

    /// Pictures of every container `changes` touch.
    fn contents_of(
        &self,
        changes: &[ContainerChange],
    ) -> Vec<(BlockPos, Vec<Option<ItemSnapshot>>)> {
        let mut positions: Vec<BlockPos> = changes.iter().map(|c| c.pos).collect();
        positions.sort_by_key(|p| (p.x, p.y, p.z));
        positions.dedup();
        positions
            .into_iter()
            .filter_map(|pos| {
                Some((
                    pos,
                    tuff_item::item::snapshot_slots(&self.block_entity(pos)?.items),
                ))
            })
            .collect()
    }

    /// Moves the stacks of validated `changes`: what the slots lose goes
    /// into `pool`, what they gain comes out of it. Nothing changes when
    /// the pool cannot cover the gains. Answers the slots moved, to tell
    /// their viewers with [`Self::tell_moved_slots`].
    fn move_container_changes(
        &mut self,
        player: &PlayerHandle,
        changes: &[ContainerChange],
        pool: &mut Vec<ItemStack>,
    ) -> Option<MovedSlots> {
        let mut slots: Vec<Option<ItemStack>> = changes
            .iter()
            .map(|change| {
                self.block_entity_mut(change.pos)
                    .and_then(|entity| entity.items.get_mut(change.slot))
                    .and_then(Option::take)
            })
            .collect();
        let new: Vec<Option<ItemSnapshot>> = changes.iter().map(|c| c.new.clone()).collect();
        let refuse = self.shared.conservation.take_injected_failure();
        let applied = match settle::apply(&mut slots, &new, pool, refuse) {
            settle::Settled::Applied => true,
            settle::Settled::Refused => false,
            settle::Settled::Violated { applied, detail } => {
                let pos = changes.first().map_or(BlockPos::new(0, 0, 0), |c| c.pos);
                let item = changes
                    .iter()
                    .find_map(|c| c.new.clone().or(c.expected.clone()));
                self.item_violation(
                    Some(player),
                    crate::world::conservation::ItemViolation {
                        operation: "container transaction",
                        player: Some(player.id),
                        pos,
                        item,
                        detail,
                    },
                );
                applied
            }
        };
        if !applied {
            for (change, stack) in changes.iter().zip(slots) {
                if let Some(slot) = self
                    .block_entity_mut(change.pos)
                    .and_then(|entity| entity.items.get_mut(change.slot))
                {
                    *slot = stack;
                }
            }
            return None;
        }
        Some(self.store_moved_slots(changes, slots))
    }

    /// Puts the settled stacks of `changes` into their slots, restarting a
    /// furnace whose input became another item.
    fn store_moved_slots(
        &mut self,
        changes: &[ContainerChange],
        slots: Vec<Option<ItemStack>>,
    ) -> MovedSlots {
        let observed = self.observes_containers();
        let mut by_pos: MovedSlots = Vec::new();
        let mut restart = Vec::new();
        for (change, stack) in changes.iter().zip(slots) {
            let edit = observed
                .then(|| SlotEdit::of(change.slot, change.expected.as_deref(), stack.as_ref()));
            let same = match (&change.expected, &stack) {
                (Some(old), Some(new)) => old.stacks_with(new),
                _ => false,
            };
            let Some(entity) = self.block_entity_mut(change.pos) else {
                if let Some(stack) = stack {
                    self.spill_stack(change.pos, stack);
                }
                continue;
            };
            if entity.kind.is_furnace() && change.slot == furnace::INPUT && !same {
                restart.push(change.pos);
            }
            if let Some(slot) = entity.items.get_mut(change.slot) {
                *slot = stack;
            }
            match by_pos.iter_mut().find(|(pos, ..)| *pos == change.pos) {
                Some((_, slots, edits)) => {
                    slots.push(change.slot);
                    edits.extend(edit);
                }
                None => by_pos.push((change.pos, vec![change.slot], edit.into_iter().collect())),
            }
        }
        for pos in restart {
            self.furnace_input_changed(pos);
        }
        by_pos
    }

    /// Tells the hooks and viewers about slots a transaction moved.
    fn tell_moved_slots(&mut self, moved: MovedSlots) {
        for (pos, slots, edits) in moved {
            if !edits.is_empty() {
                self.report_container_edit(ContainerHolder::Block(pos), edits);
            }
            self.broadcast_slots(pos, &slots);
        }
    }

    /// The first phase of a transaction over both halves of a double chest
    /// split between regions: validate this region's share, lock it and
    /// take out what its slots lose.
    pub(in crate::world::region) fn prepare_transaction(
        &mut self,
        player: PlayerHandle,
        window_id: i32,
        txn: u64,
        changes: Vec<ContainerChange>,
    ) {
        let prepared = self.transaction_valid(player.id, &changes);
        let contents = self.contents_of(&changes);
        let mut taken = Vec::new();
        if prepared {
            for change in &changes {
                self.menus
                    .container_locks
                    .insert(change.pos, (player.id, txn));
                let (lose, _) = settle::delta(change.expected.as_deref(), change.new.as_deref());
                if let Some(stack) = self
                    .block_entity_mut(change.pos)
                    .and_then(|entity| entity.items.get_mut(change.slot))
                    .and_then(|slot| ItemStack::take_from(slot, lose))
                {
                    taken.push(stack);
                }
            }
            self.menus.prepared.insert((player.id, txn), changes);
        }
        let vote = PlayerEvent::TransactionPrepared {
            window_id,
            txn,
            prepared,
            contents,
            taken,
        };
        if let Err(vote) = player.try_send(vote)
            && let PlayerEvent::TransactionPrepared { taken, .. } = *vote
        {
            self.resolve_transaction(player.id, txn, false, taken);
        }
    }

    fn spill_orphans(&mut self, orphans: Vec<(BlockPos, Vec<Option<ItemStack>>)>) {
        for (pos, items) in orphans {
            let drops = block_entity::scatter(pos, items.into_iter().flatten(), &mut self.rng);
            for (stack, position, velocity) in drops {
                self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
            }
        }
    }

    /// The second phase: `commit` fills the slots of a prepared share with
    /// `items`, else `items` (what the share took out) go back. Either way
    /// the share unlocks. Containers broken in between spill their
    /// contents with the share applied (or not), so a commit neither
    /// duplicates nor loses items.
    pub(in crate::world::region) fn resolve_transaction(
        &mut self,
        player: PlayerId,
        txn: u64,
        commit: bool,
        mut items: Vec<ItemStack>,
    ) {
        let Some(changes) = self.menus.prepared.remove(&(player, txn)) else {
            self.spill_pool(None, player, items);
            return;
        };
        self.menus
            .container_locks
            .retain(|_, lock| *lock != (player, txn));
        let mut orphans = self
            .menus
            .orphans
            .remove(&(player, txn))
            .unwrap_or_default();
        let observed = self.observes_containers();
        let mut by_pos: Vec<(BlockPos, Vec<usize>, Vec<SlotEdit>)> = Vec::new();
        for change in &changes {
            let (lose, gain) = settle::delta(change.expected.as_deref(), change.new.as_deref());
            let slot = match orphans.iter_mut().find(|(pos, _)| *pos == change.pos) {
                Some((_, slots)) => slots.get_mut(change.slot),
                None => self
                    .block_entity_mut(change.pos)
                    .and_then(|entity| entity.items.get_mut(change.slot)),
            };
            let Some(slot) = slot else {
                continue;
            };
            if commit {
                if let Some(gain) = gain {
                    settle::gain(slot, &gain, &mut items);
                }
            } else if lose > 0
                && let Some(expected) = &change.expected
            {
                let wanted = expected.clone().with_count(lose);
                for stack in settle::take_out(&mut items, &wanted) {
                    ItemStack::put_back(slot, stack);
                }
            }
            let edit = (commit && observed)
                .then(|| SlotEdit::of(change.slot, change.expected.as_deref(), slot.as_ref()));
            match by_pos.iter_mut().find(|(pos, ..)| *pos == change.pos) {
                Some((_, slots, edits)) => {
                    slots.push(change.slot);
                    edits.extend(edit);
                }
                None => by_pos.push((change.pos, vec![change.slot], edit.into_iter().collect())),
            }
        }
        for (pos, slots, edits) in by_pos {
            if orphans.iter().any(|(orphan, _)| *orphan == pos) {
                continue;
            }
            if !edits.is_empty() {
                self.report_container_edit(ContainerHolder::Block(pos), edits);
            }
            self.broadcast_slots(pos, &slots);
        }
        if !items.is_empty() {
            self.spill_pool(changes.first().map(|change| change.pos), player, items);
        }
        self.spill_orphans(orphans);
    }
}
