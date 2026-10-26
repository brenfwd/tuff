//! Pistons and sticky pistons: extending and retracting on power (read
//! with quasi-connectivity, one block above counts too), the structure
//! they push (at most 12 blocks, slime and honey dragging their
//! neighbours along), and the piston head.
//!
//! A piston reacts in two steps like the game: an update decides it should
//! move and queues a block event; the event (run in the tick's block event
//! phase) re-checks power and moves the blocks, turning each into a moving
//! piston that lands two ticks later. Blocks a neighbouring region owns
//! move too (read through the halo, changed by message, so their part of
//! the move runs a tick late); an unloaded block does not move.

use tuff_vanilla_data::block::{Facing, PushReaction};
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use super::{
    BlockBehavior, DIRECTIONS, Effect, RedstoneLevel, SHAPE_UPDATE_ORDER, SignalBehavior, Signals,
    UPDATE_CLIENTS, UPDATE_INVISIBLE, UPDATE_KNOWN_SHAPE, UPDATE_LIMIT, UPDATE_MOVED_BY_PISTON,
    UPDATE_NEIGHBORS, UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS,
};
use tuff_core::world::redstone::MovingBlock;
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct Base;
pub struct Head;
pub struct Slime;
pub struct Honey;

/// The most blocks one piston moves.
pub const PUSH_LIMIT: usize = 12;

const EVENT_EXTEND: u8 = 0;
const EVENT_RETRACT: u8 = 1;
/// Retracting while the block it pushed is still on its way out: a sticky
/// piston then leaves that block where it lands.
const EVENT_DROP: u8 = 2;

/// Flags a piston's own state change and a moving block's start use.
const SET_MOVING: u32 =
    UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS | UPDATE_MOVED_BY_PISTON | UPDATE_INVISIBLE;
const SET_QUIET: u32 =
    UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS | UPDATE_KNOWN_SHAPE | UPDATE_INVISIBLE;
const SET_EXTENDED: u32 = UPDATE_MOVED_BY_PISTON | UPDATE_CLIENTS | UPDATE_NEIGHBORS;
const LEAVE_BEHIND: u32 = UPDATE_MOVED_BY_PISTON | UPDATE_KNOWN_SHAPE | UPDATE_CLIENTS;
const DESTROYED: u32 = UPDATE_KNOWN_SHAPE | UPDATE_CLIENTS;

fn offset(pos: BlockPos, direction: Facing, distance: i32) -> BlockPos {
    let (x, y, z) = direction.step();
    BlockPos::new(
        pos.x + x * distance,
        pos.y + y * distance,
        pos.z + z * distance,
    )
}

fn is_piston(state: BlockState) -> bool {
    matches!(state.block(), Block::Piston | Block::StickyPiston)
}

fn is_air(state: BlockState) -> bool {
    matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir)
}

fn piston_type(sticky: bool) -> &'static str {
    if sticky { "sticky" } else { "normal" }
}

fn moving_piston(facing: Facing, sticky: bool) -> BlockState {
    let state = Block::MovingPiston.default_state();
    let state = state.with_facing(facing).unwrap_or(state);
    state.with("type", piston_type(sticky)).unwrap_or(state)
}

/// Whether the piston at `pos` facing `facing` is powered: from any side
/// but its front, or into the block above it.
fn powered(level: &dyn RedstoneLevel, pos: BlockPos, facing: Facing) -> bool {
    let signals = Signals::new(level);
    if DIRECTIONS
        .into_iter()
        .any(|d| d != facing && signals.has_signal(step(pos, d), d))
    {
        return true;
    }
    if signals.has_signal(pos, Facing::Down) {
        return true;
    }
    let above = step(pos, Facing::Up);
    DIRECTIONS
        .into_iter()
        .any(|d| d != Facing::Down && signals.has_signal(step(above, d), d))
}

