//! The region's doors for taking a block out of its place: breaking it and
//! a piston lifting it. See [`crate::world::block_ledger`].

use super::pistons::carried::CARRY_WAIT;
use super::*;
use crate::world::block_ledger::{BlockBurn, BrokenBlock, ClaimReason, LiftedBlock};
use crate::world::messages::BlockUpdate;
use crate::world::placement::step;
use crate::world::redstone::MovingBlock;
use tuff_world_model::chunk::is_air;

/// What this region did with a place it lifted ahead of a batch of a
/// neighbour's piston changes.
pub(super) enum Prelift {
    /// Lifted, to hand over as the change there runs.
    Lifted(LiftedBlock),
    /// Nothing could be lifted there.
    Gone,
    /// Already given to the moving block it went into here.
    Delivered,
}

impl RegionActor {
    /// Takes the block at `pos` out of the world to break it, with its
    /// block entity data. `None` when this region does not own a block
    /// there, or it already left (broken or lifted, and nothing written
    /// there since): then nothing may break or drop. Breaking a moving
    /// piston breaks the block it carries. The caller writes what remains.
    #[track_caller]
    pub(super) fn claim(&mut self, pos: BlockPos, why: ClaimReason) -> Option<BrokenBlock> {
        if !self.owns(pos) || self.sim.redstone.vacated.contains(&pos) {
            return None;
        }
        let state = RegionActor::block(self, pos)?;
        if is_air(state) {
            return None;
        }
        let block_entity = self.saved_block_entity(pos);
        self.sim.redstone.vacated.insert(pos);
        if state.block() == Block::MovingPiston {
            self.sim.redstone.awaiting_lift.remove(&pos);
            return Some(match self.sim.redstone.lifted.remove(&pos) {
                Some(lifted) => lifted.into_broken(None),
                None => {
                    let base = self
                        .moving_at(pos)
                        .filter(|moving| moving.source && moving.moved.block() != Block::PistonHead)
                        .map_or(AIR, |moving| moving.moved);
                    BrokenBlock::claimed(base, None, why)
                }
            });
        }
        Some(BrokenBlock::claimed(state, block_entity, why))
    }

    /// Takes the block at `from` out of the world for a piston to move.
    /// `None` when this region does not own a movable block there or it
    /// already left.
    #[track_caller]
    pub(super) fn lift(&mut self, from: BlockPos) -> Option<LiftedBlock> {
        if !self.owns(from) || self.sim.redstone.vacated.contains(&from) {
            return None;
        }
        let state = RegionActor::block(self, from)?;
        if is_air(state) || state.block() == Block::MovingPiston {
            return None;
        }
        self.sim.redstone.vacated.insert(from);
        Some(LiftedBlock::lifted(state))
    }

    /// Whether the block at `pos` already left its place: it is a ghost
    /// that no longer reacts to anything until it is overwritten.
    pub(super) fn vacated(&self, pos: BlockPos) -> bool {
        !self.sim.redstone.vacated.is_empty() && self.sim.redstone.vacated.contains(&pos)
    }

    /// Something was written at `pos` over `old`: a block stands there in
    /// its own right again, and a lifted block whose moving piston was
    /// written over is gone.
    pub(super) fn written_over(&mut self, pos: BlockPos, old: BlockState, new: BlockState) {
        if !self.sim.redstone.vacated.is_empty() {
            self.sim.redstone.vacated.remove(&pos);
        }
        if old.block() == Block::MovingPiston
            && new.block() != Block::MovingPiston
            && let Some(lifted) = self.sim.redstone.lifted.remove(&pos)
        {
            lifted.burn(BlockBurn::Overwritten);
        }
    }

    /// Every vacated place is written over by the end of the change that
    /// vacated it; one still vacated is a block taken out of the world and
    /// left standing.
    pub(super) fn settle_vacated(&mut self) {
        if self.sim.redstone.vacated.is_empty() {
            return;
        }
        let left: Vec<_> = self.sim.redstone.vacated.drain().collect();
        if cfg!(debug_assertions) && !std::thread::panicking() {
            panic!("blocks taken out of the world were left standing at {left:?}");
        }
        log::error!("block conservation: blocks taken out of the world left standing at {left:?}");
    }

