//! The region as the world redstone acts on: the game's block update model
//! (setting blocks with update flags, the collecting neighbour updater),
//! signal reads, redstone ticks, entities pressing plates and buttons, and
//! the effects behaviours ask for.
//!
//! Updates for positions another region owns go there as
//! [`BlockUpdate::Neighbour`] / [`BlockUpdate::Shape`] after the cascade and
//! run on its next tick. Redstone reactions only run where the 3x3 chunks
//! around the block are loaded (here or in the halo), so a signal is never
//! read from an unloaded block; elsewhere neighbour updates are dropped and
//! ticks wait, as in the game's non-ticking chunks.

use std::collections::HashMap;

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use super::{AIR, ChunkPos, RegionActor};
use crate::protocol::play;
use crate::world::block_entity::analog;
use crate::world::messages::BlockUpdate;
use crate::world::messages::RegionInternal;
use crate::world::placement::{self, step};
use crate::world::redstone::{
    self, BlockBehavior, Effect, MovingBlock, Orientation, Reader, RedstoneLevel,
    SHAPE_UPDATE_ORDER, TickPriority, UPDATE_KNOWN_SHAPE, UPDATE_LIMIT, UPDATE_MOVED_BY_PISTON,
    UPDATE_NEIGHBORS, UPDATE_ORDER, UPDATE_PUSH_ENTITIES, UPDATE_SKIP_ON_PLACE,
    UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE, UPDATE_SUPPRESS_DROPS,
};
use crate::world::region_api::BlockCause;
use crate::world::ticks::Scheduled;
use crate::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::RegionPos;

mod reader;
mod shapes;

const MAX_DEFERRED: usize = 4096;
/// How often slow block ticks (daylight detectors) run.
const SLOW_TICK_INTERVAL: u64 = 20;

/// A pending step of a cascade.
#[derive(Clone, Copy, Debug)]
enum Update {
    Neighbor {
        pos: BlockPos,
        source: Block,
        orientation: Option<Orientation>,
    },
    /// The neighbours of `pos` in [`UPDATE_ORDER`], one per step.
    Multi {
        pos: BlockPos,
        source: Block,
        skip: Option<Facing>,
        next: usize,
        orientation: Option<Orientation>,
    },
    Shape {
        direction: Facing,
        neighbour: BlockState,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        flags: u32,
        limit: u32,
    },
}

/// What one step of an [`Update`] runs.
enum Action {
    Neighbor {
        pos: BlockPos,
        source: Block,
        orientation: Option<Orientation>,
    },
    Shape {
        direction: Facing,
        neighbour: BlockState,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        flags: u32,
        limit: u32,
    },
}

impl Update {
    fn multi(
        pos: BlockPos,
        source: Block,
        skip: Option<Facing>,
        orientation: Option<Orientation>,
    ) -> Self {
        let next = usize::from(Some(UPDATE_ORDER[0]) == skip);
        Self::Multi {
            pos,
            source,
            skip,
            next,
            orientation,
        }
    }

    /// The next action and whether more remain after it.
    fn step(&mut self) -> (Action, bool) {
        match self {
            Self::Neighbor {
                pos,
                source,
                orientation,
            } => (
                Action::Neighbor {
                    pos: *pos,
                    source: *source,
                    orientation: *orientation,
                },
                false,
            ),
            Self::Multi {
                pos,
                source,
                skip,
                next,
                orientation,
            } => {
                let direction = UPDATE_ORDER[*next];
                *next += 1;
                if *next < UPDATE_ORDER.len() && Some(UPDATE_ORDER[*next]) == *skip {
                    *next += 1;
                }
                (
                    Action::Neighbor {
                        pos: step(*pos, direction),
                        source: *source,
                        orientation: orientation.map(|o| o.with_front(direction)),
                    },
                    *next < UPDATE_ORDER.len(),
                )
            }
            Self::Shape {
                direction,
                neighbour,
                pos,
                neighbour_pos,
                flags,
                limit,
            } => (
                Action::Shape {
                    direction: *direction,
                    neighbour: *neighbour,
                    pos: *pos,
                    neighbour_pos: *neighbour_pos,
                    flags: *flags,
                    limit: *limit,
                },
                false,
            ),
        }
    }
}

/// The collecting neighbour updater: a cascade runs depth first, updates
/// queued while one runs going on top of the stack in the order they were
/// queued.
#[derive(Default)]
pub(super) struct Updater {
    stack: Vec<Update>,
    added: Vec<Update>,
    count: usize,
    /// Updates for other regions, sent when the cascade ends.
    remote: HashMap<RegionPos, Vec<BlockUpdate>>,
}

