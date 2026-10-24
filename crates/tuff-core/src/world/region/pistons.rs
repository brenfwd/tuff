//! Block events and moving pistons: the region side of what a piston
//! behaviour asks for. Block events queue during a tick and run after the
//! block and fluid ticks, each telling nearby clients (a Block Action) when
//! its behaviour says so; the packet goes out before the batched block
//! changes of the same tick, so clients animate the move themselves before
//! they see its result. A moving piston's block entity is kept as saved
//! NBT for storage and clients, mirrored by a [`MovingBlock`] the region
//! ticks after its entities.

use bytes::Bytes;
use tuff_vanilla_data::block::{Axis, Facing, PushReaction};
use tuff_vanilla_data::registry::BlockEntityType;
use tuff_vanilla_data::{Block, BlockState};

use super::{AIR, ChunkPos, RegionActor, block_at};
use crate::protocol::play;
use crate::storage::chunk_nbt::{decode_block_state, encode_block_state};
use crate::world::block_entity;
use crate::world::block_ledger::HeldBlock;
use crate::world::messages::{BlockUpdate, PlayerEvent};
use crate::world::physics::{self, Aabb};
use crate::world::placement::step;
use crate::world::redstone::{
    self, BlockBehaviors, MovingBlock, RedstoneLevel, SHAPE_UPDATE_ORDER, UPDATE_LIMIT,
    UPDATE_MOVED_BY_PISTON,
};
use crate::world::region_api::BlockCause;
use crate::world::{PLAYER_HALF_WIDTH, framed, packets};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// How far past the moving block's face an entity it pushes ends up.
const PUSH_OFFSET: f64 = 0.01;
/// Update flags a finished move sets its block with when the block cannot
/// stay (it is set, then destroyed as its neighbours ask).
const ARRIVE_DOOMED: u32 = redstone::UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS
    | UPDATE_MOVED_BY_PISTON
    | redstone::UPDATE_KNOWN_SHAPE
    | redstone::UPDATE_INVISIBLE;
const ARRIVE: u32 = UPDATE_MOVED_BY_PISTON | redstone::UPDATE_ALL;

pub(super) mod carried;
mod display;

/// A queued block event, and the earlier tick a neighbour's late update
/// queued it as of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BlockEventData {
    pos: BlockPos,
    block: Block,
    kind: u8,
    data: u8,
    as_of: Option<u64>,
}

impl RegionActor {
    pub(super) fn queue_block_event(&mut self, pos: BlockPos, block: Block, kind: u8, data: u8) {
        let event = BlockEventData {
            pos,
            block,
            kind,
            data,
            as_of: self.sim.redstone.as_of,
        };
        let same = |queued: &BlockEventData| {
            (queued.pos, queued.block, queued.kind, queued.data) == (pos, block, kind, data)
        };
        if !self.sim.redstone.block_events.iter().any(same) {
            self.sim.redstone.block_events.push_back(event);
        }
    }

    /// Runs the queued block events, including those they queue. One a
    /// neighbour's late update queued runs as of that update's tick: the
    /// blocks it starts moving catch up.
    pub(super) fn run_block_events(&mut self) {
        let _span = tuff_timings::span!("region.block_events");
        while let Some(event) = self.sim.redstone.block_events.pop_front() {
            let Some(state) = RegionActor::block(self, event.pos) else {
                continue;
            };
            if state.block() != event.block || !self.owns(event.pos) {
                continue;
            }
            let late = event.as_of.filter(|tick| *tick < self.sched.tick);
            if late.is_some() {
                self.sim.redstone.as_of = late;
                self.sim.redstone.catching_up = Some(Vec::new());
            }
            let behaviour = self.shared.redstone_behaviors.get(event.block);
            self.sim.redstone.carried.shown.clear();
            let told = self.with_cause(BlockCause::Block(event.block), |region| {
                let told =
                    behaviour.trigger_event(region, event.pos, state, event.kind, event.data);
                if let Some(tick) = late {
                    region.sim.redstone.as_of = None;
                    region.catch_up_moves(tick);
                }
                told
            });
            let shown = std::mem::take(&mut self.sim.redstone.carried.shown);
            if told {
                let chunk = ChunkPos::of_block(event.pos);
                let packet = packets(block_event_frames(&event, &shown));
                self.send_chunk_packets(chunk, None, packet);
                self.show_displays(chunk, &shown);
            }
            self.flush_remote_updates();
        }
    }

