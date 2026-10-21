//! Block entities pistons move across a region border. The region owning
//! the block's old position lifts its block entity and sends it to the
//! region owning the destination ([`BlockUpdate::CarriedEntity`]), always
//! answering once it was expected so the destination never waits in vain.
//! The destination holds the landing until it arrives (for at most
//! [`CARRY_WAIT`] ticks), keeps one that arrives before the move starts,
//! and gives one that arrives after a bare landing to the landed block.
//! Nothing that cannot be placed is lost: its contents scatter.

use std::collections::HashMap;

use tuff_vanilla_data::BlockState;

use super::super::{AIR, ChunkPos, RegionActor};
use crate::world::block_entity::{self, raw::CARRIED};
use crate::world::messages::BlockUpdate;
use crate::world::placement::step;
use crate::world::redstone::MovingBlock;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

/// How long, in ticks, a destination waits for a carried block entity, and
/// keeps one that arrived early or after a bare landing.
pub(in super::super) const CARRY_WAIT: u64 = 20;

#[derive(Default)]
pub(in super::super) struct CarriedState {
    /// Moving blocks here whose block entity is on its way, with the tick
    /// they began waiting.
    awaiting: HashMap<BlockPos, u64>,
    /// Block entities that arrived before their block started moving here
    /// (`None`: the origin had none to send), with when.
    early: HashMap<BlockPos, (Option<Compound>, u64)>,
    /// Blocks that stopped moving before their block entity arrived.
    bare: HashMap<BlockPos, u64>,
    /// Blocks the block event running now started moving with their block
    /// entity, by destination, to show its viewers.
    pub(super) shown: Vec<(BlockPos, MovingBlock)>,
    pub(super) displays: Vec<super::display::CarriedDisplay>,
}

impl CarriedState {
    fn is_empty(&self) -> bool {
        self.awaiting.is_empty() && self.early.is_empty() && self.bare.is_empty()
    }
}

impl RegionActor {
    /// Whether a feature lets pistons move `state` with its block entity.
    pub(in super::super) fn carries(&self, state: BlockState) -> bool {
        state.block().block_entity().is_some()
            && self
                .shared
                .block_entity_carrier
                .as_ref()
                .is_some_and(|carrier| carrier.carries(state))
    }

    /// Takes the block entity at `from` out of the world to move it,
    /// closing its menus without spilling it.
    pub(super) fn lift_block_entity(&mut self, from: BlockPos) -> Option<Compound> {
        let column = self.columns.get_mut(&ChunkPos::of_block(from))?;
        let nbt = match column.meta.raw_block_entities.remove(&from) {
            Some(nbt) => nbt,
            None => {
                let entity = column.meta.block_entities.remove(&from)?;
                self.menus.ticking.remove(&from);
                self.menus.hopper_ticks.remove(&from);
                let nbt = entity.encode(from);
                tuff_item::item::conservation::release(entity);
                nbt
            }
        };
        column.blocks_dirty = true;
        column.version += 1;
        column.packet = None;
        self.block_entity_updates.retain(|at| *at != from);
        self.menus.sign_editors.remove(&from);
        self.sim.redstone.container_changes.insert(from);
        self.close_viewers(from);
        Some(nbt)
    }

    /// A piston is about to replace the block at `pos`, which it moves on
    /// to `dest`, having read `expected` there. When either carries a block
    /// entity and `dest` is not where this region lifts it as it moves,
    /// the block entity goes to `dest`: queued behind the piston's other
    /// updates while its event runs (`queued`), otherwise sent at once.
    pub(in super::super) fn carry_out(
        &mut self,
        pos: BlockPos,
        dest: Option<BlockPos>,
        expected: BlockState,
        queued: bool,
    ) {
        let Some(dest) = dest else {
            return;
        };
        if !self.owns(pos) {
            return;
        }
        let current = RegionActor::block(self, pos).unwrap_or(AIR);
        let lifts = self.carries(current);
        if !lifts && !self.carries(expected) {
            return;
        }
        let nbt = if lifts {
            self.lift_block_entity(pos)
        } else {
            None
        };
        if self.pos.contains(ChunkPos::of_block(dest)) {
            if nbt.is_some() {
                self.receive_carried_entity(dest, nbt);
            }
            return;
        }
        if !self
            .border
            .neighbors
            .contains_key(&ChunkPos::of_block(dest).region())
        {
            if let Some(nbt) = nbt {
                self.spill_carried(pos, &nbt);
            }
            return;
        }
        let update = BlockUpdate::CarriedEntity { pos: dest, nbt };
        if queued {
            self.queue_remote(dest, update);
        } else {
            self.send_block_updates(HashMap::from([(
                ChunkPos::of_block(dest).region(),
                vec![update],
            )]));
        }
    }