/// Region state the redstone rules keep.
#[derive(Default)]
pub(super) struct RedstoneState {
    pub(super) updater: Updater,
    /// Redstone torch toggles counting toward burnout.
    torch_toggles: Vec<(BlockPos, u64)>,
    /// Block ticks due in the batch now running that have not run yet.
    pub(super) ticks_this_tick: std::collections::HashSet<(BlockPos, Block)>,
    /// Containers whose contents may have changed since comparators were
    /// last told.
    pub(super) container_changes: std::collections::HashSet<BlockPos>,
    /// Snapshots of double chest halves owned by neighbouring regions
    /// (`None`: no container there).
    pub(super) far_halves: super::halves::Snapshots,
    /// Own chest halves whose snapshot a neighbour holds.
    pub(super) half_sent: std::collections::HashSet<BlockPos>,
    /// Item frames comparators may read: block, facing and signal by
    /// entity id, as of the last check.
    pub(super) frames: HashMap<i32, (BlockPos, Facing, u8)>,
    /// Blocks holding an item frame whose reading changed.
    pub(super) frame_changes: std::collections::HashSet<BlockPos>,
    /// Block events waiting for the block event phase, in queue order.
    pub(super) block_events: std::collections::VecDeque<super::pistons::BlockEventData>,
    /// Blocks pistons are moving, by moving piston position.
    pub(super) moving: HashMap<BlockPos, MovingBlock>,
    /// The moving blocks in the order they tick.
    pub(super) moving_order: Vec<BlockPos>,
    /// Whether scheduled block ticks are running.
    pub(super) handling_tick: bool,
    /// The orientation the neighbour update now running carries.
    pub(super) incoming: Option<Orientation>,
    /// Neighbour updates that arrived before the 3x3 around them loaded,
    /// replayed once it has.
    deferred: Vec<(BlockPos, Block)>,
    /// Command blocks whose command is running, with the chain budget.
    pub(super) command_blocks_pending: HashMap<BlockPos, u32>,
    /// Entity boxes neighbouring regions report reaching into this one.
    pub(super) foreign: HashMap<RegionPos, Vec<([f64; 6], bool)>>,
    /// Boxes a cart passed through this tick, while their blocks react.
    pass_boxes: Vec<[f64; 6]>,
    /// Regions this one last sent a non-empty footprint list.
    footprints_sent: std::collections::HashSet<RegionPos>,
    /// Blocks of other regions a piston changed during the block event
    /// running now, as that event must see them until the owner applies
    /// the change.
    pub(super) sent_changes: HashMap<BlockPos, BlockState>,
    /// Blocks this region's pistons started moving in other regions, with
    /// the tick each started, as the owner will move them.
    pub(super) remote_moving: HashMap<BlockPos, (MovingBlock, u64)>,
    /// Block entities moving across this region's borders.
    pub(super) carried: super::pistons::carried::CarriedState,
    /// The last tick the moving blocks moved.
    pub(super) moving_ticked: u64,
    /// How far pistons moved each entity (by id) and player along each
    /// axis this tick.
    pub(super) piston_deltas: HashMap<i32, [f64; 3]>,
    pub(super) piston_deltas_tick: u64,
    /// The game tick a neighbour's late updates (or a block event they
    /// queued) are being applied as of, while that is earlier than now.
    pub(super) as_of: Option<u64>,
    /// The blocks the late block event running now started moving.
    pub(super) catching_up: Option<Vec<BlockPos>>,
}

fn intersects(a: [f64; 6], b: [f64; 6]) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

impl RegionActor {
    /// Lifts entities and players standing inside the part of the block at
    /// `pos` that `old` left empty onto the block's top, now that it is a
    /// full block.
    fn push_entities_up(&mut self, pos: BlockPos, old: BlockState) {
        let floor = old
            .collision_boxes()
            .iter()
            .map(|b| f64::from(b[4]))
            .fold(0.0, f64::max);
        let [x, y, z] = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
        let gap = [x, y + floor, z, x + 1.0, y + 1.0, z + 1.0];
        let top = y + 1.0;
        if let Some(lifts) = &mut self.ents.lifts {
            lifts.push((gap, top));
        }
        for tracked in self.ents.entities.values_mut() {
            lift(tracked.entity.base_mut(), gap, top);
        }
        for presence in self.ents.presence.values() {
            let [px, py, pz] = presence.position;
            let bounds = [
                px - PLAYER_HALF_WIDTH,
                py,
                pz - PLAYER_HALF_WIDTH,
                px + PLAYER_HALF_WIDTH,
                py + presence.looks.height,
                pz + PLAYER_HALF_WIDTH,
            ];
            if presence.alive && !presence.spectator && in_gap(bounds, gap, top) {
                presence
                    .handle
                    .send(crate::world::messages::PlayerEvent::PushedUp {
                        dimension: self.dimension,
                        y: top,
                    });
            }
        }
    }
}