/// The reads deciding what a piston move takes along.
pub trait PushView {
    fn reachable_block(&self, pos: BlockPos) -> Option<BlockState>;
    fn build_limits(&self) -> (i32, i32);
    fn within_border(&self, pos: BlockPos) -> bool;
    fn carries_block_entity(&self, state: BlockState, pos: BlockPos, direction: Facing) -> bool;
}

impl<T: RedstoneLevel + ?Sized> PushView for T {
    fn reachable_block(&self, pos: BlockPos) -> Option<BlockState> {
        RedstoneLevel::reachable_block(self, pos)
    }
    fn build_limits(&self) -> (i32, i32) {
        RedstoneLevel::build_limits(self)
    }
    fn within_border(&self, pos: BlockPos) -> bool {
        RedstoneLevel::within_border(self, pos)
    }
    fn carries_block_entity(&self, state: BlockState, pos: BlockPos, direction: Facing) -> bool {
        RedstoneLevel::carries_block_entity(self, state, pos, direction)
    }
}

/// Whether a piston may move `state` at `pos` toward `direction`;
/// `destroyable` lets blocks that break when pushed count, and blocks that
/// only take pushes move only when `direction` is `connection`.
pub fn is_pushable<L: PushView + ?Sized>(
    level: &L,
    state: BlockState,
    pos: BlockPos,
    direction: Facing,
    destroyable: bool,
    connection: Facing,
) -> bool {
    let (min_y, max_y) = level.build_limits();
    if pos.y < min_y || pos.y > max_y || !level.within_border(pos) {
        return false;
    }
    if is_air(state) {
        return true;
    }
    if (direction == Facing::Down && pos.y == min_y) || (direction == Facing::Up && pos.y == max_y)
    {
        return false;
    }
    if !is_piston(state) {
        if state.destroy_speed().is_none() {
            return false;
        }
        match state.block().push_reaction() {
            PushReaction::Immovable => return false,
            PushReaction::Popped => return destroyable,
            PushReaction::Push => return direction == connection,
            PushReaction::PushPull | PushReaction::IgnoreEntity => {}
        }
    }
    state.extended() != Some(true)
        && (state.block().block_entity().is_none()
            || level.carries_block_entity(state, pos, direction))
}

fn is_sticky(state: BlockState) -> bool {
    matches!(state.block(), Block::SlimeBlock | Block::HoneyBlock)
}

fn stick_together(a: BlockState, b: BlockState) -> bool {
    match (a.block(), b.block()) {
        (Block::HoneyBlock, Block::SlimeBlock) | (Block::SlimeBlock, Block::HoneyBlock) => false,
        _ => is_sticky(a) || is_sticky(b),
    }
}

/// A move that cannot happen: blocked, too big or reaching unloaded blocks.
struct Halt;

type Resolved<T> = Result<T, Halt>;

/// The blocks one piston move takes along and breaks.
pub struct Structure<'a, L: PushView + ?Sized = dyn RedstoneLevel + 'a> {
    level: &'a L,
    piston: BlockPos,
    facing: Facing,
    extending: bool,
    start: BlockPos,
    push: Facing,
    /// Blocks to move, in the order the game lists them.
    pub to_push: Vec<BlockPos>,
    /// Blocks the move breaks.
    pub to_destroy: Vec<BlockPos>,
}

impl<'a, L: PushView + ?Sized> Structure<'a, L> {
    pub fn new(level: &'a L, piston: BlockPos, facing: Facing, extending: bool) -> Self {
        let (push, start) = if extending {
            (facing, step(piston, facing))
        } else {
            (facing.opposite(), offset(piston, facing, 2))
        };
        Self {
            level,
            piston,
            facing,
            extending,
            start,
            push,
            to_push: Vec::new(),
            to_destroy: Vec::new(),
        }
    }

    /// Works out what moves; false when the move is blocked. An unloaded
    /// block blocks the move.
    pub fn resolve(&mut self) -> bool {
        self.to_push.clear();
        self.to_destroy.clear();
        self.gather().is_ok()
    }

