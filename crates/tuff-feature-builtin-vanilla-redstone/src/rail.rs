//! Rails: how a rail shapes itself to join the rails around it, when it
//! falls off its support, and the power of powered, activator and detector
//! rails.

use tuff_vanilla_data::block::{Facing, Shape};
use tuff_vanilla_data::{Block, BlockState, Support};

use super::{
    BlockBehavior, Effect, RedstoneLevel, RedstoneLevelExt, SignalBehavior, Signals, TickPriority,
    UPDATE_ALL, update_neighbour_for_output_signal,
};
use tuff_core::world::redstone::Reader;
use tuff_world_model::BlockPos;

/// The plain rail, the only one that curves.
pub struct Rail;
/// Powered and activator rails: powered by a signal, or by a powered rail
/// of the same kind in line that is, up to 8 rails away.
pub struct PoweredRail;
pub struct DetectorRail;

const MAX_CHAIN: u32 = 8;
const DETECTOR_CHECK_TICKS: u32 = 20;

fn powered(state: BlockState) -> bool {
    state.powered() == Some(true)
}

fn is_slope(shape: Shape) -> bool {
    matches!(
        shape,
        Shape::AscendingEast | Shape::AscendingWest | Shape::AscendingNorth | Shape::AscendingSouth
    )
}

fn is_rail(block: Block) -> bool {
    matches!(
        block,
        Block::Rail | Block::PoweredRail | Block::DetectorRail | Block::ActivatorRail
    )
}

fn is_straight(block: Block) -> bool {
    block != Block::Rail
}

