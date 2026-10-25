//! The region's doors for taking a block out of its place: breaking it and
//! a piston lifting it. See [`crate::world::block_ledger`].

use super::pistons::carried::CARRY_WAIT;
use super::*;
use crate::world::block_ledger::{
    self, BlockBurn, BrokenBlock, ClaimReason, HeldBlock, Remains, VanillaDupe,
};
use crate::world::messages::BlockUpdate;
use crate::world::placement::step;
use crate::world::redstone::MovingBlock;
use tuff_world_model::chunk::is_air;

/// What this region did with a place it lifted ahead of a batch of a
/// neighbour's piston changes.
pub(super) enum Prelift {
    /// Lifted, to hand over as the change there runs.
    Lifted(HeldBlock),
    /// Nothing could be lifted there.
    Gone,
    /// Already given to the moving block it went into here.
    Delivered,
}

enum Taken {
    Standing(BlockState, Option<Compound>),
    Held(HeldBlock),
    Moving(BlockState),
    Ghost(BlockState),
}

impl RegionActor {
    /// Takes the block at `pos` out of the world to break it, with its
    /// block entity data, and writes `remains` in its place with all
    /// updates: holding the token means the block is gone. `None` when
    /// this region does not own a block there, it already left (broken or
    /// lifted, and nothing written there since), or a hook forbids the
    /// change: then nothing may break or drop. Breaking a moving piston
    /// breaks the block it carries.
    #[track_caller]
    pub(super) fn claim(
        &mut self,
        pos: BlockPos,
        remains: Remains,
        why: ClaimReason,
    ) -> Option<BrokenBlock> {
        self.claim_limited(pos, remains, why, crate::world::redstone::UPDATE_LIMIT)
    }

    /// [`Self::claim`] with the write's shape-update limit.
    #[track_caller]
    pub(super) fn claim_limited(
        &mut self,
        pos: BlockPos,
        remains: Remains,
        why: ClaimReason,
        limit: u32,
    ) -> Option<BrokenBlock> {
        self.claim_flagged(pos, remains, why, crate::world::redstone::UPDATE_ALL, limit)
    }

    /// [`Self::claim_limited`] writing the remains with `flags`.
    #[track_caller]
    pub(super) fn claim_flagged(
        &mut self,
        pos: BlockPos,
        remains: Remains,
        why: ClaimReason,
        flags: u32,
        limit: u32,
    ) -> Option<BrokenBlock> {
        Some(match self.take_out(pos, remains, limit, why, flags)? {
            Taken::Standing(state, block_entity) => BrokenBlock::claimed(state, block_entity, why),
            Taken::Held(held) => held.into_broken(None),
            Taken::Moving(base) => BrokenBlock::claimed(base, None, why),
            Taken::Ghost(state) => {
                block_ledger::duplicate(VanillaDupe::CarpetRail, &self.shared.config, state)?
            }
        })
    }

    /// Takes the block at `pos` out of the world for an entity to hold,
    /// writing `remains` in its place. `None` as for [`Self::claim`], or
    /// when only air was there.
    #[track_caller]
    pub(super) fn take_up(
        &mut self,
        pos: BlockPos,
        remains: Remains,
        why: ClaimReason,
    ) -> Option<HeldBlock> {
        match self.take_out(
            pos,
            remains,
            crate::world::redstone::UPDATE_LIMIT,
            why,
            crate::world::redstone::UPDATE_ALL,
        )? {
            Taken::Ghost(_) => None,
            Taken::Standing(state, _) => Some(HeldBlock::taken(state, why)),
            Taken::Held(held) => Some(held),
            Taken::Moving(base) => (!is_air(base)).then(|| HeldBlock::taken(base, why)),
        }
    }

    fn take_out(
        &mut self,
        pos: BlockPos,
        remains: Remains,
        limit: u32,
        why: ClaimReason,
        flags: u32,
    ) -> Option<Taken> {
        if !self.owns(pos) {
            return None;
        }
        let state = RegionActor::block(self, pos)?;
        if is_air(state) {
            return None;
        }
        let ghost = self.sim.redstone.vacated.contains(&pos);
        if ghost
            && (why != ClaimReason::Popped || self.ghost_dupe(pos) != Some(VanillaDupe::CarpetRail))
        {
            return None;
        }
        let left = match remains {
            Remains::Air => AIR,
            Remains::Vanilla => crate::world::placement::remains(state),
            Remains::State(left) | Remains::Fluid(left) => left,
        };
        if self.environment_vetoes(pos, left).is_some() {
            return None;
        }
        let block_entity = self.saved_block_entity(pos);
        let taken = if ghost {
            Taken::Ghost(state)
        } else if state.block() == Block::MovingPiston {
            self.sim.redstone.awaiting_lift.remove(&pos);
            match self.sim.redstone.take_held(pos) {
                Some(held) => Taken::Held(held),
                None => Taken::Moving(
                    self.moving_at(pos)
                        .filter(|moving| moving.source && moving.moved.block() != Block::PistonHead)
                        .map_or(AIR, |moving| moving.moved),
                ),
            }
        } else {
            Taken::Standing(state, block_entity)
        };
        self.set_block_flags(pos, left, flags, limit);
        Some(taken)
    }