    fn read(&self, pos: BlockPos) -> Resolved<BlockState> {
        self.level.reachable_block(pos).ok_or(Halt)
    }

    fn movable(&self, state: BlockState, pos: BlockPos, breakable: bool, from: Facing) -> bool {
        is_pushable(self.level, state, pos, self.push, breakable, from)
    }

    fn room_for(&self, extra: usize) -> Resolved<()> {
        if self.to_push.len() + extra > PUSH_LIMIT {
            Err(Halt)
        } else {
            Ok(())
        }
    }

    fn gather(&mut self) -> Resolved<()> {
        let front = self.read(self.start)?;
        if !self.movable(front, self.start, false, self.facing) {
            let breaks = self.extending && front.block().push_reaction() == PushReaction::Popped;
            return if breaks {
                self.to_destroy.push(self.start);
                Ok(())
            } else {
                Err(Halt)
            };
        }
        self.claim_column(self.start, self.push)?;
        self.drag_from(0, usize::MAX)
    }

    /// Runs `drag_sideways` for every listed block from `first` up to
    /// `last` (inclusive), re-reading the list length as it grows.
    fn drag_from(&mut self, first: usize, last: usize) -> Resolved<()> {
        let mut index = first;
        while let Some(&pos) = self.to_push.get(index)
            && index <= last
        {
            if is_sticky(self.read(pos)?) {
                self.drag_sideways(pos)?;
            }
            index += 1;
        }
        Ok(())
    }

    /// Counts the sticky run behind `origin` (away from the push) that
    /// comes along with it, `origin` included.
    fn trailing_run(&self, origin: BlockPos, origin_state: BlockState) -> Resolved<usize> {
        let behind = self.push.opposite();
        let mut run = 1;
        let mut ahead_state = origin_state;
        while is_sticky(ahead_state) {
            let pos = offset(origin, behind, run as i32);
            let here = self.read(pos)?;
            let joins = !is_air(here)
                && stick_together(ahead_state, here)
                && self.movable(here, pos, false, behind)
                && pos != self.piston;
            if !joins {
                break;
            }
            run += 1;
            self.room_for(run)?;
            ahead_state = here;
        }
        Ok(run)
    }

    /// Lists the column through `origin`, entered from a block toward
    /// `entered`: the sticky run behind it, then whatever it shoves in
    /// front up to air or a block that breaks.
    fn claim_column(&mut self, origin: BlockPos, entered: Facing) -> Resolved<()> {
        let origin_state = self.read(origin)?;
        let skip = is_air(origin_state)
            || !self.movable(origin_state, origin, false, entered)
            || origin == self.piston
            || self.to_push.contains(&origin);
        if skip {
            return Ok(());
        }
        self.room_for(1)?;
        let run = self.trailing_run(origin, origin_state)?;
        let behind = self.push.opposite();
        self.to_push
            .extend((0..run).rev().map(|k| offset(origin, behind, k as i32)));
        for distance in 1.. {
            let fresh = run + (distance as usize - 1);
            let pos = offset(origin, self.push, distance);
            if let Some(hit) = self.to_push.iter().position(|p| *p == pos) {
                self.to_push[hit..].rotate_right(fresh);
                return self.drag_from(0, hit + fresh);
            }
            let state = self.read(pos)?;
            if is_air(state) {
                break;
            }
            if pos == self.piston || !self.movable(state, pos, true, self.push) {
                return Err(Halt);
            }
            if state.block().push_reaction() == PushReaction::Popped {
                self.to_destroy.push(pos);
                break;
            }
            self.room_for(1)?;
            self.to_push.push(pos);
        }
        Ok(())
    }