    /// Steps the blocks a late block event started moving through the
    /// ticks since `tick`.
    fn catch_up_moves(&mut self, tick: u64) {
        let started = self.sim.redstone.catching_up.take().unwrap_or_default();
        let behind = self.sched.tick.saturating_sub(tick).min(MOVE_TICKS);
        for pos in started {
            for step in 0..behind {
                if self.moving_at(pos).is_none() || !self.tick_moving_block(pos, tick + step) {
                    break;
                }
            }
        }
    }

    /// Notes a block a piston starts moving into `pos` together with its
    /// block entity, for the viewers of the event moving it.
    pub(super) fn show_carried(&mut self, pos: BlockPos, moving: Option<MovingBlock>) {
        if let Some(moving) = moving
            && !moving.source
            && self.carries(moving.moved)
        {
            self.sim.redstone.carried.shown.push((pos, moving));
        }
    }

    pub(super) fn moving_at(&self, pos: BlockPos) -> Option<MovingBlock> {
        let moving = self.sim.redstone.moving.get(&pos)?;
        (RegionActor::block(self, pos).map(BlockState::block) == Some(Block::MovingPiston))
            .then_some(*moving)
    }

    pub(super) fn begin_move(&mut self, pos: BlockPos, moving: MovingBlock) {
        if RegionActor::block(self, pos).map(BlockState::block) != Some(Block::MovingPiston) {
            return;
        }
        if self.sim.redstone.moving.insert(pos, moving).is_none() {
            self.sim.redstone.moving_order.push(pos);
        }
        if let Some(started) = &mut self.sim.redstone.catching_up {
            started.push(pos);
        }
        let mut nbt = moving_nbt(pos, &moving);
        let carried = self.take_carried(pos, &moving);
        if let Some(carried) = carried.or_else(|| self.carried_from_afar(pos, &moving)) {
            nbt.insert(block_entity::raw::CARRIED, carried);
        }
        self.store_block_entity(pos, Err(nbt));
    }

    /// Whether a piston may move `state` with its block entity.
    pub(super) fn may_carry(&self, state: BlockState) -> bool {
        self.carries(state)
    }

    /// Takes the block entity of the block about to move into `pos` from
    /// where it stands, closing its menus without spilling it.
    fn take_carried(&mut self, pos: BlockPos, moving: &MovingBlock) -> Option<Compound> {
        if moving.source || moving.moved.block().block_entity().is_none() {
            return None;
        }
        let from = step(pos, moving.movement().opposite());
        if RegionActor::block(self, from).map(BlockState::block) != Some(moving.moved.block())
            || !self.may_carry(moving.moved)
        {
            return None;
        }
        self.lift_block_entity(from)
    }

    /// Gives the block that just landed at `pos` the block entity it
    /// carried, or scatters what it held when the block there now has
    /// another kind of block entity or none.
    fn land_carried(&mut self, pos: BlockPos, piston: Option<Compound>) {
        let Some(mut nbt) = piston.and_then(|mut piston| {
            piston
                .remove(block_entity::raw::CARRIED)
                .and_then(|tag| tag.as_compound().cloned())
        }) else {
            return;
        };
        let Some(kind) = block_entity::raw::kind(&nbt) else {
            return;
        };
        let landed = RegionActor::block(self, pos).and_then(|state| state.block().block_entity());
        if landed != Some(kind) {
            self.spill_carried(pos, &nbt);
            return;
        }
        block_entity::raw::relocate(&mut nbt, pos);
        let entity = match block_entity::BlockEntity::decode(&nbt) {
            Some((_, entity)) => Ok(entity),
            None => Err(nbt),
        };
        self.store_block_entity(pos, entity);
        if kind.sends_updates() && !self.block_entity_updates.contains(&pos) {
            self.block_entity_updates.push(pos);
        }
    }

    /// Records the moving blocks of a chunk that just loaded.
    pub(super) fn load_moving_blocks(&mut self, chunk: ChunkPos) {
        let Some(column) = self.columns.get(&chunk) else {
            return;
        };
        let mut found: Vec<(BlockPos, MovingBlock)> = column
            .meta
            .raw_block_entities
            .iter()
            .filter(|(_, nbt)| block_entity::raw::kind(nbt) == Some(BlockEntityType::Piston))
            .filter(|(pos, _)| column.chunk.get(pos.x, pos.y, pos.z).block() == Block::MovingPiston)
            .map(|(pos, nbt)| (*pos, moving_from_nbt(nbt)))
            .collect();
        found.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        for (pos, moving) in found {
            if self.sim.redstone.moving.insert(pos, moving).is_none() {
                self.sim.redstone.moving_order.push(pos);
            }
            if !moving.source {
                self.sim
                    .redstone
                    .lifted
                    .entry(pos)
                    .or_insert_with(|| HeldBlock::from_storage(moving.moved));
            }
        }
    }

