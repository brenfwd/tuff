//! Tripwire hooks and the string strung between them: two hooks facing
//! each other with unbroken tripwire between attach, and anything
//! touching an armed wire powers both hooks.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState, Support};

use super::{
    BlockBehavior, Effect, Reader, RedstoneLevel, SignalBehavior, TickPriority, UPDATE_ALL,
};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct Hook;
pub struct Wire;

/// The longest span, hook to hook.
const MAX_DISTANCE: i32 = 42;
const RECHECK_TICKS: u32 = 10;
const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

fn flag(value: Option<bool>) -> bool {
    value == Some(true)
}

fn offset(pos: BlockPos, facing: Facing, distance: i32) -> BlockPos {
    let (x, y, z) = facing.step();
    BlockPos::new(
        pos.x + x * distance,
        pos.y + y * distance,
        pos.z + z * distance,
    )
}

fn side_name(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "north",
        Facing::East => "east",
        Facing::South => "south",
        _ => "west",
    }
}

fn sound(level: &mut dyn RedstoneLevel, pos: BlockPos, sound: SoundEvent, pitch: f32) {
    level.effect(Effect::Sound {
        sound,
        pos,
        volume: 0.4,
        pitch,
    });
}

/// Plays the one sound that best describes the change from `before` to
/// `now`, each an (attached, powered) pair; a power change wins.
fn emit_state(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    now: (bool, bool),
    before: (bool, bool),
) {
    let (sound_event, pitch) = match (now.1 != before.1, now.0 != before.0) {
        (true, _) if now.1 => (SoundEvent::BlockTripwireClickOn, 0.6),
        (true, _) => (SoundEvent::BlockTripwireClickOff, 0.5),
        (false, true) if now.0 => (SoundEvent::BlockTripwireAttach, 0.7),
        (false, true) => (
            SoundEvent::BlockTripwireDetach,
            1.2 / (level.random_float() * 0.2 + 0.9),
        ),
        (false, false) => return,
    };
    sound(level, pos, sound_event, pitch);
}

fn notify(level: &mut dyn RedstoneLevel, pos: BlockPos, facing: Facing) {
    level.update_neighbors_at(pos, Block::TripwireHook);
    level.update_neighbors_at(step(pos, facing.opposite()), Block::TripwireHook);
}

/// What lies along a hook's line of sight: the string pieces met on the way
/// (indexed by distance), and the distance to a partner hook facing back.
struct Span {
    pieces: [Option<BlockState>; MAX_DISTANCE as usize],
    partner: Option<i32>,
    unbroken: bool,
    tripped: bool,
}

impl Span {
    /// Walks out from the hook at `pos`. `source`, when set, overrides the
    /// world at that distance with a wire state that is about to be there.
    fn survey(
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        facing: Facing,
        source: Option<(i32, BlockState)>,
    ) -> Self {
        let mut span = Span {
            pieces: [None; MAX_DISTANCE as usize],
            partner: None,
            unbroken: true,
            tripped: false,
        };
        let back = facing.opposite();
        for distance in 1..MAX_DISTANCE {
            let found = level.block(offset(pos, facing, distance));
            let overridden = source.and_then(|(at, s)| (at == distance).then_some(s));
            let piece = match (found.block(), overridden) {
                (Block::TripwireHook, _) => {
                    span.partner = (found.facing() == Some(back)).then_some(distance);
                    break;
                }
                (_, Some(s)) => s,
                (Block::Tripwire, None) => found,
                _ => {
                    span.unbroken = false;
                    continue;
                }
            };
            let armed = !flag(piece.disarmed());
            span.tripped |= armed && flag(piece.powered());
            span.pieces[distance as usize] = Some(piece);
            if overridden.is_some() {
                level.schedule_tick(
                    pos,
                    Block::TripwireHook,
                    RECHECK_TICKS,
                    TickPriority::Normal,
                );
                span.unbroken &= armed;
            }
        }
        span
    }
}

fn hook_state(facing: Facing, attached: bool, powered: bool) -> BlockState {
    let base = Block::TripwireHook.default_state();
    let state = base
        .with_attached(attached)
        .and_then(|s| s.with_powered(powered))
        .unwrap_or(base);
    state.with_facing(facing).unwrap_or(state)
}