    /// Pulls in the columns stuck to the sides of the sticky block `from`.
    fn drag_sideways(&mut self, from: BlockPos) -> Resolved<()> {
        let glue = self.read(from)?;
        for side in DIRECTIONS {
            if side.axis() == self.push.axis() {
                continue;
            }
            let pos = step(from, side);
            if stick_together(self.read(pos)?, glue) {
                self.claim_column(pos, side)?;
            }
        }
        Ok(())
    }
}

/// The order the game's hash map of positions yields them in: by bucket of
/// a table sized for `max` entries, then insertion order.
fn hash_map_order(positions: &mut [(BlockPos, BlockState)], max: usize) {
    let mut capacity = 16;
    while max > capacity / 4 * 3 {
        capacity *= 2;
    }
    positions.sort_by_key(|(pos, _)| {
        let hash = pos
            .y
            .wrapping_add(pos.z.wrapping_mul(31))
            .wrapping_mul(31)
            .wrapping_add(pos.x);
        (hash ^ ((hash as u32) >> 16) as i32) as u32 as usize & (capacity - 1)
    });
}

/// Moves the structure in front of (or, retracting, two blocks out from)
/// the piston at `pos`; false when it cannot move.
fn move_blocks(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    facing: Facing,
    extending: bool,
    sticky: bool,
) -> bool {
    let arm = step(pos, facing);
    if !extending && level.block(arm).block() == Block::PistonHead {
        level.piston_set(
            arm,
            Block::Air.default_state(),
            SET_QUIET,
            false,
            None,
            None,
        );
    }
    let mut structure = Structure::new(level, pos, facing, extending);
    if !structure.resolve() {
        return false;
    }
    let Structure {
        to_push,
        to_destroy,
        push: direction,
        ..
    } = structure;
    let moved: Vec<BlockState> = to_push.iter().map(|p| level.block(*p)).collect();
    let mut left: Vec<(BlockPos, BlockState)> =
        to_push.iter().copied().zip(moved.iter().copied()).collect();
    let mut updated: Vec<BlockState> = Vec::with_capacity(to_push.len() + to_destroy.len());
    for &at in to_destroy.iter().rev() {
        let state = level.block(at);
        level.piston_set(at, Block::Air.default_state(), DESTROYED, true, None, None);
        updated.push(state);
    }
    level.lift_pushed(&to_push);
    for (i, &from) in to_push.iter().enumerate().rev() {
        let state = level.block(from);
        let to = step(from, direction);
        left.retain(|(p, _)| *p != to);
        let moving = MovingBlock::new(moved[i], facing, extending, false);
        let carry = to_push.contains(&to).then(|| step(to, direction));
        level.piston_set(
            to,
            moving_piston(facing, false),
            SET_MOVING,
            false,
            Some(moving),
            carry,
        );
        updated.push(state);
    }
    if extending {
        let head = Block::PistonHead.default_state();
        let head = head.with_facing(facing).unwrap_or(head);
        let head = head.with("type", piston_type(sticky)).unwrap_or(head);
        left.retain(|(p, _)| *p != arm);
        let moving = MovingBlock::new(head, facing, true, true);
        let carry = to_push.contains(&arm).then(|| step(arm, direction));
        level.piston_set(
            arm,
            moving_piston(facing, sticky),
            SET_MOVING,
            false,
            Some(moving),
            carry,
        );
    }
    level.settle_pushed();
    hash_map_order(&mut left, to_push.len());
    let air = Block::Air.default_state();
    for &(at, _) in &left {
        level.piston_set(
            at,
            air,
            LEAVE_BEHIND,
            false,
            None,
            Some(step(at, direction)),
        );
    }
    for &(at, old) in &left {
        super::behaviour(old.block()).update_indirect_shapes(
            level,
            at,
            old,
            UPDATE_CLIENTS,
            UPDATE_LIMIT - 1,
        );
        for d in SHAPE_UPDATE_ORDER {
            level.shape_update(
                d.opposite(),
                step(at, d),
                at,
                air,
                UPDATE_CLIENTS,
                UPDATE_LIMIT - 1,
            );
        }
    }
    for (&at, state) in to_destroy.iter().rev().zip(updated.iter()) {
        let behaviour = super::behaviour(state.block());
        behaviour.after_removal(level, at, *state, false);
        behaviour.update_indirect_shapes(level, at, *state, UPDATE_CLIENTS, UPDATE_LIMIT - 1);
        level.update_neighbors_at(at, state.block());
    }
    for (&at, state) in to_push
        .iter()
        .rev()
        .zip(updated.iter().skip(to_destroy.len()))
    {
        level.update_neighbors_at(at, state.block());
    }
    if extending {
        level.update_neighbors_at(arm, Block::PistonHead);
    }
    true
}

