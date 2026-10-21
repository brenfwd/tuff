//! Double chests whose halves belong to different regions. The owner of a
//! half pushes a snapshot of it to the region next door, which reads the
//! joined chest from the pair; changes to the far half travel back as
//! messages the owner applies, so no region ever awaits another.

use std::collections::HashMap;
use tuff_item::item::DupUnchecked;

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Facing, Type as ChestType};

use super::{Column, RegionActor, block_at};
use crate::world::block_entity::{self, ContainerKind};
use crate::world::messages::{RegionHandle, RegionInternal};
use crate::world::placement;
use tuff_item::item::{ItemStack, SlotsCow};
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
    let mut joined = Vec::with_capacity(own.len() + other.len());
    if state.r#type() == Some(ChestType::Left) {
        joined.extend(other.iter().map(DupUnchecked::dup_unchecked));
        joined.extend(own.iter().map(DupUnchecked::dup_unchecked));
    } else {
        joined.extend(own.iter().map(DupUnchecked::dup_unchecked));
        joined.extend(other.iter().map(DupUnchecked::dup_unchecked));
    }
    Some(SlotsCow::Owned(joined))
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
                    let items = entity.items.dup_unchecked();
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

    pub(super) fn half_snapshot(&mut self, pos: BlockPos, items: Option<Vec<Option<ItemStack>>>) {
        self.sim.redstone.far_halves.insert(pos, items);
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
                    .map(|entity| entity.items.dup_unchecked())
            })
            .flatten();
        reply.internal(RegionInternal::HalfSnapshot { pos, items });
    }

    fn half_kind(&self, pos: BlockPos) -> Option<ContainerKind> {
        ContainerKind::of_block(self.block(pos)?.block())
    }

    pub(super) fn half_inserted(&mut self, pos: BlockPos, face: Option<Facing>, stack: ItemStack) {
        let rest = match (self.half_kind(pos), self.owns(pos)) {
            (Some(kind), true) if self.block_entity(pos).is_some() => {
                self.insert_into(kind, &[pos], face, stack, None)
            }
            _ => Some(stack),
        };
        if let Some(rest) = rest {
            self.spill_stack(pos, rest);
        }
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
        stack: ItemStack,
        golem: i32,
        at: BlockPos,
        reply: RegionHandle,
    ) {
        if !self.owns(pos) {
            return;
        }
        let Some((slot, taken)) = self.drain_local(pos, &stack) else {
            return;
        };
        if !reply.internal(RegionInternal::HalfDrained {
            golem,
            at,
            stack: taken.dup_unchecked(),
        }) {
            self.undrain(pos, slot, taken);
        }
    }

    /// Puts back what a drain removed when its taker cannot be reached.
    pub(super) fn undrain(&mut self, pos: BlockPos, slot: usize, stack: ItemStack) {
        let current = self
            .block_entity(pos)
            .and_then(|entity| entity.items.get(slot).map(DupUnchecked::dup_unchecked))
            .flatten();
        match current {
            None => {
                self.write_container_slot(pos, slot, Some(stack.dup_unchecked()));
                self.broadcast_slots(pos, vec![(slot, Some(stack))]);
            }
            Some(mut held) if held.stacks_with(&stack) => match held.join(stack) {
                Ok(()) => {
                    self.write_container_slot(pos, slot, Some(held.dup_unchecked()));
                    self.broadcast_slots(pos, vec![(slot, Some(held))]);
                }
                Err(stack) => self.spill_stack(pos, stack),
            },
            Some(_) => self.spill_stack(pos, stack),
        }
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
        if !kind.can_extract(slot, stack, face) {
            return;
        }
        let one = stack.dup_count_unchecked(1);
        self.take_one(pos, slot);
        if !reply.internal(RegionInternal::HalfPulled {
            hopper,
            stack: one.dup_unchecked(),
        }) {
            self.undrain(pos, slot, one);
        }
    }

    pub(super) fn half_arrived(&mut self, hopper: BlockPos, stack: ItemStack) {
        let rest = if self.owns(hopper) && self.block_entity(hopper).is_some() {
            self.insert_into(ContainerKind::Hopper, &[hopper], None, stack, None)
        } else {
            Some(stack)
        };
        if let Some(rest) = rest {
            self.spill_stack(hopper, rest);
        }
    }

    /// Whether `stack` would fit into the part of a container at `part`.
    pub(super) fn part_accepts(
        &self,
        kind: ContainerKind,
        part: BlockPos,
        face: Option<Facing>,
        stack: &ItemStack,
    ) -> bool {
        let Some(items) = self.part_items(part) else {
            return false;
        };
        let mut items = items
            .iter()
            .map(DupUnchecked::dup_unchecked)
            .collect::<Vec<_>>();
        let mut changed = Vec::new();
        let disabled = self
            .block_entity(part)
            .and_then(|entity| entity.crafter())
            .map_or(0, |crafter| crafter.disabled);
        block_entity::insert(
            &self.shared.recipes(),
            kind,
            &mut items,
            face,
            stack.dup_unchecked(),
            &mut changed,
            disabled,
        )
        .is_none()
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
                    held.set_count_unchecked(held.count().saturating_sub(stack.count()));
                    if held.count() == 0 {
                        items[slot] = None;
                    }
                }
                neighbour.internal(RegionInternal::HalfDrain {
                    pos: *part,
                    stack: stack.dup_unchecked(),
                    golem,
                    at,
                    reply: self.handle.clone(),
                    cause: self.sim.cause,
                });
            }
            return;
        }
    }

    /// Inserts into a far half from its snapshot and tells the owner;
    /// returns what did not fit.
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
        let Some(Some(items)) = self.sim.redstone.far_halves.get(&part) else {
            return Some(stack);
        };
        let mut items = items.dup_unchecked();
        let before = stack.count();
        let mut changed = Vec::new();
        let rest = block_entity::insert(
            &self.shared.recipes(),
            kind,
            &mut items,
            face,
            stack.dup_unchecked(),
            &mut changed,
            0,
        );
        let inserted = before - rest.as_ref().map_or(0, |rest| rest.count());
        if inserted == 0 {
            return rest;
        }
        if !neighbour.internal(RegionInternal::HalfInsert {
            pos: part,
            face,
            stack: stack.dup_count_unchecked(inserted),
            cause: self.sim.cause,
        }) {
            return Some(stack);
        }
        self.sim.redstone.far_halves.insert(part, Some(items));
        rest
    }

    /// Pulls one item from `slot` of the far half into the hopper.
    pub(super) fn pull_far(&mut self, part: BlockPos, slot: usize, hopper: BlockPos) {
        if let Some(Some(items)) = self.sim.redstone.far_halves.get_mut(&part)
            && let Some(stack) = items[slot].as_mut()
        {
            stack.set_count_unchecked(stack.count() - 1);
            if stack.count() == 0 {
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