/// Recomputes the hook at `pos` and the hook it faces: whether the span
/// between them is whole (attached) and tripped (powered).
fn calculate(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    state: BlockState,
    destroyed: bool,
    can_update: bool,
    source: Option<(i32, BlockState)>,
) {
    let Some(facing) = state.facing() else {
        return;
    };
    let before = (flag(state.attached()), flag(state.powered()));
    let span = Span::survey(level, pos, facing, source);
    let reach = span.partner.unwrap_or(0);
    let attached = !destroyed && span.unbroken && reach > 1;
    let now = (attached, span.tripped && attached);

    if let Some(distance) = span.partner {
        let back = facing.opposite();
        let far = offset(pos, facing, distance);
        level.set_block(far, hook_state(back, now.0, now.1), UPDATE_ALL);
        notify(level, far, back);
        if level.block(pos).block() != Block::TripwireHook {
            return removed(level, pos, hook_state(facing, now.0, now.1));
        }
        emit_state(level, far, now, before);
    }
    emit_state(level, pos, now, before);
    if !destroyed {
        level.set_block(pos, hook_state(facing, now.0, now.1), UPDATE_ALL);
        if can_update {
            notify(level, pos, facing);
        }
    }
    if before.0 == attached {
        return;
    }
    for (distance, piece) in span
        .pieces
        .iter()
        .enumerate()
        .take(reach.max(0) as usize)
        .skip(1)
    {
        let Some(piece) = *piece else { continue };
        let at = offset(pos, facing, distance as i32);
        if matches!(
            level.block(at).block(),
            Block::Tripwire | Block::TripwireHook
        ) {
            level.set_block(
                at,
                piece.with_attached(attached).unwrap_or(piece),
                UPDATE_ALL,
            );
        }
    }
}

fn removed(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let powered = flag(state.powered());
    if flag(state.attached()) || powered {
        calculate(level, pos, state, true, false, None);
    }
    if powered && let Some(facing) = state.facing() {
        notify(level, pos, facing);
    }
}

impl BlockBehavior for Hook {
    fn placed(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        calculate(level, pos, state, false, false, None);
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        calculate(level, pos, state, false, true, None);
    }

    fn update_shape(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        (state.facing().map(Facing::opposite) == Some(direction)
            && !neighbour.is_face_sturdy(direction.opposite(), Support::Full))
        .then(|| Block::Air.default_state())
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved {
            removed(level, pos, state);
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Hook {}

/// Tells the hooks the wire at `pos` hangs between that it is now `state`.
fn update_source(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    for direction in [Facing::South, Facing::West] {
        for i in 1..MAX_DISTANCE {
            let at = offset(pos, direction, i);
            let found = level.block(at);
            if found.block() == Block::TripwireHook {
                if found.facing() == Some(direction.opposite()) {
                    calculate(level, at, found, false, true, Some((i, state)));
                }
                break;
            }
            if found.block() != Block::Tripwire {
                break;
            }
        }
    }
}

fn connects_to(neighbour: BlockState, direction: Facing) -> bool {
    match neighbour.block() {
        Block::TripwireHook => neighbour.facing() == Some(direction.opposite()),
        Block::Tripwire => true,
        _ => false,
    }
}

fn with_side(state: BlockState, direction: Facing, on: bool) -> BlockState {
    state
        .with(side_name(direction), if on { "true" } else { "false" })
        .unwrap_or(state)
}

/// The box an entity must touch to trip the wire at `pos`.
fn touch_box(pos: BlockPos, state: BlockState) -> [f64; 6] {
    let (bottom, top) = if flag(state.attached()) {
        (1.0 / 16.0, 2.5 / 16.0)
    } else {
        (0.0, 8.0 / 16.0)
    };
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    [x, y + bottom, z, x + 1.0, y + top, z + 1.0]
}

fn check_pressed(level: &mut dyn RedstoneLevel, pos: BlockPos) {
    let state = level.block(pos);
    if state.block() != Block::Tripwire {
        return;
    }
    let was = flag(state.powered());
    let pressed = level.entities_touching(touch_box(pos, state), false) > 0;
    if pressed != was {
        let next = state.with_powered(pressed).unwrap_or(state);
        level.set_block(pos, next, UPDATE_ALL);
        update_source(level, pos, next);
    }
    if pressed {
        level.schedule_tick(pos, Block::Tripwire, RECHECK_TICKS, TickPriority::Normal);
    } else if was {
        level.schedule_tick(pos, Block::Tripwire, 1, TickPriority::Normal);
    }
}

impl BlockBehavior for Wire {
    fn placement(&self, level: &dyn Reader, pos: BlockPos, state: BlockState) -> BlockState {
        HORIZONTAL.into_iter().fold(state, |state, direction| {
            with_side(
                state,
                direction,
                connects_to(level.block(step(pos, direction)), direction),
            )
        })
    }

    fn update_shape(
        &self,
        _level: &mut dyn RedstoneLevel,
        _pos: BlockPos,
        state: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        HORIZONTAL
            .contains(&direction)
            .then(|| with_side(state, direction, connects_to(neighbour, direction)))
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            update_source(level, pos, state);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved {
            update_source(level, pos, state.with_powered(true).unwrap_or(state));
        }
    }

    fn entity_inside(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if !flag(state.powered()) && !level.has_scheduled_tick(pos, Block::Tripwire) {
            check_pressed(level, pos);
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, _state: BlockState) {
        if flag(level.block(pos).powered()) {
            check_pressed(level, pos);
        }
    }

    fn senses_entities(&self) -> bool {
        true
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Wire {}

#[cfg(test)]
#[path = "tripwire_tests.rs"]
mod tests;