/// Removes what is left of the arm, leaving its fluid behind.
fn remove_arm(level: &mut dyn RedstoneLevel, arm: BlockPos) {
    // Parity: on a vanilla server a retraction queued before its piston was replaced
    // (or a headless piston) still clears unbreakable blocks such as bedrock in front
    // of it; tuff.blocks.permanentBlockBreaking=false keeps them standing.
    let standing = level.block(arm);
    if standing.destroy_speed().is_none()
        && standing.block() != Block::MovingPiston
        && !level.permanent_block_breaking()
    {
        return;
    }
    let left = tuff_vanilla_common::placement::remains(level.block(arm));
    level.piston_set(arm, left, super::UPDATE_ALL, false, None, None);
}

fn check_if_extend(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let Some(facing) = state.facing() else {
        return;
    };
    let extend = powered(level, pos, facing);
    let extended = state.extended() == Some(true);
    if extend && !extended {
        if Structure::new(level, pos, facing, true).resolve() {
            level.block_event(pos, state.block(), EVENT_EXTEND, facing.id() as u8);
        }
    } else if !extend && extended {
        let pushed = offset(pos, facing, 2);
        let mut event = EVENT_RETRACT;
        let pushed_state = level.block(pushed);
        if pushed_state.block() == Block::MovingPiston
            && pushed_state.facing() == Some(facing)
            && let Some(moving) = level.moving_block(pushed)
            && moving.extending
            && (moving.progress_before < 0.5
                || level.game_time() == moving.last_ticked
                || level.is_handling_tick())
        {
            event = EVENT_DROP;
        }
        level.block_event(pos, state.block(), event, facing.id() as u8);
    }
}

fn facing_from_id(id: u8) -> Facing {
    match id & 7 {
        0 => Facing::Down,
        1 => Facing::Up,
        2 => Facing::North,
        3 => Facing::South,
        4 => Facing::West,
        _ => Facing::East,
    }
}

impl BlockBehavior for Base {
    fn moving_arm(&self, moved: BlockState, facing: Facing, short: bool) -> Option<BlockState> {
        let head = Block::PistonHead.default_state();
        let head = head.with_short(short).unwrap_or(head);
        let head = head
            .with("type", piston_type(moved.block() == Block::StickyPiston))
            .unwrap_or(head);
        head.with_facing(facing)
    }

    fn arm_base(&self, moved: BlockState) -> Option<BlockState> {
        moved.with_extended(true)
    }

    fn is_active(&self) -> bool {
        true
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        check_if_extend(level, pos, state);
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() && level.moving_block(pos).is_none() {
            check_if_extend(level, pos, state);
        }
    }

    fn placed(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        check_if_extend(level, pos, state);
    }