fn offset(pos: BlockPos, x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

fn above(pos: BlockPos) -> BlockPos {
    offset(pos, 0, 1, 0)
}

fn below(pos: BlockPos) -> BlockPos {
    offset(pos, 0, -1, 0)
}

fn rail_at(level: &dyn Reader, pos: BlockPos) -> bool {
    is_rail(level.block(pos).block())
}

fn supports_rigid(level: &dyn Reader, pos: BlockPos) -> bool {
    level.block(pos).is_face_sturdy(Facing::Up, Support::Rigid)
}

/// `supports_rigid`, where a block still landing in a piston's moving cell
/// holds a rail as its own face would.
fn supports_landing(level: &dyn RedstoneLevel, pos: BlockPos) -> bool {
    supports_rigid(level, pos)
        || (level.block(pos).block() == Block::MovingPiston
            && level.moving_block(pos).is_some_and(|moving| {
                !moving.source
                    && moving.progress >= 1.0
                    && moving.moved.is_face_sturdy(Facing::Up, Support::Rigid)
            }))
}

const NORTH: u8 = 1;
const SOUTH: u8 = 2;
const WEST: u8 = 4;
const EAST: u8 = 8;
const ALONG_Z: u8 = NORTH | SOUTH;
const ALONG_X: u8 = WEST | EAST;

/// Each horizontal side as a bit and its step on x and z.
const SIDES: [(u8, i32, i32); 4] = [(NORTH, 0, -1), (SOUTH, 0, 1), (WEST, -1, 0), (EAST, 1, 0)];

/// The step to each end of a rail shape: (dx, dy, dz) for both ends.
fn end_steps(shape: Shape) -> Option<[(i32, i32, i32); 2]> {
    Some(match shape {
        Shape::NorthSouth => [(0, 0, -1), (0, 0, 1)],
        Shape::EastWest => [(-1, 0, 0), (1, 0, 0)],
        Shape::AscendingEast => [(-1, 0, 0), (1, 1, 0)],
        Shape::AscendingWest => [(-1, 1, 0), (1, 0, 0)],
        Shape::AscendingNorth => [(0, 1, -1), (0, 0, 1)],
        Shape::AscendingSouth => [(0, 0, -1), (0, 1, 1)],
        Shape::SouthEast => [(1, 0, 0), (0, 0, 1)],
        Shape::SouthWest => [(-1, 0, 0), (0, 0, 1)],
        Shape::NorthWest => [(-1, 0, 0), (0, 0, -1)],
        Shape::NorthEast => [(1, 0, 0), (0, 0, -1)],
        _ => return None,
    })
}

/// The two cells a rail of `shape` at `pos` leads to.
fn ends(pos: BlockPos, shape: Shape) -> impl Iterator<Item = BlockPos> {
    end_steps(shape)
        .into_iter()
        .flatten()
        .map(move |(x, y, z)| offset(pos, x, y, z))
}

/// The curve joining exactly the two sides in `sides`, if they turn a corner.
fn corner(sides: u8) -> Option<Shape> {
    match sides {
        s if s == SOUTH | EAST => Some(Shape::SouthEast),
        s if s == SOUTH | WEST => Some(Shape::SouthWest),
        s if s == NORTH | WEST => Some(Shape::NorthWest),
        s if s == NORTH | EAST => Some(Shape::NorthEast),
        _ => None,
    }
}

/// Which curve a rail at a junction takes, most preferred first: a powered
/// junction turns toward the north-west, an unpowered one toward the
/// south-east.
const POWERED_TURNS: [(u8, Shape); 4] = [
    (NORTH | WEST, Shape::NorthWest),
    (NORTH | EAST, Shape::NorthEast),
    (SOUTH | WEST, Shape::SouthWest),
    (SOUTH | EAST, Shape::SouthEast),
];
const UNPOWERED_TURNS: [(u8, Shape); 4] = [
    (SOUTH | EAST, Shape::SouthEast),
    (SOUTH | WEST, Shape::SouthWest),
    (NORTH | EAST, Shape::NorthEast),
    (NORTH | WEST, Shape::NorthWest),
];

/// The flat shape a rail takes once it links to the cells on `sides`.
fn linked_shape(sides: u8, curves: bool) -> Option<Shape> {
    if curves && let Some(turn) = corner(sides) {
        return Some(turn);
    }
    if sides & ALONG_X != 0 {
        Some(Shape::EastWest)
    } else if sides & ALONG_Z != 0 {
        Some(Shape::NorthSouth)
    } else {
        None
    }
}

/// The flat shape a freshly placed rail picks among the rails on `sides`.
fn chosen_shape(sides: u8, curves: bool, powered: bool, current: Shape) -> Option<Shape> {
    if curves && let Some(turn) = corner(sides) {
        return Some(turn);
    }
    match (sides & ALONG_Z != 0, sides & ALONG_X != 0) {
        (false, false) => None,
        (true, false) => Some(Shape::NorthSouth),
        (false, true) => Some(Shape::EastWest),
        (true, true) if curves => {
            let turns = if powered {
                &POWERED_TURNS
            } else {
                &UNPOWERED_TURNS
            };
            turns
                .iter()
                .find(|(pair, _)| sides & pair == *pair)
                .map(|&(_, turn)| turn)
                .or(Some(current))
        }
        (true, true) => Some(current),
    }
}

/// Tilts a flat straight rail up toward a rail one level higher at either
/// end; the south and west ends win a tie.
fn tilted(level: &dyn Reader, pos: BlockPos, flat: Shape) -> Shape {
    let ramps: [((i32, i32), Shape); 2] = match flat {
        Shape::NorthSouth => [
            ((0, 1), Shape::AscendingSouth),
            ((0, -1), Shape::AscendingNorth),
        ],
        Shape::EastWest => [
            ((-1, 0), Shape::AscendingWest),
            ((1, 0), Shape::AscendingEast),
        ],
        _ => return flat,
    };
    ramps
        .into_iter()
        .find(|&((x, z), _)| rail_at(level, offset(pos, x, 1, z)))
        .map_or(flat, |(_, ramp)| ramp)
}

/// A rail and the cells it links to while shaping.
struct Track {
    at: BlockPos,
    state: BlockState,
    curves: bool,
    links: [BlockPos; 3],
    linked: usize,
}

impl Track {
    fn new(at: BlockPos, state: BlockState) -> Self {
        let mut track = Self {
            at,
            state,
            curves: !is_straight(state.block()),
            links: [at; 3],
            linked: 0,
        };
        track.relink(state.shape());
        track
    }

    fn relink(&mut self, shape: Option<Shape>) {
        self.linked = 0;
        if let Some(shape) = shape {
            for end in ends(self.at, shape) {
                self.link(end);
            }
        }
    }

    fn link(&mut self, cell: BlockPos) {
        self.links[self.linked] = cell;
        self.linked += 1;
    }

    fn links(&self) -> &[BlockPos] {
        &self.links[..self.linked]
    }

    /// The rail at `pos`, or a level above or below it.
    fn find(level: &dyn Reader, pos: BlockPos) -> Option<Self> {
        [pos, above(pos), below(pos)].into_iter().find_map(|at| {
            let state = level.block(at);
            is_rail(state.block()).then(|| Self::new(at, state))
        })
    }

    fn leads_to(&self, pos: BlockPos) -> bool {
        self.links().iter().any(|c| c.x == pos.x && c.z == pos.z)
    }

    /// Bits for the sides this rail links toward.
    fn linked_sides(&self) -> u8 {
        SIDES
            .iter()
            .filter(|&&(_, x, z)| self.leads_to(offset(self.at, x, 0, z)))
            .fold(0, |sides, &(bit, _, _)| sides | bit)
    }

    /// Keeps only the links whose cell holds a rail leading back here,
    /// moved to that rail's level.
    fn drop_one_way_links(&mut self, level: &dyn Reader) {
        let mut kept = 0;
        for i in 0..self.linked {
            if let Some(rail) = Self::find(level, self.links[i])
                && rail.leads_to(self.at)
            {
                self.links[kept] = rail.at;
                kept += 1;
            }
        }
        self.linked = kept;
    }

    /// Whether this rail could take `other` as a link: it already does, or
    /// it has a free end.
    fn accepts(&self, other: BlockPos) -> bool {
        self.leads_to(other) || self.linked != 2
    }

    fn count_potential_connections(&self, level: &dyn Reader) -> usize {
        SIDES
            .iter()
            .filter(|&&(_, x, z)| {
                let side = offset(self.at, x, 0, z);
                rail_at(level, side) || rail_at(level, above(side)) || rail_at(level, below(side))
            })
            .count()
    }

    /// Bits for the sides holding a rail willing to link to this one.
    fn willing_sides(&self, level: &dyn Reader) -> u8 {
        let mut sides = 0;
        for &(bit, x, z) in &SIDES {
            if let Some(mut rail) = Self::find(level, offset(self.at, x, 0, z)) {
                rail.drop_one_way_links(level);
                if rail.accepts(self.at) {
                    sides |= bit;
                }
            }
        }
        sides
    }

    fn reshape(&mut self, shape: Shape) {
        self.state = self.state.with_shape(shape).unwrap_or(self.state);
    }

    /// Takes `other` as a link and reshapes to fit.
    fn join(&mut self, level: &mut dyn RedstoneLevel, other: BlockPos) {
        self.link(other);
        let shape = linked_shape(self.linked_sides(), self.curves)
            .map_or(Shape::NorthSouth, |flat| tilted(level, self.at, flat));
        self.reshape(shape);
        level.set_block(self.at, self.state, UPDATE_ALL);
    }

    /// Shapes the rail to the rails around it and lets them link to it.
    fn settle(mut self, level: &mut dyn RedstoneLevel, powered: bool, first: bool) -> Self {
        let current = self.state.shape().unwrap_or(Shape::NorthSouth);
        let sides = self.willing_sides(level);
        let shape = chosen_shape(sides, self.curves, powered, current)
            .map_or(current, |flat| tilted(level, self.at, flat));
        self.relink(Some(shape));
        self.reshape(shape);
        if !first && level.block(self.at) == self.state {
            return self;
        }
        level.set_block(self.at, self.state, UPDATE_ALL);
        let links = self.links;
        for &end in &links[..self.linked] {
            if let Some(mut rail) = Self::find(level, end) {
                rail.drop_one_way_links(level);
                if rail.accepts(self.at) {
                    rail.join(level, self.at);
                }
            }
        }
        self
    }
}

fn update_dir(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    state: BlockState,
    first: bool,
) -> BlockState {
    let signal = Signals::new(level).has_neighbor_signal(pos);
    Track::new(pos, state).settle(level, signal, first).state
}

/// The placement reaction every rail shares: it shapes itself, and a
/// straight rail then reacts to its own change.
fn update_placed(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> BlockState {
    let shaped = update_dir(level, pos, state, true);
    if is_straight(state.block()) {
        level.neighbor_changed(pos, state.block());
    }
    shaped
}

/// Whether the rail has lost what holds it: the block below, or the
/// block a slope leans on.
fn unsupported(level: &dyn RedstoneLevel, pos: BlockPos, shape: Shape) -> bool {
    if !supports_landing(level, below(pos)) {
        return true;
    }
    let leans_on = match shape {
        Shape::AscendingEast => offset(pos, 1, 0, 0),
        Shape::AscendingWest => offset(pos, -1, 0, 0),
        Shape::AscendingNorth => offset(pos, 0, 0, -1),
        Shape::AscendingSouth => offset(pos, 0, 0, 1),
        _ => return false,
    };
    !supports_rigid(level, leans_on)
}

/// The neighbour reaction every rail shares; false when the rail is gone
/// (replaced, or dropped for want of support).
fn still_there(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
    if level.block(pos).block() != state.block() {
        return false;
    }
    let Some(shape) = state.shape() else {
        return false;
    };
    if unsupported(level, pos, shape) {
        level.effect(Effect::DropResources(pos, state));
        level.remove_block(pos);
        return false;
    }
    true
}

/// Whether the rail at `pos` gets power along its line, looking toward
/// south or west when `forward`.
fn find_signal(s: &Signals, pos: BlockPos, state: BlockState, forward: bool, depth: u32) -> bool {
    if depth >= MAX_CHAIN {
        return false;
    }
    let Some(shape) = state.shape() else {
        return false;
    };
    let (mut x, mut y, mut z) = (pos.x, pos.y, pos.z);
    let mut check_below = true;
    let line = match shape {
        Shape::NorthSouth => {
            z += if forward { 1 } else { -1 };
            Shape::NorthSouth
        }
        Shape::EastWest => {
            x += if forward { -1 } else { 1 };
            Shape::EastWest
        }
        Shape::AscendingEast => {
            if forward {
                x -= 1;
            } else {
                x += 1;
                y += 1;
                check_below = false;
            }
            Shape::EastWest
        }
        Shape::AscendingWest => {
            if forward {
                x -= 1;
                y += 1;
                check_below = false;
            } else {
                x += 1;
            }
            Shape::EastWest
        }
        Shape::AscendingNorth => {
            if forward {
                z += 1;
            } else {
                z -= 1;
                y += 1;
                check_below = false;
            }
            Shape::NorthSouth
        }
        Shape::AscendingSouth => {
            if forward {
                z += 1;
                y += 1;
                check_below = false;
            } else {
                z -= 1;
            }
            Shape::NorthSouth
        }
        _ => return false,
    };
    let next = BlockPos::new(x, y, z);
    same_rail_with_power(s, state.block(), next, forward, depth, line)
        || (check_below
            && same_rail_with_power(s, state.block(), below(next), forward, depth, line))
}

fn same_rail_with_power(
    s: &Signals,
    block: Block,
    pos: BlockPos,
    forward: bool,
    depth: u32,
    line: Shape,
) -> bool {
    let state = s.block(pos);
    if state.block() != block {
        return false;
    }
    let Some(shape) = state.shape() else {
        return false;
    };
    let crosses = match line {
        Shape::EastWest => matches!(
            shape,
            Shape::NorthSouth | Shape::AscendingNorth | Shape::AscendingSouth
        ),
        Shape::NorthSouth => matches!(
            shape,
            Shape::EastWest | Shape::AscendingEast | Shape::AscendingWest
        ),
        _ => false,
    };
    if crosses || !powered(state) {
        return false;
    }
    s.has_neighbor_signal(pos) || find_signal(s, pos, state, forward, depth + 1)
}

fn update_powered(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let should = {
        let s = Signals::new(level);
        s.has_neighbor_signal(pos)
            || find_signal(&s, pos, state, true, 0)
            || find_signal(&s, pos, state, false, 0)
    };
    if should == powered(state) {
        return;
    }
    level.set_block(pos, state.with_powered(should).unwrap_or(state), UPDATE_ALL);
    level.update_neighbors_at(below(pos), state.block());
    if state.shape().is_some_and(is_slope) {
        level.update_neighbors_at(above(pos), state.block());
    }
}

fn after_rail_removal(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    state: BlockState,
    moved: bool,
) {
    if moved {
        return;
    }
    let block = state.block();
    if state.shape().is_some_and(is_slope) {
        level.update_neighbors_at(above(pos), block);
    }
    if is_straight(block) {
        level.update_neighbors_at(pos, block);
        level.update_neighbors_at(below(pos), block);
    }
}

impl BlockBehavior for Rail {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            update_placed(level, pos, state);
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        source: Block,
    ) {
        if !still_there(level, pos, state) {
            return;
        }
        if source.default_state().is_signal_source()
            && Track::new(pos, state).count_potential_connections(level) == 3
        {
            update_dir(level, pos, state, false);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        after_rail_removal(level, pos, state, moved);
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Rail {}

impl BlockBehavior for PoweredRail {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            update_placed(level, pos, state);
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        if still_there(level, pos, state) {
            update_powered(level, pos, state);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        after_rail_removal(level, pos, state, moved);
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for PoweredRail {}

fn check_pressed(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    if !supports_landing(level, below(pos)) || level.block(pos).block() != state.block() {
        return;
    }
    let area = [
        f64::from(pos.x) + 0.2,
        f64::from(pos.y),
        f64::from(pos.z) + 0.2,
        f64::from(pos.x + 1) - 0.2,
        f64::from(pos.y + 1) - 0.2,
        f64::from(pos.z + 1) - 0.2,
    ];
    let pressed = level.minecarts_touching(area) > 0;
    let block = state.block();
    if pressed != powered(state) {
        let next = state.with_powered(pressed).unwrap_or(state);
        level.set_block(pos, next, UPDATE_ALL);
        if let Some(shape) = next.shape() {
            for connection in ends(pos, shape) {
                let connected = level.block(connection).block();
                level.neighbor_changed(connection, connected);
            }
        }
        level.update_neighbors_at(pos, block);
        level.update_neighbors_at(below(pos), block);
    }
    if pressed {
        level.schedule_tick(pos, block, DETECTOR_CHECK_TICKS, TickPriority::Normal);
    }
    update_neighbour_for_output_signal(level, pos, block);
}

impl BlockBehavior for DetectorRail {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block() {
            let shaped = update_placed(level, pos, state);
            check_pressed(level, pos, shaped);
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        still_there(level, pos, state);
    }

    fn entity_inside(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if !powered(state) {
            check_pressed(level, pos, state);
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if powered(state) {
            check_pressed(level, pos, state);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        after_rail_removal(level, pos, state, moved);
    }

    fn senses_entities(&self) -> bool {
        true
    }

    fn is_active(&self) -> bool {
        true
    }

    fn analog_output(
        &self,
        level: &dyn Reader,
        pos: BlockPos,
        state: BlockState,
        _direction: Facing,
    ) -> Option<u8> {
        Some(crate::comparator::detector_rail_signal(level, pos, state))
    }
}

impl SignalBehavior for DetectorRail {}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod tests;