    /// Ends the move through `pos` now: the arm vanishes, anything else
    /// arrives, taking the shape its neighbours give it.
    pub(super) fn finish_move(&mut self, pos: BlockPos) {
        let Some(moving) = self.moving_at(pos) else {
            return;
        };
        if moving.progress_before >= 1.0 {
            return;
        }
        self.landed_bare(pos);
        self.sim.redstone.moving.remove(&pos);
        let piston = self.remove_block_entity(pos, None);
        let landing = self.landing(pos, &moving);
        let new = if moving.source || is_air(landing) {
            AIR
        } else {
            self.shape_from_neighbours(landing, pos)
        };
        RedstoneLevel::set_block(self, pos, new, redstone::UPDATE_ALL);
        self.land_carried(pos, piston);
        RedstoneLevel::neighbor_changed(self, pos, new.block());
    }

    /// Moves every moving block one step, landing those that finished.
    pub(super) fn tick_moving_blocks(&mut self) {
        let _span = tuff_timings::span!("region.moving_blocks");
        let tick = self.sched.tick;
        self.expire_displays();
        self.expire_carried();
        self.expire_lifted();
        self.sim.redstone.moving_ticked = tick;
        self.sim
            .redstone
            .remote_moving
            .retain(|_, (_, started)| tick.saturating_sub(*started) < MOVE_TICKS);
        if self.sim.redstone.moving_order.is_empty() {
            return;
        }
        let order = std::mem::take(&mut self.sim.redstone.moving_order);
        let mut kept = Vec::with_capacity(order.len());
        for pos in order {
            if self.moving_at(pos).is_none() {
                self.sim.redstone.moving.remove(&pos);
                self.forget_moving(pos);
                continue;
            }
            let cause = BlockCause::Block(Block::MovingPiston);
            if self.with_cause(cause, |region| region.tick_moving_block(pos, tick)) {
                kept.push(pos);
            }
        }
        kept.append(&mut self.sim.redstone.moving_order);
        kept.dedup();
        self.sim.redstone.moving_order = kept;
    }