    fn trigger_event(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        kind: u8,
        data: u8,
    ) -> bool {
        let Some(facing) = state.facing() else {
            return false;
        };
        let sticky = state.block() == Block::StickyPiston;
        let extended = state.with_extended(true).unwrap_or(state);
        let extend = powered(level, pos, facing);
        if extend && (kind == EVENT_RETRACT || kind == EVENT_DROP) {
            level.set_block(pos, extended, UPDATE_CLIENTS);
            return false;
        }
        if !extend && kind == EVENT_EXTEND {
            return false;
        }
        if kind == EVENT_EXTEND {
            if !move_blocks(level, pos, facing, true, sticky) {
                return false;
            }
            level.set_block(pos, extended, SET_EXTENDED);
            let pitch = level.random_float() * 0.25 + 0.6;
            level.effect(Effect::Sound {
                sound: SoundEvent::BlockPistonExtend,
                pos,
                volume: 0.5,
                pitch,
            });
            return true;
        }
        let arm = step(pos, facing);
        if level.moving_block(arm).is_some() {
            level.finish_moving(arm);
        }
        let moving = moving_piston(facing, sticky);
        level.set_block(pos, moving, SET_QUIET);
        let base = state.block().default_state();
        let base = base.with_facing(facing_from_id(data)).unwrap_or(base);
        level.start_moving(pos, MovingBlock::new(base, facing, false, true));
        level.update_neighbors_at(pos, Block::MovingPiston);
        for d in SHAPE_UPDATE_ORDER {
            level.shape_update(
                d.opposite(),
                step(pos, d),
                pos,
                moving,
                UPDATE_CLIENTS,
                UPDATE_LIMIT - 1,
            );
        }
        if sticky {
            let two = offset(pos, facing, 2);
            let pulled = level.block(two);
            let mut caught = false;
            if pulled.block() == Block::MovingPiston
                && let Some(moving) = level.moving_block(two)
                && moving.facing == facing
                && moving.extending
            {
                level.finish_moving(two);
                caught = true;
            }
            if !caught {
                if kind == EVENT_RETRACT
                    && !is_air(pulled)
                    && is_pushable(level, pulled, two, facing.opposite(), false, facing)
                    && (pulled.block().push_reaction() == PushReaction::PushPull
                        || is_piston(pulled))
                {
                    move_blocks(level, pos, facing, false, sticky);
                } else {
                    remove_arm(level, arm);
                }
            }
        } else {
            remove_arm(level, arm);
        }
        let pitch = level.random_float() * 0.15 + 0.6;
        level.effect(Effect::Sound {
            sound: SoundEvent::BlockPistonContract,
            pos,
            volume: 0.5,
            pitch,
        });
        true
    }
}

impl SignalBehavior for Base {}

fn fitting_base(head: BlockState, base: BlockState) -> bool {
    let block = if head.get("type") == Some("sticky") {
        Block::StickyPiston
    } else {
        Block::Piston
    };
    base.block() == block && base.extended() == Some(true) && base.facing() == head.facing()
}

fn head_survives(level: &dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
    let Some(facing) = state.facing() else {
        return false;
    };
    let base = level.block(step(pos, facing.opposite()));
    fitting_base(state, base)
        || (base.block() == Block::MovingPiston && base.facing() == Some(facing))
}

impl BlockBehavior for Head {
    fn moving_arm(&self, moved: BlockState, facing: Facing, short: bool) -> Option<BlockState> {
        moved.with_facing(facing)?.with_short(short)
    }

    fn is_active(&self) -> bool {
        true
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        source: Block,
    ) {
        if head_survives(level, pos, state)
            && let Some(facing) = state.facing()
        {
            level.neighbor_changed(step(pos, facing.opposite()), source);
        }
    }

    fn creative_break(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        let Some(facing) = state.facing() else {
            return;
        };
        let base = step(pos, facing.opposite());
        if fitting_base(state, level.block(base)) {
            level.destroy_block(base, false);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _moved: bool,
    ) {
        let Some(facing) = state.facing() else {
            return;
        };
        let base = step(pos, facing.opposite());
        if fitting_base(state, level.block(base)) {
            level.destroy_block(base, true);
        }
    }

    fn update_shape(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        _neighbour: BlockState,
    ) -> Option<BlockState> {
        if Some(direction.opposite()) == state.facing() && !head_survives(level, pos, state) {
            Some(Block::Air.default_state())
        } else {
            None
        }
    }
}

impl SignalBehavior for Head {}

impl BlockBehavior for Slime {
    fn bounces_pushed(&self) -> bool {
        true
    }
}

impl SignalBehavior for Slime {}

impl BlockBehavior for Honey {
    fn drags_riders(&self) -> bool {
        true
    }
}

impl SignalBehavior for Honey {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Grid(HashMap<BlockPos, BlockState>);