    /// The block entity of a block starting to move into `pos` from a
    /// neighbouring region: one that already arrived, or none yet, and
    /// then its landing waits for it.
    pub(super) fn carried_from_afar(
        &mut self,
        pos: BlockPos,
        moving: &MovingBlock,
    ) -> Option<Compound> {
        if moving.source || !self.carries(moving.moved) {
            return None;
        }
        let from = step(pos, moving.movement().opposite());
        if self.pos.contains(ChunkPos::of_block(from)) {
            return None;
        }
        if let Some((nbt, _)) = self.sim.redstone.carried.early.remove(&pos) {
            return nbt;
        }
        self.sim
            .redstone
            .carried
            .awaiting
            .insert(pos, self.sched.tick);
        None
    }

    /// The block moving into `pos` changed to what was really there: it
    /// waits for a block entity only if it now carries one.
    pub(super) fn carried_changed(&mut self, pos: BlockPos) {
        let Some(moving) = self.moving_at(pos) else {
            return;
        };
        let carried = self
            .raw_block_entity(pos)
            .is_some_and(|nbt| nbt.contains_key(CARRIED));
        if carried || !self.carries(moving.moved) {
            self.sim.redstone.carried.awaiting.remove(&pos);
        }
    }

    /// A block entity moved into `pos` from a neighbouring region (`None`:
    /// the block there had none after all).
    pub(in super::super) fn receive_carried_entity(
        &mut self,
        pos: BlockPos,
        nbt: Option<Compound>,
    ) {
        if self.moving_at(pos).is_some_and(|moving| !moving.source) {
            self.sim.redstone.carried.awaiting.remove(&pos);
            let Some(nbt) = nbt else {
                return;
            };
            match self.raw_block_entity_mut(pos) {
                Some(piston) if !piston.contains_key(CARRIED) => {
                    piston.insert(CARRIED, nbt);
                }
                _ => self.spill_carried(pos, &nbt),
            }
            return;
        }
        if self.sim.redstone.carried.bare.remove(&pos).is_some() {
            if let Some(nbt) = nbt {
                self.land_late(pos, nbt);
            }
            return;
        }
        let tick = self.sched.tick;
        if let Some((Some(old), _)) = self.sim.redstone.carried.early.insert(pos, (nbt, tick)) {
            self.spill_carried(pos, &old);
        }
    }

    /// Whether the block that finished moving through `pos` waits for its
    /// block entity before it lands.
    pub(super) fn holds_landing(&mut self, pos: BlockPos) -> bool {
        let Some(&since) = self.sim.redstone.carried.awaiting.get(&pos) else {
            return false;
        };
        if self.sched.tick.saturating_sub(since) < CARRY_WAIT {
            return true;
        }
        self.landed_bare(pos);
        false
    }

    /// The block moving through `pos` lands now, with no block entity yet
    /// if it is still on its way.
    pub(super) fn landed_bare(&mut self, pos: BlockPos) {
        let carried = &mut self.sim.redstone.carried;
        if carried.awaiting.remove(&pos).is_some() {
            carried.bare.insert(pos, self.sched.tick);
        }
    }

    /// Gives a block that landed bare the block entity it carried; what the
    /// bare one gathered meanwhile spills.
    fn land_late(&mut self, pos: BlockPos, nbt: Compound) {
        let Some(kind) = block_entity::raw::kind(&nbt) else {
            return;
        };
        let state = RegionActor::block(self, pos);
        if state.and_then(|state| state.block().block_entity()) != Some(kind) {
            self.spill_carried(pos, &nbt);
            return;
        }
        self.remove_block_entity(pos, state);
        self.land_carried(pos, Some(Compound::new().with(CARRIED, nbt)));
    }

    /// Scatters what a carried block entity held at `pos`.
    pub(super) fn spill_carried(&mut self, pos: BlockPos, nbt: &Compound) {
        let Some(kind) = block_entity::raw::kind(nbt) else {
            return;
        };
        if !self.owns(pos) {
            return;
        }
        let stacks = block_entity::raw::contents(kind, nbt);
        let drops = block_entity::scatter(pos, stacks, &mut self.rng);
        for (stack, position, velocity) in drops {
            self.spawn_item_at(
                stack,
                position,
                velocity,
                crate::world::items::BLOCK_DROP_PICKUP_DELAY,
            );
        }
    }

    /// Lets go of waits that ended: a waiting block no longer moving counts
    /// as landed bare, and block entities no move claimed in time spill.
    pub(super) fn expire_carried(&mut self) {
        if self.sim.redstone.carried.is_empty() {
            return;
        }
        let tick = self.sched.tick;
        let gone: Vec<BlockPos> = self
            .sim
            .redstone
            .carried
            .awaiting
            .keys()
            .copied()
            .filter(|pos| self.moving_at(*pos).is_none())
            .collect();
        for pos in gone {
            self.landed_bare(pos);
        }
        let carried = &mut self.sim.redstone.carried;
        carried
            .bare
            .retain(|_, since| tick.saturating_sub(*since) < CARRY_WAIT);
        let mut expired: Vec<(BlockPos, Compound)> = Vec::new();
        carried.early.retain(|pos, (nbt, since)| {
            if tick.saturating_sub(*since) < CARRY_WAIT {
                return true;
            }
            if let Some(nbt) = nbt.take() {
                expired.push((*pos, nbt));
            }
            false
        });
        expired.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        for (pos, nbt) in expired {
            self.spill_carried(pos, &nbt);
        }
    }
}