    /// One step of the block moving through `pos`, as on game tick `at`;
    /// false once it landed.
    fn tick_moving_block(&mut self, pos: BlockPos, at: u64) -> bool {
        let Some(mut moving) = self.sim.redstone.moving.get(&pos).copied() else {
            return false;
        };
        moving.last_ticked = at;
        moving.progress_before = moving.progress;
        if moving.progress_before >= 1.0 {
            if self.holds_landing(pos) || self.awaits_lift(pos) {
                self.sim.redstone.moving.insert(pos, moving);
                return true;
            }
            self.sim.redstone.moving.remove(&pos);
            let piston = self.remove_block_entity(pos, None);
            if RegionActor::block(self, pos).map(BlockState::block) != Some(Block::MovingPiston) {
                self.forget_moving(pos);
                return false;
            }
            moving.moved = self.landing(pos, &moving);
            if is_air(moving.moved) {
                RedstoneLevel::set_block(self, pos, AIR, ARRIVE);
                self.land_carried(pos, piston);
                RedstoneLevel::neighbor_changed(self, pos, Block::Air);
                return false;
            }
            let new = self.shape_from_neighbours(moving.moved, pos);
            if is_air(new) {
                self.set_block_flags(pos, moving.moved, ARRIVE_DOOMED, UPDATE_LIMIT);
                self.land_carried(pos, piston);
                if RegionActor::block(self, pos) == Some(moving.moved) {
                    self.destroy_by_update(pos, moving.moved, true, UPDATE_LIMIT);
                }
                return false;
            }
            let new = new.with_waterlogged(false).unwrap_or(new);
            RedstoneLevel::set_block(self, pos, new, ARRIVE);
            self.land_carried(pos, piston);
            RedstoneLevel::neighbor_changed(self, pos, new.block());
            return false;
        }
        let progress = moving.progress + 0.5;
        self.push_entities(pos, &moving, progress);
        self.carry_stuck_entities(pos, &moving, progress);
        moving.progress = progress.min(1.0);
        self.sim.redstone.moving.insert(pos, moving);
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert("progress", moving.progress_before);
        }
        true
    }

    /// Applies a piston's change a neighbouring region sent. A block that
    /// starts moving catches up on the steps it missed since the piston
    /// moved, so it lands on the same tick as the piston's own blocks. When
    /// the block there is not the one the piston saw, the one really there
    /// travels on in its place.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn receive_piston_set(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        drops: bool,
        moving: Option<MovingBlock>,
        lifted: Option<crate::world::block_ledger::HeldBlock>,
        tick: u64,
        expected: BlockState,
        carry: Option<BlockPos>,
    ) {
        let cause = std::mem::replace(&mut self.sim.cause, BlockCause::Block(Block::Piston));
        let lifted = match lifted {
            Some(lifted) => Some(lifted),
            None => moving.and_then(|moving| self.lift_for(pos, &moving)),
        };
        if !drops
            && let Some(current) = RegionActor::block(self, pos)
            && current != expected
            && current.block() != Block::MovingPiston
            && !self.vacated(pos)
        {
            self.stale_piston_read(pos, current, carry);
        }
        self.carry_out(pos, carry, expected, false);
        self.hand_over_prelifted(pos, carry);
        self.apply_piston_set(pos, state, flags, drops, moving);
        self.hold_lifted(pos, moving, lifted);
        if moving.is_some() {
            let behind = self.sched.tick.saturating_sub(tick).min(MOVE_TICKS);
            for step in 0..behind {
                if self.moving_at(pos).is_none() || !self.tick_moving_block(pos, tick + step) {
                    break;
                }
            }
        }
        self.sim.cause = cause;
    }

    /// The block at `pos` is `current`, not what a piston in a neighbouring
    /// region read from a stale copy: it was lifted to move on to `carry`
    /// in its place, unless a piston could not have moved it (then it
    /// breaks); with nowhere to go it is crushed.
    fn stale_piston_read(&mut self, pos: BlockPos, current: BlockState, carry: Option<BlockPos>) {
        if !is_air(current) && (carry.is_none() || !self.piston_movable(current)) {
            RedstoneLevel::effect(self, redstone::Effect::DropResources(pos, current));
            self.remove_block_entity(pos, Some(current));
        }
    }

    /// Whether a piston could move `current`.
    pub(super) fn piston_movable(&self, current: BlockState) -> bool {
        !is_air(current)
            && (current.block().block_entity().is_none() || self.carries(current))
            && current.destroy_speed().is_some()
            && matches!(
                current.block().push_reaction(),
                PushReaction::PushPull | PushReaction::Push | PushReaction::IgnoreEntity
            )
    }

    /// The block moving into `pos` is really `moved`.
    pub(super) fn receive_carried(&mut self, pos: BlockPos, moved: BlockState) {
        if self.moving_at(pos).is_none() {
            return;
        }
        let Some(moving) = self.sim.redstone.moving.get_mut(&pos) else {
            return;
        };
        if moving.source {
            return;
        }
        moving.moved = moved;
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert("blockState", encode_block_state(moved));
        }
        self.carried_changed(pos);
    }

    /// Ends the move this region's piston started in a neighbouring region.
    pub(super) fn finish_remote_move(&mut self, pos: BlockPos) {
        let Some((moving, _)) = self.sim.redstone.remote_moving.remove(&pos) else {
            return;
        };
        let landed = if moving.source { AIR } else { moving.moved };
        self.sim.redstone.sent_changes.insert(pos, landed);
        self.queue_remote(pos, BlockUpdate::FinishMove { pos });
    }

    /// The block this region's piston is moving through `pos` in a
    /// neighbouring region, as far along as the owner has it.
    pub(super) fn remote_moving_at(&self, pos: BlockPos) -> Option<MovingBlock> {
        let (mut moving, started) = *self.sim.redstone.remote_moving.get(&pos)?;
        if self
            .sim
            .redstone
            .sent_changes
            .get(&pos)
            .is_some_and(|state| state.block() != Block::MovingPiston)
        {
            return None;
        }
        let steps = self.sched.tick.saturating_sub(started)
            + u64::from(self.sim.redstone.moving_ticked == self.sched.tick);
        if steps >= MOVE_TICKS {
            return None;
        }
        let half = |n: u64| (0.5 * n as f32).min(1.0);
        moving.progress = half(steps);
        if steps > 0 {
            moving.progress_before = half(steps - 1);
            moving.last_ticked = started + steps - 1;
        }
        Some(moving)
    }

    /// `state` after every neighbour of `pos` reshaped it.
    fn shape_from_neighbours(&mut self, state: BlockState, pos: BlockPos) -> BlockState {
        let mut state = state;
        for direction in SHAPE_UPDATE_ORDER {
            if is_air(state) {
                break;
            }
            let neighbour = RegionActor::block(self, step(pos, direction)).unwrap_or(AIR);
            let behaviour = self.shared.redstone_behaviors.get(state.block());
            let shaped = if (state.block().component().is_some() || behaviour.is_active())
                && self.redstone_ready(pos)
            {
                behaviour.update_shape(self, pos, state, direction, neighbour)
            } else {
                None
            };
            if let Some(shaped) =
                shaped.or_else(|| self.placement_shape(pos, state, direction, neighbour))
            {
                state = shaped;
            }
        }
        state
    }

    /// The collision boxes of every block this region's pistons are
    /// moving (also into a neighbour) or it receives, where they are now,
    /// for entities moving into them.
    pub(super) fn moving_colliders(&self) -> Vec<Aabb> {
        let mut boxes = Vec::new();
        for (pos, moving) in &self.sim.redstone.moving {
            if self.moving_at(*pos).is_some() {
                boxes.extend(moving_collision(
                    &*self.shared.redstone_behaviors,
                    *pos,
                    moving,
                ));
            }
        }
        for pos in self.sim.redstone.remote_moving.keys() {
            if let Some(moving) = self.remote_moving_at(*pos) {
                boxes.extend(moving_collision(
                    &*self.shared.redstone_behaviors,
                    *pos,
                    &moving,
                ));
            }
        }
        boxes
    }

    /// Moves an entity or player as a piston does: at most 0.51 blocks
    /// along an axis each tick, and for an entity, stopping at blocks.
    /// Returns how far it really moved.
    fn piston_move(&mut self, id: i32, movement: Facing, amount: f64) -> [f64; 3] {
        let (mx, my, mz) = movement.step();
        let mut delta = [
            amount * f64::from(mx),
            amount * f64::from(my),
            amount * f64::from(mz),
        ];
        if delta.iter().map(|v| v * v).sum::<f64>() > 1.0e-7 {
            if self.sim.redstone.piston_deltas_tick != self.sched.tick {
                self.sim.redstone.piston_deltas.clear();
                self.sim.redstone.piston_deltas_tick = self.sched.tick;
            }
            let axis = match movement.axis() {
                Axis::X => 0,
                Axis::Y => 1,
                Axis::Z => 2,
            };
            let moved = self.sim.redstone.piston_deltas.entry(id).or_default();
            let total = (delta[axis] + moved[axis]).clamp(-MAX_PISTON_STEP, MAX_PISTON_STEP);
            let allowed = total - moved[axis];
            moved[axis] = total;
            delta = [0.0; 3];
            if allowed.abs() <= 1.0e-5 {
                return delta;
            }
            delta[axis] = allowed;
        }
        let (columns, halo) = (&self.columns, &self.border.halo);
        let Some(tracked) = self.ents.entities.get_mut(&id) else {
            return delta;
        };
        let lookup = |p: BlockPos| block_at(columns, halo, p);
        let moved = physics::collide(tracked.entity.bounds(), delta, 0.0, false, &lookup);
        let base = tracked.entity.base_mut();
        for axis in 0..3 {
            base.position[axis] += moved[axis];
        }
        moved
    }

    /// Moves the entities in `area` other than `source` (and shulkers)
    /// `amount` blocks along `direction`, stopping at blocks.
    pub(super) fn shell_push(&mut self, source: i32, area: Aabb, direction: Facing, amount: f64) {
        let (dx, dy, dz) = direction.step();
        let delta = [
            amount * f64::from(dx),
            amount * f64::from(dy),
            amount * f64::from(dz),
        ];
        let (columns, halo) = (&self.columns, &self.border.halo);
        let lookup = |p: BlockPos| block_at(columns, halo, p);
        for (id, tracked) in self.ents.entities.iter_mut() {
            if *id == source
                || tracked.entity.base().kind == tuff_vanilla_data::registry::EntityType::Shulker
                || !physics::intersects(tracked.entity.bounds(), area)
            {
                continue;
            }
            let moved = physics::collide(tracked.entity.bounds(), delta, 0.0, false, &lookup);
            let base = tracked.entity.base_mut();
            for axis in 0..3 {
                base.position[axis] += moved[axis];
            }
            tracked.sync.impulse();
        }
    }

    /// Pushes the region's entities and players out of the way of a moving
    /// block about to reach `progress`, and bounces them off slime.
    fn push_entities(&mut self, pos: BlockPos, moving: &MovingBlock, progress: f32) {
        let behaviour = self.shared.redstone_behaviors.get(moving.moved.block());
        let shape = if moving.source && !moving.extending {
            behaviour
                .moving_arm(moving.moved, moving.facing, moving.progress > 0.25)
                .unwrap_or(moving.moved)
        } else {
            moving.moved
        };
        let boxes = shape.collision_boxes();
        if boxes.is_empty() {
            return;
        }
        let movement = moving.movement();
        let delta = f64::from(progress - moving.progress);
        let origin = moving_origin(pos, moving);
        let placed: Vec<Aabb> = boxes.iter().map(|b| place(origin, b)).collect();
        let areas: Vec<Aabb> = placed
            .iter()
            .map(|b| movement_area(*b, movement, delta))
            .collect();
        let bounds = placed.iter().copied().reduce(union).unwrap_or_default();
        let reach = union(movement_area(bounds, movement, delta), bounds);
        let bounce = behaviour.bounces_pushed();
        let (mx, my, mz) = movement.step();
        let fix_base = !moving.extending && moving.source;
        let ids: Vec<i32> = self
            .ents
            .entities
            .iter()
            .filter(|(_, tracked)| {
                !tracked.entity.ignores_pistons()
                    && physics::intersects(tracked.entity.bounds(), reach)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let Some(tracked) = self.ents.entities.get_mut(&id) else {
                continue;
            };
            if bounce {
                let base = tracked.entity.base_mut();
                match movement.axis() {
                    Axis::X => base.velocity[0] = f64::from(mx),
                    Axis::Y => base.velocity[1] = f64::from(my),
                    Axis::Z => base.velocity[2] = f64::from(mz),
                }
            }
            let velocity = tracked.entity.base().velocity;
            let needed = needed_push(&areas, movement, tracked.entity.bounds(), delta);
            if bounce || needed > 0.0 {
                self.tell_piston_driver(id, bounce.then_some(velocity));
            }
            if needed <= 0.0 {
                continue;
            }
            self.piston_move(id, movement, needed.min(delta) + PUSH_OFFSET);
            if fix_base
                && let Some(tracked) = self.ents.entities.get(&id)
                && let Some(amount) = out_of_base(pos, movement, tracked.entity.bounds(), delta)
            {
                self.piston_move(id, movement.opposite(), amount);
            }
        }
        let players: Vec<(i32, Aabb)> = self
            .ents
            .presence
            .iter()
            .filter(|(_, presence)| presence.is_physical())
            .map(|(id, presence)| (*id, presence_bounds(presence)))
            .filter(|(_, bounds)| physics::intersects(*bounds, reach))
            .collect();
        for (id, bounds) in players {
            let needed = needed_push(&areas, movement, bounds, delta);
            if needed <= 0.0 {
                continue;
            }
            let mut moved = self.piston_move(id, movement, needed.min(delta) + PUSH_OFFSET);
            if fix_base
                && let Some(amount) =
                    out_of_base(pos, movement, physics::offset(bounds, moved), delta)
            {
                let back = self.piston_move(id, movement.opposite(), amount);
                for axis in 0..3 {
                    moved[axis] += back[axis];
                }
            }
            self.push_player(id, moved);
        }
    }

    /// Carries what stands on a honey block along as it moves sideways.
    fn carry_stuck_entities(&mut self, pos: BlockPos, moving: &MovingBlock, progress: f32) {
        let movement = moving.movement();
        if !self
            .shared
            .redstone_behaviors
            .get(moving.moved.block())
            .drags_riders()
            || movement.axis() == Axis::Y
        {
            return;
        }
        let top = moving
            .moved
            .collision_boxes()
            .iter()
            .map(|b| f64::from(b[4]))
            .fold(f64::NEG_INFINITY, f64::max);
        if !top.is_finite() {
            return;
        }
        let area = place(
            moving_origin(pos, moving),
            &[0.0, top as f32, 0.0, 1.0, 1.500_001, 1.0],
        );
        let delta = f64::from(progress - moving.progress);
        let stuck = |bounds: Aabb, position: [f64; 3], grounded: bool| {
            grounded
                && physics::intersects(bounds, area)
                && (area[0]..=area[3]).contains(&position[0])
                && (area[2]..=area[5]).contains(&position[2])
        };
        let ids: Vec<i32> = self
            .ents
            .entities
            .iter()
            .filter(|(_, tracked)| {
                let base = tracked.entity.base();
                !tracked.entity.ignores_pistons()
                    && stuck(tracked.entity.bounds(), base.position, base.on_ground)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.piston_move(id, movement, delta);
        }
        let players: Vec<i32> = self
            .ents
            .presence
            .iter()
            .filter(|(_, presence)| {
                presence.is_physical()
                    && stuck(
                        presence_bounds(presence),
                        presence.position,
                        presence.grounded,
                    )
            })
            .map(|(id, _)| *id)
            .collect();
        for id in players {
            let moved = self.piston_move(id, movement, delta);
            self.push_player(id, moved);
        }
    }

    /// Moves the region's copy of a player and tells its session.
    fn push_player(&mut self, id: i32, delta: [f64; 3]) {
        if delta == [0.0; 3] {
            return;
        }
        let dimension = self.dimension;
        let Some(presence) = self.ents.presence.get_mut(&id) else {
            return;
        };
        for axis in 0..3 {
            presence.position[axis] += delta[axis];
        }
        presence
            .handle
            .send(PlayerEvent::PistonPushed { dimension, delta });
    }
}

/// How many ticks a moved block takes from starting to land.
const MOVE_TICKS: u64 = 3;
/// The furthest pistons move anything along one axis in a tick.
const MAX_PISTON_STEP: f64 = 0.51;

fn union(a: Aabb, b: Aabb) -> Aabb {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].min(b[2]),
        a[3].max(b[3]),
        a[4].max(b[4]),
        a[5].max(b[5]),
    ]
}