    /// Takes the block at `from` out of the world for a piston to move.
    /// `None` when this region does not own a movable block there or it
    /// already left.
    #[track_caller]
    pub(super) fn lift(&mut self, from: BlockPos) -> Option<HeldBlock> {
        if !self.owns(from) || self.sim.redstone.vacated.contains(&from) {
            return None;
        }
        let state = RegionActor::block(self, from)?;
        if is_air(state) || state.block() == Block::MovingPiston {
            return None;
        }
        self.sim.redstone.vacated.insert(from);
        Some(HeldBlock::lifted(state))
    }

    /// The glitch a ghost at `pos` still takes part in: a block a piston
    /// lifted away that vanilla's update order would let react once more,
    /// when its setting is on.
    pub(super) fn ghost_dupe(&self, pos: BlockPos) -> Option<VanillaDupe> {
        if !self.sim.redstone.vacated.contains(&pos) {
            return None;
        }
        let block = RegionActor::block(self, pos)?.block();
        let kind = if block == Block::Tnt {
            VanillaDupe::Tnt
        } else if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::RAILS, block)
            || tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::WOOL_CARPETS, block)
        {
            VanillaDupe::CarpetRail
        } else {
            return None;
        };
        self.shared
            .config
            .get_bool(kind.key())
            .unwrap_or(true)
            .then_some(kind)
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
        let redstone = &mut self.sim.redstone;
        if redstone.vacated.is_empty() && redstone.moving.is_empty() {
            return;
        }
        redstone.vacated.remove(&pos);
        if old.block() == Block::MovingPiston
            && new.block() != Block::MovingPiston
            && let Some(lifted) = self.sim.redstone.take_held(pos)
        {
            lifted.burn(BlockBurn::Overwritten);
        }
    }

    /// Every vacated place is written over by the end of the change that
    /// vacated it; one still vacated is a ghost whose block is already
    /// elsewhere, so it is overwritten with air.
    pub(super) fn settle_vacated(&mut self) {
        if self.sim.redstone.vacated.is_empty() {
            return;
        }
        let left: Vec<_> = self.sim.redstone.vacated.drain().collect();
        for &pos in &left {
            self.raw_set_block(pos, AIR);
        }
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
    pub(super) fn lift_for(&mut self, to: BlockPos, moving: &MovingBlock) -> Option<HeldBlock> {
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
            None => self
                .sim
                .redstone
                .lifted_ahead
                .remove(&from)
                .or_else(|| self.lift(from)),
        }
    }

    /// Lifts the places of `from` this region owns ahead of the moves that
    /// write their destinations.
    pub(super) fn lift_ahead(&mut self, from: &[BlockPos]) {
        for &pos in from {
            if let Some(lifted) = self.lift(pos) {
                self.sim.redstone.lifted_ahead.insert(pos, lifted);
            }
        }
    }

    /// Blocks lifted ahead whose moves never started break where they were.
    pub(super) fn settle_lifted_ahead(&mut self) {
        for (pos, lifted) in std::mem::take(&mut self.sim.redstone.lifted_ahead) {
            self.break_lifted(pos, lifted);
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
        lifted: Option<HeldBlock>,
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
    pub(super) fn receive_lifted(&mut self, pos: BlockPos, block: Option<HeldBlock>) {
        let waiting = self.moving_at(pos).is_some_and(|moving| !moving.source)
            && !self.sim.redstone.has_held(pos);
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

    fn place_lifted(&mut self, pos: BlockPos, lifted: HeldBlock) {
        if self
            .moving_at(pos)
            .is_some_and(|moving| moving.moved != lifted.state())
        {
            self.receive_carried(pos, lifted.state());
        }
        if let Some(earlier) = self.sim.redstone.give_held(pos, lifted) {
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
    pub(super) fn landing(
        &mut self,
        pos: BlockPos,
        moving: &MovingBlock,
        held: Option<HeldBlock>,
    ) -> BlockState {
        if moving.source {
            return moving.moved;
        }
        self.sim.redstone.awaiting_lift.remove(&pos);
        held.map_or(AIR, HeldBlock::land)
    }

    /// A moving block left this region's bookkeeping without landing: its
    /// chunk unloaded (the block stays saved in its moving piston).
    pub(super) fn forget_moving(&mut self, pos: BlockPos, held: Option<HeldBlock>) {
        self.sim.redstone.awaiting_lift.remove(&pos);
        if let Some(lifted) = held {
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
    pub(super) fn break_lifted(&mut self, pos: BlockPos, lifted: HeldBlock) {
        let broken = lifted.into_broken(None);
        if self.shared.rule_bool("block_drops") {
            let stacks = self.shared.block_loot().destroyed(broken, &mut self.rng);
            self.drop_items(pos, stacks);
        }
    }
}
