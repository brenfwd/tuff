//! Double chests whose halves belong to different regions. The owner of a
//! half pushes a snapshot of it to the region next door, which reads the
//! joined chest from the pair; changes to the far half travel back as
//! messages the owner applies, so no region ever awaits another.

use std::collections::HashMap;

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Facing, Type as ChestType};

use super::{Column, RegionActor, block_at};
use crate::world::block_entity::{self, ContainerKind};
use crate::world::conservation::Transfer;
use crate::world::messages::{RegionHandle, RegionInternal};
use crate::world::placement;
use tuff_item::item::{BurnReason, ItemSnapshot, ItemStack, MintReason, SlotsCow};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::{ChunkMap, ChunkPos};

pub(super) type Snapshots = HashMap<BlockPos, Option<Vec<Option<ItemStack>>>>;

fn verified_partner(
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    pos: BlockPos,
    state: BlockState,
) -> Option<BlockPos> {
    let kind = ContainerKind::of_block(state.block()).filter(|kind| kind.pairs())?;
    placement::chest_partner(state, pos).filter(|partner| {
        block(*partner).is_some_and(|other| {
            ContainerKind::of_block(other.block()) == Some(kind)
                && other.facing() == state.facing()
                && other.r#type().is_some_and(|t| t != ChestType::Single)
                && placement::chest_partner(other, *partner) == Some(pos)
        })
    })
}

/// The slots of the container at `pos` as a read-only view: both halves of
/// a double chest in menu order when the far half's snapshot is known,
/// else just this block entity's own.
pub(super) fn merged_view<'a>(
    columns: &'a ChunkMap<Column>,
    halo: &ChunkMap<Chunk>,
    far: &'a Snapshots,
    pos: BlockPos,
) -> Option<SlotsCow<'a>> {
    let own = columns
        .get(&ChunkPos::of_block(pos))?
        .meta
        .block_entities
        .get(&pos)?
        .items
        .as_slice();
    let Some(state) = block_at(columns, halo, pos) else {
        return Some(SlotsCow::Borrowed(own));
    };
    let block = |p| block_at(columns, halo, p);
    let Some(partner) = verified_partner(&block, pos, state) else {
        return Some(SlotsCow::Borrowed(own));
    };
    let other = match columns.get(&ChunkPos::of_block(partner)) {
        Some(column) => column.meta.block_entities.get(&partner).map(|e| &e.items),
        None => far.get(&partner).and_then(Option::as_ref),
    };
    let Some(other) = other else {
        return Some(SlotsCow::Borrowed(own));
    };
    if state.r#type() == Some(ChestType::Left) {
        Some(SlotsCow::Joined(other, own))
    } else {
        Some(SlotsCow::Joined(own, other))
    }
}