fn place(origin: [f64; 3], b: &[f32; 6]) -> Aabb {
    [
        origin[0] + f64::from(b[0]),
        origin[1] + f64::from(b[1]),
        origin[2] + f64::from(b[2]),
        origin[0] + f64::from(b[3]),
        origin[1] + f64::from(b[4]),
        origin[2] + f64::from(b[5]),
    ]
}

/// Where the moving block's own corner is now.
fn moving_origin(pos: BlockPos, moving: &MovingBlock) -> [f64; 3] {
    let offset = f64::from(if moving.extending {
        moving.progress - 1.0
    } else {
        1.0 - moving.progress
    });
    let (fx, fy, fz) = moving.facing.step();
    [
        f64::from(pos.x) + offset * f64::from(fx),
        f64::from(pos.y) + offset * f64::from(fy),
        f64::from(pos.z) + offset * f64::from(fz),
    ]
}

/// The collision boxes of a moving block where it is now: a retracting
/// piston's base keeps its extended shape, the arm and moved blocks are
/// where they have got to.
fn moving_collision(
    behaviors: &dyn BlockBehaviors,
    pos: BlockPos,
    moving: &MovingBlock,
) -> Vec<Aabb> {
    let mut boxes = Vec::new();
    let behaviour = behaviors.get(moving.moved.block());
    if !moving.extending
        && moving.source
        && let Some(base) = behaviour.arm_base(moving.moved)
    {
        boxes.extend(physics::block_boxes(Some(base), pos));
    }
    let state = if moving.source {
        let short = moving.extending != (1.0 - moving.progress < 0.25);
        behaviour
            .moving_arm(moving.moved, moving.facing, short)
            .unwrap_or(moving.moved)
    } else {
        moving.moved
    };
    let origin = moving_origin(pos, moving);
    boxes.extend(state.collision_boxes().iter().map(|b| place(origin, b)));
    boxes
}