    /// Lifts, ahead of a batch of a neighbour's changes, every place of
    /// this region whose block one of its pistons moves on, so nothing in
    /// the batch reacts to blocks already on their way.
    pub(super) fn prelift(&mut self, updates: &[BlockUpdate]) {
        for update in updates {
            let BlockUpdate::Piston {
                pos,
                drops: false,
                carry: Some(_),
                expected,
                ..
            } = update
            else {
                continue;
            };
            if !self.owns(*pos) || self.sim.redstone.prelifted.contains_key(pos) {
                continue;
            }
            let current = RegionActor::block(self, *pos).unwrap_or(AIR);
            let lifted = if current == *expected || self.piston_movable(current) {
                self.lift(*pos)
            } else {
                None
            };
            let prelift = lifted.map_or(Prelift::Gone, Prelift::Lifted);
            self.sim.redstone.prelifted.insert(*pos, prelift);
        }
    }

    /// Ends a batch: every place lifted ahead was handed over.
    pub(super) fn settle_prelifted(&mut self) {
        for (pos, prelift) in std::mem::take(&mut self.sim.redstone.prelifted) {
            if let Prelift::Lifted(lifted) = prelift {
                self.break_lifted(pos, lifted);
            }
        }
    }

    /// The block a piston moves into `to` leaves the place it stood, when
    /// this region owns that place.
    #[track_caller]
    pub(super) fn lift_for(&mut self, to: BlockPos, moving: &MovingBlock) -> Option<LiftedBlock> {
        if moving.source {
            return None;
        }
        let from = step(to, moving.movement().opposite());
        match self.sim.redstone.prelifted.remove(&from) {
            Some(Prelift::Lifted(lifted)) => {
                self.sim.redstone.prelifted.insert(from, Prelift::Delivered);
                Some(lifted)
            }
            Some(other) => {
                self.sim.redstone.prelifted.insert(from, other);
                None
            }
            None => self.lift(from),
        }
    }

    /// Hands the block lifted ahead from `pos` on to `dest`, where it
    /// moves, as the piston's change at `pos` runs.
    pub(super) fn hand_over_prelifted(&mut self, pos: BlockPos, dest: Option<BlockPos>) {
        let Some(prelift) = self.sim.redstone.prelifted.remove(&pos) else {
            return;
        };
        let block = match prelift {
            Prelift::Lifted(lifted) => Some(lifted),
            Prelift::Gone => None,
            Prelift::Delivered => {
                self.sim.redstone.prelifted.insert(pos, Prelift::Delivered);
                return;
            }
        };
        let Some(dest) = dest else {
            if let Some(lifted) = block {
                self.break_lifted(pos, lifted);
            }
            return;
        };
        if self.pos.contains(ChunkPos::of_block(dest)) {
            self.receive_lifted(dest, block);
        } else {
            let update = BlockUpdate::Lifted { pos: dest, block };
            self.send_block_updates(HashMap::from([(
                ChunkPos::of_block(dest).region(),
                vec![update],
            )]));
        }
    }

    /// Gives the moving block that just started at `to` the block it
    /// carries, or waits for the region owning its old place to hand it
    /// over.
    pub(super) fn hold_lifted(
        &mut self,
        to: BlockPos,
        moving: Option<MovingBlock>,
        lifted: Option<LiftedBlock>,
    ) {
        let started = moving.filter(|moving| !moving.source && self.moving_at(to).is_some());
        let Some(moving) = started else {
            if let Some(lifted) = lifted {
                self.break_lifted(to, lifted);
            }
            return;
        };
        let lifted = match lifted {
            Some(lifted) => Some(lifted),
            None => match self.sim.redstone.early_lifted.remove(&to) {
                Some((block, _)) => block,
                None => {
                    let from = step(to, moving.movement().opposite());
                    if !self.owns(from) {
                        self.sim.redstone.awaiting_lift.insert(to, self.sched.tick);
                    }
                    None
                }
            },
        };
        if let Some(lifted) = lifted {
            self.place_lifted(to, lifted);
        }
    }