    impl PushView for Grid {
        fn reachable_block(&self, pos: BlockPos) -> Option<BlockState> {
            if pos.x.abs() > 9 {
                return None;
            }
            Some(
                self.0
                    .get(&pos)
                    .copied()
                    .unwrap_or(Block::Air.default_state()),
            )
        }
        fn build_limits(&self) -> (i32, i32) {
            (-4, 4)
        }
        fn within_border(&self, pos: BlockPos) -> bool {
            pos.z.abs() <= 8
        }
        fn carries_block_entity(&self, _: BlockState, _: BlockPos, _: Facing) -> bool {
            false
        }
    }

    const PALETTE: [Block; 9] = [
        Block::Air,
        Block::Stone,
        Block::SlimeBlock,
        Block::HoneyBlock,
        Block::SlimeBlock,
        Block::Obsidian,
        Block::Torch,
        Block::WhiteGlazedTerracotta,
        Block::Piston,
    ];

    fn scene(seed: u64) -> (Grid, Facing, bool) {
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut cells = HashMap::new();
        for x in -4..=4 {
            for y in -3..=3 {
                for z in -3..=3 {
                    let roll = (next() % 13) as usize;
                    if roll < PALETTE.len() {
                        cells.insert(BlockPos::new(x, y, z), PALETTE[roll].default_state());
                    }
                }
            }
        }
        let facing = DIRECTIONS[(next() % 6) as usize];
        let extending = next() % 2 == 0;
        let piston = BlockPos::new(0, 0, 0);
        let base = Block::StickyPiston.default_state();
        let base = base.with_facing(facing).unwrap_or(base);
        cells.insert(piston, base);
        (Grid(cells), facing, extending)
    }

    fn digest(seed: u64) -> (u64, usize) {
        let (grid, facing, extending) = scene(seed);
        let piston = BlockPos::new(0, 0, 0);
        let mut structure = Structure::new(&grid, piston, facing, extending);
        let ok = structure.resolve();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |v: i64| {
            h ^= v as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        };
        feed(ok as i64);
        for list in [&structure.to_push, &structure.to_destroy] {
            feed(list.len() as i64);
            for p in list {
                feed(p.x as i64);
                feed(p.y as i64);
                feed(p.z as i64);
            }
        }
        (h, if ok { structure.to_push.len() } else { 0 })
    }

    #[tuff_core::watchdog]
    #[test]
    fn resolved_structures_match_the_recorded_order() {
        let mut all: u64 = 0;
        let mut moved = 0usize;
        for seed in 0..20_000 {
            let (h, pushed) = digest(seed);
            all = all.rotate_left(7) ^ h;
            moved += usize::from(pushed >= 4);
        }
        assert!(moved > 1000, "{moved}");
        assert_eq!(all, 16396825941340157849);
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "timing run: cargo test --release -p tuff-feature-builtin-vanilla-redstone --lib resolve_timing -- --ignored --nocapture"]
    fn resolve_timing() {
        let scenes: Vec<_> = (0..2000).map(scene).collect();
        let piston = BlockPos::new(0, 0, 0);
        let start = std::time::Instant::now();
        let mut total = 0usize;
        for _ in 0..200 {
            for (grid, facing, extending) in &scenes {
                let view: &dyn PushView = grid;
                let mut structure = Structure::new(view, piston, *facing, *extending);
                if structure.resolve() {
                    total += structure.to_push.len() + structure.to_destroy.len();
                }
            }
        }
        println!("resolve_timing: {:?} ({total})", start.elapsed());
    }
}