/// How far `bounds` must move toward `movement` to clear the swept
/// `areas`, stopping early once it needs the whole step.
fn needed_push(areas: &[Aabb], movement: Facing, bounds: Aabb, delta: f64) -> f64 {
    let mut needed: f64 = 0.0;
    for area in areas {
        if physics::intersects(*area, bounds) {
            needed = needed.max(distance_out(*area, movement, bounds));
            if needed >= delta {
                break;
            }
        }
    }
    needed
}

/// How far back out of a retracting piston's base cell something it
/// dragged into it moves, if it is only just inside.
fn out_of_base(pos: BlockPos, movement: Facing, bounds: Aabb, delta: f64) -> Option<f64> {
    let cell = place(
        [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)],
        &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
    );
    if !physics::intersects(bounds, cell) {
        return None;
    }
    let opposite = movement.opposite();
    let inside = [
        bounds[0].max(cell[0]),
        bounds[1].max(cell[1]),
        bounds[2].max(cell[2]),
        bounds[3].min(cell[3]),
        bounds[4].min(cell[4]),
        bounds[5].min(cell[5]),
    ];
    let whole = distance_out(cell, opposite, bounds) + PUSH_OFFSET;
    let part = distance_out(cell, opposite, inside) + PUSH_OFFSET;
    ((whole - part).abs() < PUSH_OFFSET).then(|| whole.min(delta) + PUSH_OFFSET)
}