    /// The block moving into `pos`, handed over by the region owning the
    /// place it left.
    pub(super) fn receive_lifted(&mut self, pos: BlockPos, block: Option<LiftedBlock>) {
        let waiting = self.moving_at(pos).is_some_and(|moving| !moving.source)
            && !self.sim.redstone.lifted.contains_key(&pos);
        if waiting {
            self.sim.redstone.awaiting_lift.remove(&pos);
            if let Some(lifted) = block {
                self.place_lifted(pos, lifted);
            }
            return;
        }
        let tick = self.sched.tick;
        if let Some((Some(earlier), _)) = self.sim.redstone.early_lifted.insert(pos, (block, tick))
        {
            self.break_lifted(pos, earlier);
        }
    }

    fn place_lifted(&mut self, pos: BlockPos, lifted: LiftedBlock) {
        if self
            .moving_at(pos)
            .is_some_and(|moving| moving.moved != lifted.state())
        {
            self.receive_carried(pos, lifted.state());
        }
        if let Some(earlier) = self.sim.redstone.lifted.insert(pos, lifted) {
            self.break_lifted(pos, earlier);
        }
    }

    /// Whether the moving block at `pos` waits for its lifted block to be
    /// handed over before it lands.
    pub(super) fn awaits_lift(&self, pos: BlockPos) -> bool {
        self.sim
            .redstone
            .awaiting_lift
            .get(&pos)
            .is_some_and(|&since| self.sched.tick.saturating_sub(since) < CARRY_WAIT)
    }

    /// What the moving block at `pos` lands as: its lifted block, or
    /// nothing when none came (it broke where it stood).
    pub(super) fn landing(&mut self, pos: BlockPos, moving: &MovingBlock) -> BlockState {
        if moving.source {
            return moving.moved;
        }
        self.sim.redstone.awaiting_lift.remove(&pos);
        self.sim
            .redstone
            .lifted
            .remove(&pos)
            .map_or(AIR, LiftedBlock::land)
    }

    /// A moving block left this region's bookkeeping without landing: its
    /// chunk unloaded (the block stays saved in its moving piston).
    pub(super) fn forget_moving(&mut self, pos: BlockPos) {
        self.sim.redstone.awaiting_lift.remove(&pos);
        if let Some(lifted) = self.sim.redstone.lifted.remove(&pos) {
            if self.columns.contains_key(&ChunkPos::of_block(pos)) {
                lifted.burn(BlockBurn::Overwritten);
            } else {
                lifted.to_storage();
            }
        }
    }

    /// Lifted blocks handed over for a move that never came here, and
    /// waits that ran out.
    pub(super) fn expire_lifted(&mut self) {
        let tick = self.sched.tick;
        let redstone = &mut self.sim.redstone;
        redstone
            .awaiting_lift
            .retain(|_, since| tick.saturating_sub(*since) < CARRY_WAIT);
        if redstone.early_lifted.is_empty() {
            return;
        }
        let expired: Vec<_> = redstone
            .early_lifted
            .iter()
            .filter(|(_, (_, since))| tick.saturating_sub(*since) >= CARRY_WAIT)
            .map(|(pos, _)| *pos)
            .collect();
        for pos in expired {
            if let Some((Some(lifted), _)) = self.sim.redstone.early_lifted.remove(&pos) {
                self.break_lifted(pos, lifted);
            }
        }
    }

    /// A lifted block with nowhere to land breaks into its drops at `pos`.
    pub(super) fn break_lifted(&mut self, pos: BlockPos, lifted: LiftedBlock) {
        let broken = lifted.into_broken(None);
        if self.shared.rule_bool("block_drops") {
            let stacks = self.shared.block_loot().destroyed(broken, &mut self.rng);
            self.drop_items(pos, stacks);
        }
    }
}