fn in_gap(bounds: [f64; 6], gap: [f64; 6], top: f64) -> bool {
    crate::world::physics::intersects(bounds, gap) && bounds[1] >= top - 1.0 && bounds[1] < top
}

/// Moves an entity standing in `gap` up to `top`.
pub(super) fn lift(base: &mut crate::world::entity::EntityBase, gap: [f64; 6], top: f64) {
    if in_gap(base.bounds(), gap, top) {
        base.position[1] = top;
        base.on_ground = true;
    }
}

pub(super) fn player_bounds([x, y, z]: [f64; 3]) -> [f64; 6] {
    [
        x - PLAYER_HALF_WIDTH,
        y,
        z - PLAYER_HALF_WIDTH,
        x + PLAYER_HALF_WIDTH,
        y + PLAYER_HEIGHT,
        z + PLAYER_HALF_WIDTH,
    ]
}

impl RegionActor {
    /// What `block` does in the update/signal cascade, per whichever
    /// feature filled `Registries::block_behaviors` (inert if none did).
    pub(super) fn block_behavior(&self, block: Block) -> &'static dyn BlockBehavior {
        self.shared.redstone_behaviors.get(block)
    }

    /// Sets a block the way the game does with `flags`: the old block's
    /// removal and the new one's placement reactions, then (per the flags)
    /// neighbour-changed updates, comparator updates and shape updates.
    /// Returns the old state, or `None` if the position is not loaded here.
    pub(super) fn set_block_flags(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        limit: u32,
    ) -> Option<BlockState> {
        if matches!(
            self.sim.cause,
            BlockCause::Block(_) | BlockCause::Entity { .. } | BlockCause::Weather
        ) && !self.shared.region_hooks.is_empty()
            && let Some(old) = RegionActor::block(self, pos)
            && old != state
        {
            let (cause, hooks) = (self.sim.cause, self.shared.region_hooks.clone());
            if !hooks
                .iter()
                .all(|hook| hook.allows_environment_change(self, pos, old, state, cause))
            {
                return Some(old);
            }
        }
        let old = self.raw_set_block(pos, state)?;
        if old == state {
            return Some(old);
        }
        if old.block() == state.block() {
            self.announce_toggle(pos, old, state);
        }
        let moved = flags & UPDATE_MOVED_BY_PISTON != 0;
        if flags & UPDATE_PUSH_ENTITIES != 0 && self.shared.block_rules.reverts_to_base(old, state)
        {
            self.push_entities_up(pos, old);
        }
        if old.block() != state.block() && (flags & UPDATE_NEIGHBORS != 0 || moved) {
            self.block_behavior(old.block())
                .after_removal(self, pos, old, moved);
        }
        let current = RegionActor::block(self, pos);
        if current.map(BlockState::block) == Some(state.block())
            && flags & UPDATE_SKIP_ON_PLACE == 0
        {
            self.block_behavior(state.block())
                .on_place(self, pos, state, old, moved);
            if RegionActor::block(self, pos) == Some(state)
                && self.shared.growth_behaviors.has_placed(state.block())
            {
                let behaviours = self.shared.growth_behaviors.clone();
                behaviours.placed(self, pos, state);
            }
            if let Some(delay) = self.shared.block_rules.place_tick(state.block()) {
                RedstoneLevel::schedule_tick(self, pos, state.block(), delay, TickPriority::Normal);
            }
        }
        if RegionActor::block(self, pos) != Some(state) {
            return Some(old);
        }
        if flags & UPDATE_NEIGHBORS != 0 {
            RedstoneLevel::update_neighbors_at(self, pos, old.block());
            if state.has_analog_output() {
                redstone::update_neighbour_for_output_signal(self, pos, state.block());
            }
        }
        if flags & UPDATE_KNOWN_SHAPE == 0 && limit > 0 {
            let flags = flags & !(UPDATE_NEIGHBORS | UPDATE_SUPPRESS_DROPS);
            self.block_behavior(old.block()).update_indirect_shapes(
                self,
                pos,
                old,
                flags,
                limit - 1,
            );
            for direction in SHAPE_UPDATE_ORDER {
                self.add_and_run(Update::Shape {
                    direction: direction.opposite(),
                    neighbour: state,
                    pos: step(pos, direction),
                    neighbour_pos: pos,
                    flags,
                    limit: limit - 1,
                });
            }
            self.block_behavior(state.block()).update_indirect_shapes(
                self,
                pos,
                state,
                flags,
                limit - 1,
            );
        }
        Some(old)
    }

    fn add_and_run(&mut self, update: Update) {
        let limit = self.shared.live.max_chained_neighbor_updates();
        let updater = &mut self.sim.redstone.updater;
        let running = updater.count > 0;
        let too_many = limit >= 0 && updater.count >= limit as usize;
        updater.count += 1;
        if !too_many {
            if running {
                updater.added.push(update);
            } else {
                updater.stack.push(update);
            }
        } else if updater.count - 1 == limit as usize {
            log::warn!("too many chained neighbour updates; skipping the rest from {update:?}");
        }
        if !running {
            self.run_updates();
        }
    }

    fn run_updates(&mut self) {
        loop {
            let updater = &mut self.sim.redstone.updater;
            if updater.stack.is_empty() && updater.added.is_empty() {
                updater.count = 0;
                let remote = std::mem::take(&mut updater.remote);
                self.send_block_updates(remote);
                return;
            }
            let added = std::mem::take(&mut updater.added);
            updater.stack.extend(added.into_iter().rev());
            loop {
                let updater = &mut self.sim.redstone.updater;
                let Some(top) = updater.stack.last_mut() else {
                    break;
                };
                let (action, more) = top.step();
                self.run_action(action);
                let updater = &mut self.sim.redstone.updater;
                if !more {
                    updater.stack.pop();
                    break;
                }
                if !updater.added.is_empty() {
                    break;
                }
            }
        }
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::Neighbor {
                pos,
                source,
                orientation,
            } => {
                self.debug_event(
                    tuff_vanilla_data::registry::DebugSubscription::NeighborUpdates,
                    pos,
                    crate::protocol::play::DebugValue::Pos(pos),
                );
                self.sim.redstone.incoming = orientation;
                self.run_neighbor_changed(pos, source);
                self.sim.redstone.incoming = None;
            }
            Action::Shape {
                direction,
                neighbour,
                pos,
                neighbour_pos,
                flags,
                limit,
            } => self.run_shape_update(direction, pos, neighbour_pos, neighbour, flags, limit),
        }
    }

    pub(super) fn queue_remote(&mut self, pos: BlockPos, update: BlockUpdate) {
        let region = ChunkPos::of_block(pos).region();
        self.sim
            .redstone
            .updater
            .remote
            .entry(region)
            .or_default()
            .push(update);
    }

    /// Whether redstone may react at `pos`: the chunks within one chunk of
    /// it are all loaded here or in the halo.
    pub(super) fn redstone_ready(&self, pos: BlockPos) -> bool {
        let chunk = ChunkPos::of_block(pos);
        self.columns.contains_key(&chunk) && self.neighbourhood_loaded(chunk)
    }

    fn run_neighbor_changed(&mut self, pos: BlockPos, source: Block) {
        let chunk = ChunkPos::of_block(pos);
        if !self.pos.contains(chunk) {
            self.queue_remote(
                pos,
                BlockUpdate::Neighbour {
                    pos,
                    source,
                    cause: self.sim.cause,
                },
            );
            return;
        }
        let Some(state) = RegionActor::block(self, pos) else {
            return;
        };
        if !is_air(state) {
            self.fluid_neighbour_changed(pos);
        }
        let Some(state) = RegionActor::block(self, pos) else {
            return;
        };
        if self.shared.growth_behaviors.has_block_update(state.block()) {
            let behaviours = self.shared.growth_behaviors.clone();
            behaviours.block_update(self, pos, state, source);
            return;
        }
        let behaviour = self.block_behavior(state.block());
        if (state.block().component().is_some() || behaviour.is_active())
            && !self.redstone_ready(pos)
        {
            if self.sim.redstone.deferred.len() < MAX_DEFERRED && self.columns.contains_key(&chunk)
            {
                self.sim.redstone.deferred.push((pos, source));
            }
            return;
        }
        behaviour.neighbor_changed(self, pos, state, source);
    }

    /// Replays the neighbour updates dropped while their surroundings were
    /// still loading.
    pub(super) fn replay_deferred_updates(&mut self) {
        let _span = tuff_timings::span!("region.deferred_updates");
        if self.sim.redstone.deferred.is_empty() {
            return;
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.sim.redstone.deferred)
            .into_iter()
            .partition(|(pos, _)| self.redstone_ready(*pos));
        self.sim.redstone.deferred = waiting
            .into_iter()
            .filter(|(pos, _)| self.columns.contains_key(&ChunkPos::of_block(*pos)))
            .collect();
        for (pos, source) in ready {
            self.run_neighbor_changed(pos, source);
        }
    }

    /// Destroys a block a shape update removed: particles, drops unless
    /// suppressed, and the fluid it held left behind.
    pub(super) fn destroy_by_update(
        &mut self,
        pos: BlockPos,
        old: BlockState,
        drops: bool,
        limit: u32,
    ) {
        let saved = self.saved_block_entity(pos);
        self.break_event(pos, old, None);
        if drops {
            let stacks = self
                .shared
                .block_loot()
                .destroyed(old, saved.as_ref(), &mut self.rng);
            self.drop_items(pos, stacks);
        }
        self.set_block_flags(pos, placement::remains(old), redstone::UPDATE_ALL, limit);
    }

    /// Runs a redstone block tick.
    pub(super) fn redstone_tick(&mut self, pos: BlockPos, state: BlockState) {
        self.block_behavior(state.block()).tick(self, pos, state);
    }

    /// The game tick changes made now count as made on: the current one,
    /// or an earlier one while a neighbour's late updates apply.
    pub(super) fn now(&self) -> u64 {
        self.sim.redstone.as_of.unwrap_or(self.sched.tick)
    }

    pub(super) fn schedule_tick_with(
        &mut self,
        pos: BlockPos,
        block: Block,
        delay: u32,
        priority: i32,
    ) {
        let order = self.sim.next_tick_order();
        let due = self.now() + u64::from(delay);
        if let Some(column) = self.columns.get_mut(&ChunkPos::of_block(pos)) {
            column.block_ticks.schedule(Scheduled {
                pos,
                kind: block,
                due,
                priority,
                order,
            });
            column.blocks_dirty = true;
        }
    }

    /// Applies a neighbour-changed update another region sent.
    pub(super) fn receive_neighbour(&mut self, pos: BlockPos, source: Block) {
        RedstoneLevel::neighbor_changed(self, pos, source);
    }

    /// Applies a piston's change to a block this region owns, from its own
    /// piston or a neighbour's.
    pub(super) fn apply_piston_set(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        drops: bool,
        moving: Option<MovingBlock>,
    ) {
        if drops {
            let old = Reader::block(self, pos);
            RedstoneLevel::effect(self, Effect::DropResources(pos, old));
        }
        RedstoneLevel::set_block(self, pos, state, flags);
        if let Some(moving) = moving {
            self.begin_move(pos, moving);
        }
    }

    /// Sends the updates for other regions queued outside a cascade.
    pub(super) fn flush_remote_updates(&mut self) {
        self.sim.redstone.sent_changes.clear();
        if self.sim.redstone.updater.count == 0 {
            let remote = std::mem::take(&mut self.sim.redstone.updater.remote);
            self.send_block_updates(remote);
        }
    }

    /// Applies a shape update another region sent.
    pub(super) fn receive_shape(
        &mut self,
        pos: BlockPos,
        direction: Facing,
        neighbour: BlockState,
        flags: u32,
        limit: u32,
    ) {
        let neighbour_pos = step(pos, direction);
        RedstoneLevel::shape_update(self, direction, pos, neighbour_pos, neighbour, flags, limit);
    }

    pub(super) fn container_slots_at(
        &self,
        pos: BlockPos,
        state: BlockState,
    ) -> Vec<&Option<ItemStack>> {
        self.joined_parts(pos, state)
            .into_iter()
            .filter_map(|part| self.part_items(part))
            .flat_map(|items| items.iter())
            .collect()
    }

    pub(super) fn minecart_readings(&self, area: [f64; 6]) -> Vec<redstone::CartReading<'_>> {
        let mut carts: Vec<(i32, &dyn crate::world::entity::RegionEntity)> = self
            .ents
            .entities
            .iter()
            .filter(|(_, tracked)| {
                tracked.entity.rides_rails() && intersects(tracked.entity.bounds(), area)
            })
            .map(|(id, tracked)| (*id, &*tracked.entity))
            .collect();
        carts.sort_unstable_by_key(|(id, _)| *id);
        carts
            .into_iter()
            .map(|(_, cart)| redstone::CartReading {
                command_success: (cart.base().kind
                    == tuff_vanilla_data::registry::EntityType::CommandBlockMinecart)
                    .then(|| cart.base().extra.get_int("SuccessCount").unwrap_or(0)),
                slots: cart.container().and_then(|slots| slots.as_slice()),
            })
            .collect()
    }

    /// Notes item frames whose comparator reading changed since the last
    /// check (turned, given or emptied, placed, gone or moved), so
    /// comparators behind them are told on the next flush.
    pub(super) fn check_item_frames(&mut self) {
        let _span = tuff_timings::span!("region.item_frames");
        let mut seen = HashMap::new();
        for (id, tracked) in &self.ents.entities {
            if let Some((facing, signal)) = tracked.entity.comparator_frame() {
                let [x, y, z] = tracked.entity.base().position;
                let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                seen.insert(*id, (pos, facing, signal));
            }
        }
        if seen.is_empty() && self.sim.redstone.frames.is_empty() {
            return;
        }
        for (id, frame) in &seen {
            match self.sim.redstone.frames.remove(id) {
                Some(old) if old == *frame => {}
                Some(old) => {
                    self.sim.redstone.frame_changes.insert(old.0);
                    self.sim.redstone.frame_changes.insert(frame.0);
                }
                None => {
                    self.sim.redstone.frame_changes.insert(frame.0);
                }
            }
        }
        for (_, (pos, _, _)) in self.sim.redstone.frames.drain() {
            self.sim.redstone.frame_changes.insert(pos);
        }
        self.sim.redstone.frames = seen;
    }

    /// Tells comparators reading containers whose contents changed.
    pub(super) fn flush_container_changes(&mut self) {
        if !self.sim.redstone.frame_changes.is_empty() {
            let mut changed: Vec<BlockPos> = self.sim.redstone.frame_changes.drain().collect();
            changed.sort_by_key(|pos| (pos.x, pos.y, pos.z));
            for pos in changed {
                if self.redstone_ready(pos) {
                    redstone::update_neighbour_for_output_signal(self, pos, Block::Air);
                }
            }
        }
        if self.sim.redstone.container_changes.is_empty() {
            return;
        }
        let mut changed: Vec<BlockPos> = self.sim.redstone.container_changes.drain().collect();
        changed.sort_by_key(|pos| (pos.x, pos.y, pos.z));
        self.share_halves(&changed);
        for pos in changed {
            let Some(state) = RegionActor::block(self, pos) else {
                continue;
            };
            if !is_air(state) && self.redstone_ready(pos) {
                redstone::update_neighbour_for_output_signal(self, pos, state.block());
            }
        }
    }

    /// Tells each neighbouring region the boxes of this region's entities
    /// that reach into it, so plates across the border feel them.
    pub(super) fn share_footprints(&mut self) {
        let _span = tuff_timings::span!("region.footprints");
        let mut by_region: HashMap<RegionPos, Vec<([f64; 6], bool)>> = HashMap::new();
        for tracked in self.ents.entities.values() {
            let bounds = tracked.entity.bounds();
            let living = tracked.entity.living().is_some();
            let corner = |x: f64, z: f64| ChunkPos::of_position([x, 0.0, z]).region();
            let mut regions = [
                corner(bounds[0], bounds[2]),
                corner(bounds[3], bounds[2]),
                corner(bounds[0], bounds[5]),
                corner(bounds[3], bounds[5]),
            ];
            regions.sort_by_key(|r| (r.x, r.z));
            for (index, region) in regions.iter().enumerate() {
                if *region != self.pos && (index == 0 || regions[index - 1] != *region) {
                    by_region.entry(*region).or_default().push((bounds, living));
                }
            }
        }
        let stale: Vec<RegionPos> = self
            .sim
            .redstone
            .footprints_sent
            .iter()
            .filter(|region| !by_region.contains_key(*region))
            .copied()
            .collect();
        for region in stale {
            self.sim.redstone.footprints_sent.remove(&region);
            if let Some(neighbor) = self.border.neighbors.get(&region) {
                neighbor.internal(RegionInternal::Footprints {
                    from: self.pos,
                    boxes: Vec::new(),
                });
            }
        }
        for (region, boxes) in by_region {
            if let Some(neighbor) = self.border.neighbors.get(&region) {
                self.sim.redstone.footprints_sent.insert(region);
                neighbor.internal(RegionInternal::Footprints {
                    from: self.pos,
                    boxes,
                });
            }
        }
    }

    /// Positions of plates, buttons and entity-sensing blocks the boxes touch.
    fn sensors_touched(&self, boxes: Vec<[f64; 6]>) -> Vec<BlockPos> {
        let mut touched = Vec::new();
        const EPSILON: f64 = 1.0e-5;
        for [x0, y0, z0, x1, y1, z1] in boxes {
            let (x0, y0, z0) = (
                (x0 + EPSILON).floor() as i32,
                (y0 + EPSILON).floor() as i32,
                (z0 + EPSILON).floor() as i32,
            );
            let (x1, y1, z1) = (
                (x1 - EPSILON).floor() as i32,
                (y1 - EPSILON).floor() as i32,
                (z1 - EPSILON).floor() as i32,
            );
            for x in x0..=x1 {
                for y in y0..=y1 {
                    for z in z0..=z1 {
                        let pos = BlockPos::new(x, y, z);
                        let Some(state) = RegionActor::block(self, pos) else {
                            continue;
                        };
                        if self.block_behavior(state.block()).senses_entities()
                            && !touched.contains(&pos)
                        {
                            touched.push(pos);
                        }
                    }
                }
            }
        }
        touched
    }

    /// Lets blocks react to a cart that moved through `boxes` this tick,
    /// counting those boxes as carts touching meanwhile.
    pub(super) fn entities_passed_through(&mut self, boxes: Vec<[f64; 6]>) {
        let touched = self.sensors_touched(boxes.clone());
        self.sim.redstone.pass_boxes = boxes;
        for pos in touched {
            if !self.owns(pos) || !self.redstone_ready(pos) {
                continue;
            }
            let Some(state) = RegionActor::block(self, pos) else {
                continue;
            };
            self.block_behavior(state.block())
                .entity_inside(self, pos, state);
        }
        self.sim.redstone.pass_boxes.clear();
    }

    /// Lets redstone blocks react to the entities touching them: plates
    /// and arrow-pressed buttons.
    pub(super) fn entities_inside_blocks(&mut self, players_only: bool) {
        let _span = tuff_timings::span!("region.entities_inside_blocks");
        let mut boxes: Vec<[f64; 6]> = Vec::new();
        if !players_only {
            boxes.extend(
                self.ents
                    .entities
                    .values()
                    .map(|tracked| tracked.entity.bounds()),
            );
            boxes.extend(
                self.sim
                    .redstone
                    .foreign
                    .values()
                    .flatten()
                    .map(|(b, _)| *b),
            );
        }
        boxes.extend(
            self.ents
                .presence
                .values()
                .filter(|presence| presence.is_physical())
                .map(|presence| player_bounds(presence.position)),
        );
        let touched = self.sensors_touched(boxes);
        for pos in touched {
            if !self.owns(pos) || !self.redstone_ready(pos) {
                continue;
            }
            let Some(state) = RegionActor::block(self, pos) else {
                continue;
            };
            self.block_behavior(state.block())
                .entity_inside(self, pos, state);
        }
    }

    /// Every 20 game ticks, runs the slow tick of each ready block holding
    /// block entity data whose behaviour has one.
    pub(super) fn run_slow_ticks(&mut self) {
        let _span = tuff_timings::span!("region.slow_ticks");
        if !self.sched.tick.is_multiple_of(SLOW_TICK_INTERVAL) {
            return;
        }
        let mut positions: Vec<BlockPos> = Vec::new();
        for column in self.columns.values() {
            if column.unloading {
                continue;
            }
            for pos in column.meta.raw_block_entities.keys() {
                let state = column.chunk.get(pos.x, pos.y, pos.z);
                if self.block_behavior(state.block()).has_slow_tick() {
                    positions.push(*pos);
                }
            }
        }
        positions.sort_by_key(|p| (p.y, p.z, p.x));
        for pos in positions {
            if !self.owns(pos) || !self.redstone_ready(pos) {
                continue;
            }
            let Some(state) = RegionActor::block(self, pos) else {
                continue;
            };
            self.block_behavior(state.block())
                .slow_tick(self, pos, state);
        }
    }

    /// Lightning struck the block at `pos`.
    pub(super) fn lightning_struck_block(&mut self, pos: BlockPos) {
        if !self.owns(pos) || !self.redstone_ready(pos) {
            return;
        }
        let Some(state) = RegionActor::block(self, pos) else {
            return;
        };
        self.block_behavior(state.block())
            .struck_by_lightning(self, pos, state);
    }

    /// A projectile struck the target block at `hit.pos`: it emits a signal
    /// the stronger the nearer the hit is to the face's centre.
    pub(super) fn target_hit(
        &mut self,
        hit: crate::world::raycast::Hit,
        impact: &crate::world::entity::ProjectileImpact,
    ) {
        let Some(state) = RegionActor::block(self, hit.pos) else {
            return;
        };
        if !self.owns(hit.pos) {
            return;
        }
        let Some((strength, duration)) =
            self.block_behavior(state.block())
                .hit_signal(hit.face, hit.location, impact.arrow)
        else {
            return;
        };
        if let Some(owner) = impact.owner.as_ref().filter(|o| o.is_player())
            && let Some(player) = self.seen_player(owner.id)
        {
            player.send(crate::world::messages::PlayerEvent::AwardStat(
                tuff_vanilla_data::registry::CustomStat::TargetHit,
                1,
            ));
            player.send(crate::world::messages::PlayerEvent::Advancement(Box::new(
                crate::world::messages::AdvancementEvent::TargetHit {
                    projectile: crate::world::predicate::EntityFacts {
                        kind: Some(impact.kind),
                        position: hit.location,
                        dimension: self.dimension().key.clone(),
                        ..Default::default()
                    },
                    signal: i32::from(strength),
                },
            )));
        }
        if RedstoneLevel::has_scheduled_tick(self, hit.pos, state.block()) {
            return;
        }
        if let Some(powered) = state.with_power(strength) {
            RedstoneLevel::set_block(self, hit.pos, powered, redstone::UPDATE_ALL);
            RedstoneLevel::schedule_tick(
                self,
                hit.pos,
                state.block(),
                duration,
                TickPriority::Normal,
            );
        }
    }

    /// Tells the neighbours of a block a command set without updates, as
    /// the game does after `/setblock` and `/fill`.
    pub(super) fn notify_block_set(&mut self, pos: BlockPos, old: BlockState) {
        let Some(state) = RegionActor::block(self, pos) else {
            return;
        };
        if old.block() != state.block() {
            self.block_behavior(old.block())
                .after_removal(self, pos, old, false);
        }
        RedstoneLevel::update_neighbors_at(self, pos, state.block());
        if state.has_analog_output() {
            redstone::update_neighbour_for_output_signal(self, pos, state.block());
        }
    }

    /// Rings the bell at `pos`, struck from `face` or its facing.
    pub(super) fn ring_bell(&mut self, pos: BlockPos, face: Option<Facing>) {
        let Some(state) = RegionActor::block(self, pos) else {
            return;
        };
        let Some(face) = face.or(state.facing()) else {
            return;
        };
        RegionActor::block_event(self, pos, play::block_event::BELL_RING, face.id() as u8);
        self.play_sound(
            tuff_vanilla_data::registry::SoundEvent::BlockBellUse,
            pos,
            2.0,
            1.0,
            None,
        );
        crate::world::region_api::RegionWorld::bell_rung(self, pos);
        crate::world::area::broadcast(
            self,
            crate::world::area::around(pos.center(), crate::world::region_api::BELL_HEARING),
            move |world| world.bell_rung(pos),
        );
        let hooks = self.shared.region_hooks.clone();
        for hook in hooks.iter() {
            hook.bell_rung(self, pos);
        }
    }

    /// The block a player right-clicked, when its redstone behaviour uses
    /// the click.
    pub(super) fn use_redstone(&mut self, pos: BlockPos, state: BlockState) -> bool {
        state.block().component().is_some()
            && self.block_behavior(state.block()).used(self, pos, state)
    }

    /// A placed state adjusted to the power around it.
    pub(super) fn redstone_placement(&self, pos: BlockPos, state: BlockState) -> BlockState {
        self.block_behavior(state.block())
            .placement(self, pos, state)
    }

    /// Runs after a player placed a block.
    pub(super) fn redstone_placed(&mut self, pos: BlockPos, state: BlockState) {
        self.block_behavior(state.block()).placed(self, pos, state);
    }
}