fn presence_bounds(presence: &super::Presence) -> Aabb {
    let [x, y, z] = presence.position;
    [
        x - PLAYER_HALF_WIDTH,
        y,
        z - PLAYER_HALF_WIDTH,
        x + PLAYER_HALF_WIDTH,
        y + presence.looks.height,
        z + PLAYER_HALF_WIDTH,
    ]
}

/// The space a box sweeps moving `delta` toward `movement`.
fn movement_area(b: [f64; 6], movement: Facing, delta: f64) -> [f64; 6] {
    let [x0, y0, z0, x1, y1, z1] = b;
    match movement {
        Facing::East => [x1, y0, z0, x1 + delta, y1, z1],
        Facing::West => [x0 - delta, y0, z0, x0, y1, z1],
        Facing::Up => [x0, y1, z0, x1, y1 + delta, z1],
        Facing::Down => [x0, y0 - delta, z0, x1, y0, z1],
        Facing::South => [x0, y0, z1, x1, y1, z1 + delta],
        Facing::North => [x0, y0, z0 - delta, x1, y1, z0],
    }
}

/// How far `bounds` must move toward `movement` to clear `area`.
fn distance_out(area: [f64; 6], movement: Facing, bounds: [f64; 6]) -> f64 {
    match movement {
        Facing::East => area[3] - bounds[0],
        Facing::West => bounds[3] - area[0],
        Facing::Up => area[4] - bounds[1],
        Facing::Down => bounds[4] - area[1],
        Facing::South => area[5] - bounds[2],
        Facing::North => bounds[5] - area[2],
    }
}