impl RegionActor {
    /// The block entities behind the container at `pos` as hoppers, golems
    /// and comparators see it: like [`Self::container_parts`], but a far
    /// half counts once its snapshot has arrived (asking for it if not).
    pub(super) fn joined_parts(&self, pos: BlockPos, state: BlockState) -> Vec<BlockPos> {
        let Some(kind) = ContainerKind::of_block(state.block()) else {
            return Vec::new();
        };
        if !kind.pairs() {
            return vec![pos];
        }
        let block = |p| self.block(p);
        let Some(partner) = verified_partner(&block, pos, state) else {
            return vec![pos];
        };
        if !self.owns(partner) {
            match self.sim.redstone.far_halves.get(&partner) {
                Some(Some(_)) => {}
                Some(None) => return vec![pos],
                None => {
                    self.request_half(partner);
                    return vec![pos];
                }
            }
        }
        if state.r#type() == Some(ChestType::Left) {
            vec![partner, pos]
        } else {
            vec![pos, partner]
        }
    }

    fn request_half(&self, partner: BlockPos) {
        if let Some(neighbour) = self.border.region_of_neighbour(partner) {
            neighbour.internal(RegionInternal::HalfRequest {
                pos: partner,
                reply: self.handle.clone(),
            });
        }
    }

    /// The slots of a part of a container: its block entity here, or the
    /// snapshot of a far half.
    pub(super) fn part_items(&self, part: BlockPos) -> Option<&[Option<ItemStack>]> {
        if self.owns(part) {
            self.block_entity(part)
                .map(|entity| entity.items.as_slice())
        } else {
            self.sim
                .redstone
                .far_halves
                .get(&part)?
                .as_ref()
                .map(Vec::as_slice)
        }
    }

    fn horizontal_neighbours(pos: BlockPos) -> [BlockPos; 4] {
        [
            BlockPos::new(pos.x + 1, pos.y, pos.z),
            BlockPos::new(pos.x - 1, pos.y, pos.z),
            BlockPos::new(pos.x, pos.y, pos.z + 1),
            BlockPos::new(pos.x, pos.y, pos.z - 1),
        ]
    }

    /// Pushes the contents of changed chest halves to the region owning
    /// their other half.
    pub(super) fn share_halves(&mut self, changed: &[BlockPos]) {
        for pos in changed {
            if !self.owns(*pos) {
                continue;
            }
            let state = self.block(*pos);
            let partner = state.and_then(|state| {
                let block = |p| self.block(p);
                verified_partner(&block, *pos, state)
            });
            match partner.filter(|partner| !self.owns(*partner)) {
                Some(partner) => {
                    let Some(entity) = self.block_entity(*pos) else {
                        continue;
                    };
                    let items = tuff_item::item::snapshot_slots(&entity.items);
                    if let Some(neighbour) = self.border.region_of_neighbour(partner) {
                        neighbour.internal(RegionInternal::HalfSnapshot {
                            pos: *pos,
                            items: Some(items),
                        });
                        self.sim.redstone.half_sent.insert(*pos);
                    }
                }
                None => {
                    if self.sim.redstone.half_sent.remove(pos) {
                        let mut told = Vec::new();
                        for other in Self::horizontal_neighbours(*pos) {
                            let region = ChunkPos::of_block(other).region();
                            if region != self.pos && !told.contains(&region) {
                                told.push(region);
                                if let Some(neighbour) = self.border.neighbors.get(&region) {
                                    neighbour.internal(RegionInternal::HalfSnapshot {
                                        pos: *pos,
                                        items: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Keeps a far half's pictured contents as template stacks hoppers,
    /// golems and comparators here read; they are never real items.
    pub(super) fn half_snapshot(
        &mut self,
        pos: BlockPos,
        items: Option<Vec<Option<ItemSnapshot>>>,
    ) {
        let mirror = items.map(|items| {
            items
                .iter()
                .map(|slot| {
                    slot.as_ref().map(|shown| {
                        ItemStack::mint_like(shown, shown.count(), MintReason::Template)
                    })
                })
                .collect()
        });
        self.sim.redstone.far_halves.insert(pos, mirror);
        for near in Self::horizontal_neighbours(pos) {
            if self.owns(near) {
                self.sim.redstone.container_changes.insert(near);
            }
        }
    }

    pub(super) fn half_requested(&mut self, pos: BlockPos, reply: RegionHandle) {
        let items = self
            .owns(pos)
            .then(|| {
                self.block_entity(pos)
                    .map(|entity| tuff_item::item::snapshot_slots(&entity.items))
            })
            .flatten();
        reply.internal(RegionInternal::HalfSnapshot { pos, items });
    }

    fn half_kind(&self, pos: BlockPos) -> Option<ContainerKind> {
        ContainerKind::of_block(self.block(pos)?.block())
    }

    pub(super) fn half_inserted(&mut self, pos: BlockPos, face: Option<Facing>, stack: ItemStack) {
        let (before, received) = (self.held_at(pos), u64::from(stack.count()));
        let rest = match (self.half_kind(pos), self.owns(pos)) {
            (Some(kind), true) if self.block_entity(pos).is_some() => {
                self.insert_into(kind, &[pos], face, stack, None)
            }
            _ => Some(stack),
        };
        let spilled = rest.as_ref().map_or(0, |rest| u64::from(rest.count()));
        if let Some(rest) = rest {
            self.spill_stack(pos, rest);
        }
        let after = self.held_at(pos);
        self.transfer_balanced(Transfer::Hopper, pos, before, received, after, spilled);
    }

    pub(super) fn spill_stack(&mut self, pos: BlockPos, stack: ItemStack) {
        let at = pos.center();
        self.spawn_item_at(
            stack,
            at,
            [0.0; 3],
            crate::world::items::BLOCK_DROP_PICKUP_DELAY,
        );
    }

    pub(super) fn golem_drain_message(&mut self, message: RegionInternal) {
        match message {
            RegionInternal::HalfDrain {
                pos,
                stack,
                golem,
                at,
                reply,
                cause,
            } => self.with_cause(cause, |region| {
                region.half_drained(pos, stack, golem, at, reply)
            }),
            RegionInternal::HalfDrained { golem, at, stack } => {
                self.golem_received(golem, at, stack)
            }
            _ => {}
        }
    }

    pub(super) fn half_drained(
        &mut self,
        pos: BlockPos,
        stack: ItemSnapshot,
        golem: i32,
        at: BlockPos,
        reply: RegionHandle,
    ) {
        if !self.owns(pos) || !self.may_transfer(Transfer::Hopper, pos, Some(reply.pos)) {
            return;
        }
        let Some((slot, taken)) = self.drain_local(pos, &stack) else {
            return;
        };
        if let Err(message) = reply.try_internal(RegionInternal::HalfDrained {
            golem,
            at,
            stack: taken,
        }) && let RegionInternal::HalfDrained { stack, .. } = *message
        {
            self.undrain(pos, slot, stack);
        }
    }

    /// Puts back what a drain removed when its taker cannot be reached.
    pub(super) fn undrain(&mut self, pos: BlockPos, slot: usize, stack: ItemStack) {
        let observed = self.observes_containers();
        let Some(held) = self
            .block_entity_mut(pos)
            .and_then(|entity| entity.items.get_mut(slot))
        else {
            self.spill_stack(pos, stack);
            return;
        };
        let old = observed.then(|| held.as_ref().map(ItemSnapshot::of));
        let rest = match held {
            None => {
                *held = Some(stack);
                None
            }
            Some(held) => held.join(stack).err(),
        };
        if let Some(rest) = rest {
            self.spill_stack(pos, rest);
            return;
        }
        if let Some(old) = old {
            self.report_slot(pos, slot, old);
        }
        self.broadcast_slots(pos, &[slot]);
    }

    pub(super) fn half_pulled_from(
        &mut self,
        pos: BlockPos,
        slot: usize,
        hopper: BlockPos,
        reply: RegionHandle,
    ) {
        if !self.owns(pos) || self.menus.container_locks.contains_key(&pos) {
            return;
        }
        let Some(kind) = self.half_kind(pos) else {
            return;
        };
        let face = Some(Facing::Down);
        let Some(stack) = self
            .block_entity(pos)
            .and_then(|entity| entity.items.get(slot)?.as_ref())
        else {
            return;
        };
        if !kind.can_extract(slot, stack, face)
            || !self.may_transfer(Transfer::Hopper, pos, Some(reply.pos))
        {
            return;
        }
        let Some(one) = self.take_one_out(pos, slot) else {
            return;
        };
        let shown = self.observes_containers().then(|| ItemSnapshot::of(&one));
        match reply.try_internal(RegionInternal::HalfPulled { hopper, stack: one }) {
            Ok(()) => self.took_one(pos, slot, shown.as_deref()),
            Err(message) => {
                if let RegionInternal::HalfPulled { stack, .. } = *message {
                    self.return_one(pos, slot, stack);
                }
            }
        }
    }

    pub(super) fn half_arrived(&mut self, hopper: BlockPos, stack: ItemStack) {
        let (before, received) = (self.held_at(hopper), u64::from(stack.count()));
        let rest = if self.owns(hopper) && self.block_entity(hopper).is_some() {
            self.insert_into(ContainerKind::Hopper, &[hopper], None, stack, None)
        } else {
            Some(stack)
        };
        let spilled = rest.as_ref().map_or(0, |rest| u64::from(rest.count()));
        if let Some(rest) = rest {
            self.spill_stack(hopper, rest);
        }
        let after = self.held_at(hopper);
        self.transfer_balanced(Transfer::Hopper, hopper, before, received, after, spilled);
    }

    /// Whether `stack` would fit into the part of a container at `part`.
    pub(super) fn part_accepts(
        &self,
        kind: ContainerKind,
        part: BlockPos,
        face: Option<Facing>,
        stack: &ItemStack,
        count: u8,
    ) -> bool {
        let Some(items) = self.part_items(part) else {
            return false;
        };
        let disabled = self
            .block_entity(part)
            .and_then(|entity| entity.crafter())
            .map_or(0, |crafter| crafter.disabled);
        block_entity::insert_room(
            &self.shared.recipes(),
            kind,
            items,
            face,
            stack,
            count,
            disabled,
        ) == count
    }

    /// A copper golem's take from the parts of a joined container: the
    /// first slot holding `stack` (with its components) gives up to
    /// `stack.count`, and the golem is handed only what was removed.
    pub(super) fn take_joined(
        &mut self,
        parts: &[BlockPos],
        stack: &ItemStack,
        golem: i32,
        at: BlockPos,
    ) {
        for part in parts {
            let Some(slots) = self.part_items(*part) else {
                continue;
            };
            let Some(slot) = slots
                .iter()
                .position(|s| s.as_ref().is_some_and(|s| s.stacks_with(stack)))
            else {
                continue;
            };
            if self.owns(*part) {
                if let Some((_, taken)) = self.drain_local(*part, stack) {
                    self.ents.taken_stacks.push((golem, taken));
                }
            } else if let Some(neighbour) = self.border.region_of_neighbour(*part) {
                if let Some(Some(items)) = self.sim.redstone.far_halves.get_mut(part)
                    && let Some(held) = items[slot].as_mut()
                {
                    let left = held.count().saturating_sub(stack.count());
                    held.resize(left, MintReason::Template, BurnReason::Template);
                    if left == 0 {
                        items[slot] = None;
                    }
                }
                neighbour.internal(RegionInternal::HalfDrain {
                    pos: *part,
                    stack: ItemSnapshot::of(stack),
                    golem,
                    at,
                    reply: self.handle.clone(),
                    cause: self.sim.cause,
                });
            }
            return;
        }
    }

    /// Inserts into a far half as its snapshot shows it and sends the
    /// owner what goes in; returns what did not fit.
    pub(super) fn insert_far(
        &mut self,
        kind: ContainerKind,
        part: BlockPos,
        face: Option<Facing>,
        stack: ItemStack,
    ) -> Option<ItemStack> {
        let Some(neighbour) = self.border.region_of_neighbour(part).cloned() else {
            return Some(stack);
        };
        if !self.may_transfer(Transfer::Hopper, part, Some(neighbour.pos)) {
            return Some(stack);
        }
        let Some(Some(items)) = self.sim.redstone.far_halves.get(&part) else {
            return Some(stack);
        };
        let recipes = self.shared.recipes();
        let inserted =
            block_entity::insert_room(&recipes, kind, items, face, &stack, stack.count(), 0);
        if inserted == 0 {
            return Some(stack);
        }
        let (moving, rest) = stack.split(inserted);
        let shown = ItemStack::mint_like(&moving, moving.count(), MintReason::Template);
        if let Err(message) = neighbour.try_internal(RegionInternal::HalfInsert {
            pos: part,
            face,
            stack: moving,
            cause: self.sim.cause,
        }) {
            shown.burn(BurnReason::Template);
            let RegionInternal::HalfInsert { stack: back, .. } = *message else {
                return rest;
            };
            return Some(match rest {
                None => back,
                Some(rest) => match ItemStack::merge(back, rest) {
                    Ok(merged) => merged,
                    Err((back, rest)) => {
                        self.spill_stack(part, rest);
                        back
                    }
                },
            });
        }
        if let Some(Some(items)) = self.sim.redstone.far_halves.get_mut(&part) {
            let mut changed = Vec::new();
            if let Some(left) =
                block_entity::insert(&recipes, kind, items, face, shown, &mut changed, 0)
            {
                left.burn(BurnReason::Template);
            }
        }
        rest
    }

    /// Pulls one item from `slot` of the far half into the hopper.
    pub(super) fn pull_far(&mut self, part: BlockPos, slot: usize, hopper: BlockPos) {
        if let Some(Some(items)) = self.sim.redstone.far_halves.get_mut(&part)
            && let Some(stack) = items[slot].as_mut()
        {
            let left = stack.count().saturating_sub(1);
            stack.resize(left, MintReason::Template, BurnReason::Template);
            if left == 0 {
                items[slot] = None;
            }
        }
        if let Some(neighbour) = self.border.region_of_neighbour(part) {
            neighbour.internal(RegionInternal::HalfPull {
                pos: part,
                slot,
                hopper,
                reply: self.handle.clone(),
            });
        }
    }
}