/// What a block event's viewers get. Clients animate a move themselves and
/// refuse to move blocks with block entities, so each carried block first
/// turns into a plain stand-in where it stands, and once the event made
/// its moving piston the client is told the block really moving there.
fn block_event_frames(event: &BlockEventData, carried: &[(BlockPos, MovingBlock)]) -> Vec<Bytes> {
    let stand_in = Block::Stone.default_state().id();
    let mut frames = Vec::with_capacity(1 + 2 * carried.len());
    frames.extend(carried.iter().map(|(pos, moving)| {
        framed(&play::BlockUpdate {
            position: step(*pos, moving.movement().opposite()),
            state: stand_in,
        })
    }));
    frames.push(framed(&play::BlockEvent {
        position: event.pos,
        action: event.kind,
        parameter: event.data,
        block: event.block.id(),
    }));
    for (pos, moving) in carried {
        if let Some((_, kind, nbt)) = block_entity::raw::client_data(&moving_nbt(*pos, moving)) {
            frames.push(framed(&play::BlockEntityData {
                position: *pos,
                kind: kind.id(),
                nbt: &nbt,
            }));
        }
    }
    frames
}

fn moving_nbt(pos: BlockPos, moving: &MovingBlock) -> Compound {
    block_entity::raw::new(BlockEntityType::Piston, pos)
        .with("blockState", encode_block_state(moving.moved))
        .with("facing", moving.facing.id() as i32)
        .with("progress", moving.progress_before)
        .with("extending", moving.extending)
        .with("source", moving.source)
}

fn moving_from_nbt(nbt: &Compound) -> MovingBlock {
    let moved = nbt.get("blockState").map_or(AIR, decode_block_state);
    let facing = nbt
        .get_int("facing")
        .and_then(facing_from_id)
        .unwrap_or(Facing::Down);
    let progress = nbt.get_float("progress").unwrap_or(0.0);
    MovingBlock {
        moved,
        facing,
        extending: nbt.get_bool("extending").unwrap_or(false),
        source: nbt.get_bool("source").unwrap_or(false),
        progress,
        progress_before: progress,
        last_ticked: 0,
    }
}

fn facing_from_id(id: i32) -> Option<Facing> {
    Some(match id {
        0 => Facing::Down,
        1 => Facing::Up,
        2 => Facing::North,
        3 => Facing::South,
        4 => Facing::West,
        5 => Facing::East,
        _ => return None,
    })
}
