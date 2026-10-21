//! Nether and end portals as pure functions of a block lookup: frame
//! detection when fire is lit, where a travelling player comes out, how a
//! missing exit portal is built, and the end's portal ring and arrival
//! platform. Regions apply the results through their block-writing path.

use std::collections::HashSet;

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::tag::block::{FIRE, NETHER_PORTAL_FRAME};
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const MIN_WIDTH: i32 = 2;
const MAX_WIDTH: i32 = 21;
const MIN_HEIGHT: i32 = 3;
const MAX_HEIGHT: i32 = 21;
/// Blocks a search for an existing exit portal covers on each side.
pub const NETHER_PORTAL_RADIUS: i32 = 16;
pub const OVERWORLD_PORTAL_RADIUS: i32 = 128;
/// Columns searched around the exit point for a spot for a new portal.
const CREATE_RADIUS: i32 = 16;
/// Ticks after arriving during which a portal does not send a player back.
pub const PLAYER_PORTAL_COOLDOWN: u32 = 10;
/// Where players arrive in the end, and the platform built under them.
pub const END_SPAWN: BlockPos = BlockPos::new(100, 50, 0);
pub const PLAYER_WIDTH: f64 = PLAYER_HALF_WIDTH * 2.0;
const AIR: BlockState = Block::Air.default_state();

/// A nether portal's horizontal axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Z,
}

impl Axis {
    pub fn of(state: BlockState) -> Option<Self> {
        match state.get("axis")? {
            "x" => Some(Self::X),
            "z" => Some(Self::Z),
            _ => None,
        }
    }

    pub fn of_facing(facing: Facing) -> Self {
        match facing {
            Facing::East | Facing::West => Self::X,
            _ => Self::Z,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::X => Self::Z,
            Self::Z => Self::X,
        }
    }

    /// The direction the game walks a frame from its bottom-left corner.
    fn right(self) -> Facing {
        match self {
            Self::X => Facing::West,
            Self::Z => Facing::South,
        }
    }

    pub fn positive(self) -> Facing {
        match self {
            Self::X => Facing::East,
            Self::Z => Facing::South,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Z => "z",
        }
    }

    pub fn portal_state(self) -> BlockState {
        Block::NetherPortal
            .default_state()
            .with("axis", self.name())
            .expect("nether portal axis")
    }

    fn coordinate(self, pos: BlockPos) -> i32 {
        match self {
            Self::X => pos.x,
            Self::Z => pos.z,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalKind {
    Nether,
    End,
    Gateway,
}

/// A portal block a player's body overlaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalContact {
    pub kind: PortalKind,
    pub pos: BlockPos,
}

impl PortalContact {
    /// The portal a region reported the body inside.
    pub fn of(contact: tuff_core::world::contact::PortalContact) -> Option<Self> {
        if contact.claimed {
            return None;
        }
        let kind = match contact.block.block() {
            Block::NetherPortal => PortalKind::Nether,
            Block::EndPortal => PortalKind::End,
            Block::EndGateway => PortalKind::Gateway,
            _ => return None,
        };
        Some(Self {
            kind,
            pos: contact.pos,
        })
    }
}

/// The largest rectangle of portal blocks around one of them: `min` is its
/// bottom corner on the negative side of `axis`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalRectangle {
    pub min: BlockPos,
    pub axis: Axis,
    pub width: i32,
    pub height: i32,
}

pub fn step(pos: BlockPos, direction: Facing, count: i32) -> BlockPos {
    let (dx, dy, dz) = direction.step();
    BlockPos::new(pos.x + dx * count, pos.y + dy * count, pos.z + dz * count)
}

pub fn is_frame(state: BlockState) -> bool {
    NETHER_PORTAL_FRAME.contains(&state.block())
}

fn is_empty(state: BlockState) -> bool {
    is_air(state) || FIRE.contains(&state.block()) || state.block() == Block::NetherPortal
}

/// The frame around a position, as the game measures it: the bottom-left
/// inner block, the inner width and height, and how many portal blocks
/// already fill it. Invalid frames have a zero width or height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub axis: Axis,
    pub bottom_left: BlockPos,
    pub width: i32,
    pub height: i32,
    pub portal_blocks: i32,
}

impl Shape {
    fn invalid(axis: Axis, pos: BlockPos) -> Self {
        Self {
            axis,
            bottom_left: pos,
            width: 0,
            height: 0,
            portal_blocks: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        (MIN_WIDTH..=MAX_WIDTH).contains(&self.width)
            && (MIN_HEIGHT..=MAX_HEIGHT).contains(&self.height)
    }

    pub fn is_complete(&self) -> bool {
        self.is_valid() && self.portal_blocks == self.width * self.height
    }

    /// Every inner position, with the state that fills it.
    pub fn portal_blocks(&self) -> impl Iterator<Item = (BlockPos, BlockState)> + '_ {
        let state = self.axis.portal_state();
        let right = self.axis.right();
        (0..self.height).flat_map(move |up| {
            (0..self.width).map(move |across| {
                let pos = step(step(self.bottom_left, Facing::Up, up), right, across);
                (pos, state)
            })
        })
    }
}

/// The empty, valid frame around `pos`, trying `preferred` first.
pub fn find_empty_shape(
    level: &dyn Fn(BlockPos) -> BlockState,
    pos: BlockPos,
    min_y: i32,
    preferred: Axis,
) -> Option<Shape> {
    [preferred, preferred.other()]
        .into_iter()
        .map(|axis| find_any_shape(level, pos, min_y, axis))
        .find(|shape| shape.is_valid() && shape.portal_blocks == 0)
}

pub fn find_any_shape(
    level: &dyn Fn(BlockPos) -> BlockState,
    pos: BlockPos,
    min_y: i32,
    axis: Axis,
) -> Shape {
    let probe = FrameProbe {
        level,
        along: axis.right(),
    };
    let Some(corner) = probe.inner_corner(pos, min_y) else {
        return Shape::invalid(axis, pos);
    };
    let mut shape = Shape::invalid(axis, corner);
    shape.width = probe.ledge_span(corner, probe.along);
    if !(MIN_WIDTH..=MAX_WIDTH).contains(&shape.width) {
        shape.width = 0;
        return shape;
    }
    let (rows, filled) = probe.open_rows(corner, shape.width);
    shape.portal_blocks = filled;
    if (MIN_HEIGHT..=MAX_HEIGHT).contains(&rows) && probe.lintel_closed(corner, shape.width, rows) {
        shape.height = rows;
    }
    shape
}

/// Block lookups for one frame orientation; `along` walks the frame's
/// inner rows away from its starting corner.
struct FrameProbe<'a> {
    level: &'a dyn Fn(BlockPos) -> BlockState,
    along: Facing,
}

impl FrameProbe<'_> {
    fn frame_at(&self, pos: BlockPos) -> bool {
        is_frame((self.level)(pos))
    }

    /// Drops `start` onto the lowest open block above the frame, then
    /// slides it to the far end of the ledge opposite `along`.
    fn inner_corner(&self, start: BlockPos, min_y: i32) -> Option<BlockPos> {
        let lowest = min_y.max(start.y - MAX_HEIGHT);
        let mut y = start.y;
        while y > lowest && is_empty((self.level)(BlockPos::new(start.x, y - 1, start.z))) {
            y -= 1;
        }
        let dropped = BlockPos::new(start.x, y, start.z);
        let back = self.along.opposite();
        let reach = self.ledge_span(dropped, back);
        (reach > 0).then(|| step(dropped, back, reach - 1))
    }

    /// Open blocks walking `toward` over a frame ledge until a frame side
    /// closes the run; 0 when the ledge or the closing side is missing.
    fn ledge_span(&self, from: BlockPos, toward: Facing) -> i32 {
        let (dx, _, dz) = toward.step();
        let mut cell = from;
        for span in 0..=MAX_WIDTH {
            let state = (self.level)(cell);
            if !is_empty(state) {
                return if is_frame(state) { span } else { 0 };
            }
            if !self.frame_at(BlockPos::new(cell.x, cell.y - 1, cell.z)) {
                return 0;
            }
            cell = BlockPos::new(cell.x + dx, cell.y, cell.z + dz);
        }
        0
    }

    /// How many rows above `corner` are walled on both sides and open
    /// inside, and how many portal blocks those rows hold.
    fn open_rows(&self, corner: BlockPos, width: i32) -> (i32, i32) {
        let mut filled = 0;
        for rows in 0..MAX_HEIGHT {
            let row_start = BlockPos::new(corner.x, corner.y + rows, corner.z);
            if !self.frame_at(step(row_start, self.along, -1))
                || !self.frame_at(step(row_start, self.along, width))
            {
                return (rows, filled);
            }
            for offset in 0..width {
                let state = (self.level)(step(row_start, self.along, offset));
                if state.block() == Block::NetherPortal {
                    filled += 1;
                } else if !is_empty(state) {
                    return (rows, filled);
                }
            }
        }
        (MAX_HEIGHT, filled)
    }

    fn lintel_closed(&self, corner: BlockPos, width: i32, rows: i32) -> bool {
        let lintel = BlockPos::new(corner.x, corner.y + rows, corner.z);
        (0..width).all(|offset| self.frame_at(step(lintel, self.along, offset)))
    }
}

/// Whether fire lit at `pos` (an air block) would light a portal: some
/// neighbour is frame and an empty frame surrounds the position. The axis
/// the player faces across is tried first.
pub fn lights_portal(
    level: &dyn Fn(BlockPos) -> BlockState,
    pos: BlockPos,
    min_y: i32,
    facing: Facing,
) -> bool {
    let touches_frame = [
        Facing::Down,
        Facing::Up,
        Facing::North,
        Facing::South,
        Facing::West,
        Facing::East,
    ]
    .into_iter()
    .any(|direction| is_frame(level(step(pos, direction, 1))));
    touches_frame
        && find_empty_shape(
            level,
            pos,
            min_y,
            Axis::of_facing(facing.counter_clockwise()),
        )
        .is_some()
}

/// Whether a nether portal block at `pos` still sits in a complete frame
/// after its neighbour in `direction` changed.
pub fn portal_survives(
    level: &dyn Fn(BlockPos) -> BlockState,
    pos: BlockPos,
    min_y: i32,
    state: BlockState,
    direction: Facing,
    neighbour: BlockState,
) -> bool {
    let Some(axis) = Axis::of(state) else {
        return true;
    };
    let across = direction.is_horizontal() && Axis::of_facing(direction) != axis;
    across
        || neighbour.block() == Block::NetherPortal
        || find_any_shape(level, pos, min_y, axis).is_complete()
}

/// The largest rectangle of blocks passing `test` around `center`, spanning
/// `axis` horizontally and y vertically, each at most 21 long.
pub fn largest_rectangle_around(
    center: BlockPos,
    axis: Axis,
    test: &dyn Fn(BlockPos) -> bool,
) -> PortalRectangle {
    let limit = 21;
    let (negative, positive) = (axis.positive().opposite(), axis.positive());
    let extent = |from: BlockPos, direction: Facing, limit: i32| -> i32 {
        let mut count = 0;
        while count < limit && test(step(from, direction, count + 1)) {
            count += 1;
        }
        count
    };
    let negative_delta = extent(center, negative, limit);
    let positive_delta = extent(center, positive, limit);
    let count = (negative_delta + 1 + positive_delta) as usize;
    let mut bounds = vec![(0, 0); count];
    bounds[negative_delta as usize] = (
        extent(center, Facing::Down, limit),
        extent(center, Facing::Up, limit),
    );
    let center_index = bounds[negative_delta as usize].0;
    for i in 1..=negative_delta {
        let last = bounds[(negative_delta - (i - 1)) as usize];
        let from = step(center, negative, i);
        bounds[(negative_delta - i) as usize] = (
            extent(from, Facing::Down, last.0),
            extent(from, Facing::Up, last.1),
        );
    }
    for i in 1..=positive_delta {
        let last = bounds[(negative_delta + i - 1) as usize];
        let from = step(center, positive, i);
        bounds[(negative_delta + i) as usize] = (
            extent(from, Facing::Down, last.0),
            extent(from, Facing::Up, last.1),
        );
    }
    let (mut min_axis, mut min_y, mut size_axis, mut size_y) = (0, 0, 0, 0);
    let mut columns = vec![0; count];
    let mut row = center_index;
    while row >= 0 {
        for (i, (low, high)) in bounds.iter().enumerate() {
            let (min, max) = (center_index - low, center_index + high);
            columns[i] = if row < min || row > max {
                0
            } else {
                max + 1 - row
            };
        }
        let ((start, end), height) = max_rectangle(&columns);
        let new_size_axis = 1 + end - start;
        if new_size_axis * height > size_axis * size_y {
            min_axis = start;
            min_y = row;
            size_axis = new_size_axis;
            size_y = height;
        }
        row -= 1;
    }
    let min = step(
        step(center, positive, min_axis - negative_delta),
        Facing::Up,
        min_y - center_index,
    );
    PortalRectangle {
        min,
        axis,
        width: size_axis,
        height: size_y,
    }
}

/// The widest-area rectangle under a histogram: its column range and height.
fn max_rectangle(columns: &[i32]) -> ((i32, i32), i32) {
    let (mut max_start, mut max_end, mut max_height) = (0i32, 0i32, 0i32);
    let mut stack: Vec<usize> = vec![0];
    let mut column = 1;
    while column <= columns.len() {
        let height = if column == columns.len() {
            0
        } else {
            columns[column]
        };
        while let Some(&top) = stack.last() {
            let stack_height = columns[top];
            if height >= stack_height {
                stack.push(column);
                break;
            }
            stack.pop();
            let start = stack.last().map_or(0, |top| top + 1) as i32;
            if stack_height * (column as i32 - start) > max_height * (max_end - max_start) {
                max_end = column as i32;
                max_start = start;
                max_height = stack_height;
            }
        }
        if stack.is_empty() {
            stack.push(column);
        }
        column += 1;
    }
    ((max_start, max_end - 1), max_height)
}

/// The rectangle of the portal that `pos` (a nether portal block) is part of.
pub fn portal_rectangle(
    level: &dyn Fn(BlockPos) -> BlockState,
    pos: BlockPos,
) -> Option<PortalRectangle> {
    let state = level(pos);
    let axis = Axis::of(state).filter(|_| state.block() == Block::NetherPortal)?;
    Some(largest_rectangle_around(pos, axis, &|p| level(p) == state))
}

/// Where a player at `position` stands within a portal, as fractions:
/// across the portal, up it, and its offset forward of the portal plane.
pub fn relative_position(rectangle: &PortalRectangle, position: [f64; 3]) -> [f64; 3] {
    relative_position_of(rectangle, position, [PLAYER_WIDTH, PLAYER_HEIGHT])
}

/// [`relative_position`] for a body `size` (width, height) wide and tall.
pub fn relative_position_of(
    rectangle: &PortalRectangle,
    position: [f64; 3],
    size: [f64; 2],
) -> [f64; 3] {
    let [body_width, body_height] = size;
    let axis = rectangle.axis;
    let width = rectangle.width as f64 - body_width;
    let height = rectangle.height as f64 - body_height;
    let min = rectangle.min;
    let along = |axis: Axis| match axis {
        Axis::X => position[0],
        Axis::Z => position[2],
    };
    let right = if width > 0.0 {
        let start = axis.coordinate(min) as f64 + body_width / 2.0;
        ((along(axis) - start) / width).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let up = if height > 0.0 {
        ((position[1] - min.y as f64) / height).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let forward_axis = axis.other();
    let forward = along(forward_axis) - (forward_axis.coordinate(min) as f64 + 0.5);
    [right, up, forward]
}

/// The player's position on arrival at `exit`, keeping its relative
/// position in the portal it left, and how far to turn it.
pub fn arrival(exit: &PortalRectangle, source_axis: Axis, offset: [f64; 3]) -> ([f64; 3], f32) {
    arrival_of(exit, source_axis, offset, [PLAYER_WIDTH, PLAYER_HEIGHT])
}

/// [`arrival`] for a body `size` (width, height) wide and tall.
pub fn arrival_of(
    exit: &PortalRectangle,
    source_axis: Axis,
    offset: [f64; 3],
    size: [f64; 2],
) -> ([f64; 3], f32) {
    let [body_width, body_height] = size;
    let rotation = if source_axis == exit.axis { 0.0 } else { 90.0 };
    let width = exit.width as f64;
    let height = exit.height as f64;
    let right = body_width / 2.0 + (width - body_width) * offset[0];
    let up = (height - body_height) * offset[1];
    let forward = 0.5 + offset[2];
    let min = exit.min;
    let position = match exit.axis {
        Axis::X => [
            min.x as f64 + right,
            min.y as f64 + up,
            min.z as f64 + forward,
        ],
        Axis::Z => [
            min.x as f64 + forward,
            min.y as f64 + up,
            min.z as f64 + right,
        ],
    };
    (position, rotation)
}

/// The nearest portal block to `origin`: by distance, then the lowest.
pub fn closest(
    candidates: impl IntoIterator<Item = BlockPos>,
    origin: BlockPos,
) -> Option<BlockPos> {
    candidates.into_iter().min_by(|a, b| {
        distance_sq(*a, origin)
            .total_cmp(&distance_sq(*b, origin))
            .then(a.y.cmp(&b.y))
    })
}

fn distance_sq(a: BlockPos, b: BlockPos) -> f64 {
    let (dx, dy, dz) = ((a.x - b.x) as f64, (a.y - b.y) as f64, (a.z - b.z) as f64);
    dx * dx + dy * dy + dz * dz
}

/// Which portal block, if any, a player standing at `position` is inside.
pub fn contact(
    position: [f64; 3],
    level: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<PortalContact> {
    let bounds = [
        position[0] - PLAYER_HALF_WIDTH,
        position[1],
        position[2] - PLAYER_HALF_WIDTH,
        position[0] + PLAYER_HALF_WIDTH,
        position[1] + PLAYER_HEIGHT,
        position[2] + PLAYER_HALF_WIDTH,
    ];
    contact_box(bounds, level)
}

/// [`contact`] for a body filling `bounds` (min x, y, z, max x, y, z).
pub fn contact_box(
    bounds: [f64; 6],
    level: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Option<PortalContact> {
    const DEFLATE: f64 = 1.0e-5;
    let min = [
        bounds[0] + DEFLATE,
        bounds[1] + DEFLATE,
        bounds[2] + DEFLATE,
    ];
    let max = [
        bounds[3] - DEFLATE,
        bounds[4] - DEFLATE,
        bounds[5] - DEFLATE,
    ];
    let range = |axis: usize| min[axis].floor() as i32..=(max[axis].ceil() as i32 - 1);
    for y in range(1) {
        for z in range(2) {
            for x in range(0) {
                let pos = BlockPos::new(x, y, z);
                let Some(state) = level(pos) else {
                    continue;
                };
                let kind = match state.block() {
                    Block::NetherPortal => PortalKind::Nether,
                    Block::EndPortal => {
                        let (low, high) = (y as f64 + 6.0 / 16.0, y as f64 + 12.0 / 16.0);
                        if max[1] <= low || min[1] >= high {
                            continue;
                        }
                        PortalKind::End
                    }
                    Block::EndGateway => PortalKind::Gateway,
                    _ => continue,
                };
                return Some(PortalContact { kind, pos });
            }
        }
    }
    None
}

/// The positions the game visits when looking for room for a portal: a
/// square spiral out from `center`, east first then south.
pub fn spiral(center: BlockPos, radius: i32) -> impl Iterator<Item = BlockPos> {
    let directions = [Facing::East, Facing::South, Facing::West, Facing::North];
    let legs = 4 * radius;
    let mut cursor = step(center, Facing::South, 1);
    let mut leg: i32 = -1;
    let mut leg_size = 0;
    let mut leg_index = 0;
    std::iter::from_fn(move || {
        cursor = step(cursor, directions[((leg + 4) % 4) as usize], 1);
        if leg_index >= leg_size {
            if leg >= legs {
                return None;
            }
            leg += 1;
            leg_index = 0;
            leg_size = leg / 2 + 1;
        }
        leg_index += 1;
        Some(cursor)
    })
}

/// What creating a portal needs from its dimension.
pub trait PortalLevel {
    /// `None` where blocks are not loaded.
    fn get(&self, pos: BlockPos) -> Option<BlockState>;
    fn set(&mut self, pos: BlockPos, state: BlockState);
    /// One above the highest block that blocks motion in the column.
    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32>;
    fn min_y(&self) -> i32;
    /// One past the highest y a portal may occupy.
    fn logical_max_y(&self) -> i32;
    fn inside_border(&self, pos: BlockPos) -> bool;
    /// The nearest block inside the world border, keeping y.
    fn clamp_to_border(&self, pos: BlockPos) -> BlockPos;
}

pub use tuff_core::world::contact::blocks_motion;

fn can_portal_replace(level: &dyn PortalLevel, pos: BlockPos) -> bool {
    level.get(pos).is_some_and(|state| {
        state.is_replaceable() && !state.is_liquid() && state.get("waterlogged") != Some("true")
    })
}

fn can_host_frame(
    level: &dyn PortalLevel,
    origin: BlockPos,
    direction: Facing,
    offset: i32,
) -> bool {
    let clockwise = direction.clockwise();
    for width in -1..3 {
        for height in -1..4 {
            let pos = step(
                step(step(origin, direction, width), clockwise, offset),
                Facing::Up,
                height,
            );
            if height < 0 && !level.get(pos).is_some_and(BlockState::is_solid) {
                return false;
            }
            if height >= 0 && !can_portal_replace(level, pos) {
                return false;
            }
        }
    }
    true
}

/// Builds a 2x3 portal near `origin` the way the game does: in the closest
/// column that has solid ground and room for the whole frame, else one with
/// room for the frame alone, else floating on an obsidian platform at
/// `origin` clamped into 70..(logical height - 10).
pub fn create_portal(
    level: &mut dyn PortalLevel,
    origin: BlockPos,
    axis: Axis,
) -> Option<PortalRectangle> {
    let direction = axis.positive();
    let min_y = level.min_y();
    let max_placeable_y = level.logical_max_y() - 1;
    let mut closest_full: Option<(f64, BlockPos)> = None;
    let mut closest_partial: Option<(f64, BlockPos)> = None;
    for column in spiral(origin, CREATE_RADIUS) {
        let Some(height) = level.motion_blocking_height(column.x, column.z) else {
            continue;
        };
        let height = height.min(max_placeable_y);
        if !level.inside_border(column) || !level.inside_border(step(column, direction, 1)) {
            continue;
        }
        let mut y = height;
        while y >= min_y {
            let at = BlockPos::new(column.x, y, column.z);
            if can_portal_replace(level, at) {
                let first_empty_y = y;
                while y > min_y
                    && can_portal_replace(level, BlockPos::new(column.x, y - 1, column.z))
                {
                    y -= 1;
                }
                let delta = first_empty_y - y;
                if y + 4 <= max_placeable_y && (delta <= 0 || delta >= 3) {
                    let candidate = BlockPos::new(column.x, y, column.z);
                    if can_host_frame(level, candidate, direction, 0) {
                        let distance = distance_sq(origin, candidate);
                        if can_host_frame(level, candidate, direction, -1)
                            && can_host_frame(level, candidate, direction, 1)
                            && closest_full.is_none_or(|(best, _)| best > distance)
                        {
                            closest_full = Some((distance, candidate));
                        }
                        if closest_full.is_none()
                            && closest_partial.is_none_or(|(best, _)| best > distance)
                        {
                            closest_partial = Some((distance, candidate));
                        }
                    }
                }
            }
            y -= 1;
        }
    }
    let clockwise = direction.clockwise();
    let base = match closest_full.or(closest_partial) {
        Some((_, pos)) => pos,
        None => {
            let min_start_y = (min_y + 1).max(70);
            let max_start_y = max_placeable_y - 9;
            if max_start_y < min_start_y {
                return None;
            }
            let floating = level.clamp_to_border(step(
                BlockPos::new(origin.x, origin.y.clamp(min_start_y, max_start_y), origin.z),
                direction,
                -1,
            ));
            for side in -1..2 {
                for width in 0..2 {
                    for height in -1..3 {
                        let state = if height < 0 {
                            Block::Obsidian.default_state()
                        } else {
                            AIR
                        };
                        let pos = step(
                            step(step(floating, direction, width), clockwise, side),
                            Facing::Up,
                            height,
                        );
                        level.set(pos, state);
                    }
                }
            }
            floating
        }
    };
    for width in -1..3 {
        for height in -1..4 {
            if width == -1 || width == 2 || height == -1 || height == 3 {
                let pos = step(step(base, direction, width), Facing::Up, height);
                level.set(pos, Block::Obsidian.default_state());
            }
        }
    }
    let portal = axis.portal_state();
    for width in 0..2 {
        for height in 0..3 {
            level.set(
                step(step(base, direction, width), Facing::Up, height),
                portal,
            );
        }
    }
    Some(PortalRectangle {
        min: base,
        axis,
        width: 2,
        height: 3,
    })
}

/// The blocks [`collision_free`] reads for a body `size` (width, height)
/// standing at `bottom`.
pub fn collision_free_bounds(bottom: [f64; 3], size: [f64; 2]) -> (BlockPos, BlockPos) {
    let [width, height] = size;
    let reach = width * 1.5 + 1.0;
    (
        BlockPos::containing([bottom[0] - reach, bottom[1] - height, bottom[2] - reach]),
        BlockPos::containing([
            bottom[0] + reach,
            bottom[1] + height * 2.0 + 2.0,
            bottom[2] + reach,
        ]),
    )
}

/// Moves a body `size` (width, height) arriving with its feet at `bottom`
/// to the nearest spot within a block above it where its box meets no
/// block, as the game does on leaving a portal; bodies wider or taller
/// than 4 and spots with no room stay where they are.
pub fn collision_free(
    bottom: [f64; 3],
    size: [f64; 2],
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> [f64; 3] {
    const EPSILON: f64 = 1.0e-6;
    let [width, height] = size;
    if width > 4.0 || height > 4.0 {
        return bottom;
    }
    let half = height / 2.0;
    let center = [bottom[0], bottom[1] + half, bottom[2]];
    let allowed = [
        center[0] - width / 2.0 - EPSILON,
        center[1] - EPSILON,
        center[2] - width / 2.0 - EPSILON,
        center[0] + width / 2.0 + EPSILON,
        center[1] + 1.0 + EPSILON,
        center[2] + width / 2.0 + EPSILON,
    ];
    let inflate = [width, height, width];
    let search = [
        allowed[0] - inflate[0],
        allowed[1] - inflate[1],
        allowed[2] - inflate[2],
        allowed[3] + inflate[0],
        allowed[4] + inflate[1],
        allowed[5] + inflate[2],
    ];
    let mut blocked: Vec<[f64; 6]> = Vec::new();
    for x in search[0].floor() as i32 - 1..=search[3].floor() as i32 {
        for y in search[1].floor() as i32 - 1..=search[4].floor() as i32 {
            for z in search[2].floor() as i32 - 1..=search[5].floor() as i32 {
                let Some(state) = block(BlockPos::new(x, y, z)) else {
                    return bottom;
                };
                for b in state.collision_boxes() {
                    let b = [
                        x as f64 + f64::from(b[0]),
                        y as f64 + f64::from(b[1]),
                        z as f64 + f64::from(b[2]),
                        x as f64 + f64::from(b[3]),
                        y as f64 + f64::from(b[4]),
                        z as f64 + f64::from(b[5]),
                    ];
                    let meets = (0..3).all(|a| b[a] < search[a + 3] && b[a + 3] > search[a]);
                    if meets {
                        blocked.push([
                            b[0] - width / 2.0,
                            b[1] - half,
                            b[2] - width / 2.0,
                            b[3] + width / 2.0,
                            b[4] + half,
                            b[5] + width / 2.0,
                        ]);
                    }
                }
            }
        }
    }
    let cuts = |axis: usize| {
        let mut cuts = vec![allowed[axis], allowed[axis + 3]];
        for b in &blocked {
            for edge in [b[axis], b[axis + 3]] {
                if edge > allowed[axis] && edge < allowed[axis + 3] {
                    cuts.push(edge);
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        cuts
    };
    let (xs, ys, zs) = (cuts(0), cuts(1), cuts(2));
    let mut best: Option<(f64, [f64; 3])> = None;
    for x in xs.windows(2) {
        for y in ys.windows(2) {
            for z in zs.windows(2) {
                let cell = [x[0], y[0], z[0], x[1], y[1], z[1]];
                let mid = [
                    (cell[0] + cell[3]) / 2.0,
                    (cell[1] + cell[4]) / 2.0,
                    (cell[2] + cell[5]) / 2.0,
                ];
                let inside = |b: &[f64; 6]| (0..3).all(|a| b[a] < mid[a] && mid[a] < b[a + 3]);
                if blocked.iter().any(inside) {
                    continue;
                }
                let point: [f64; 3] =
                    std::array::from_fn(|a| center[a].clamp(cell[a], cell[a + 3]));
                let distance: f64 = (0..3).map(|a| (point[a] - center[a]).powi(2)).sum();
                if best.is_none_or(|(d, _)| distance < d) {
                    best = Some((distance, point));
                }
            }
        }
    }
    best.map_or(bottom, |(_, point)| [point[0], point[1] - half, point[2]])
}

/// The chunk-covering square a search of `radius` around `origin` reads.
pub fn search_bounds(origin: BlockPos, radius: i32) -> (BlockPos, BlockPos) {
    (
        BlockPos::new(origin.x - radius, origin.y, origin.z - radius),
        BlockPos::new(origin.x + radius, origin.y, origin.z + radius),
    )
}

/// The blocks a portal built near `origin` may touch.
pub fn create_bounds(origin: BlockPos) -> (BlockPos, BlockPos) {
    let reach = CREATE_RADIUS + 4;
    (
        BlockPos::new(origin.x - reach, origin.y, origin.z - reach),
        BlockPos::new(origin.x + reach, origin.y, origin.z + reach),
    )
}

// The end.

fn frame_with_eye(state: BlockState, facing: Facing) -> bool {
    state.block() == Block::EndPortalFrame
        && state.get("eye") == Some("true")
        && state.get("facing")
            == Some(match facing {
                Facing::North => "north",
                Facing::South => "south",
                Facing::West => "west",
                Facing::East => "east",
                Facing::Up | Facing::Down => "",
            })
}

/// The centre of the 3x3 that a complete ring of eyed frames around
/// `pos` (one of the frames) encloses.
pub fn end_portal_center(
    level: &dyn Fn(BlockPos) -> BlockState,
    pos: BlockPos,
) -> Option<BlockPos> {
    let ring = |center: BlockPos| -> Vec<(BlockPos, Facing)> {
        let mut blocks = Vec::with_capacity(12);
        for i in -1..=1 {
            blocks.push((
                BlockPos::new(center.x + i, center.y, center.z - 2),
                Facing::South,
            ));
            blocks.push((
                BlockPos::new(center.x + i, center.y, center.z + 2),
                Facing::North,
            ));
            blocks.push((
                BlockPos::new(center.x - 2, center.y, center.z + i),
                Facing::East,
            ));
            blocks.push((
                BlockPos::new(center.x + 2, center.y, center.z + i),
                Facing::West,
            ));
        }
        blocks
    };
    let mut centers = HashSet::new();
    for (frame, _) in ring(pos) {
        let center = BlockPos::new(2 * pos.x - frame.x, pos.y, 2 * pos.z - frame.z);
        centers.insert(center);
    }
    let mut centers: Vec<BlockPos> = centers.into_iter().collect();
    centers.sort_by_key(|c| (c.x, c.z));
    centers.into_iter().find(|&center| {
        ring(center)
            .into_iter()
            .all(|(frame, facing)| frame_with_eye(level(frame), facing))
    })
}

/// The nine blocks a completed ring fills.
pub fn end_portal_blocks(center: BlockPos) -> impl Iterator<Item = BlockPos> {
    (-1..=1).flat_map(move |dx| {
        (-1..=1).map(move |dz| BlockPos::new(center.x + dx, center.y, center.z + dz))
    })
}

/// The obsidian platform and the air above it that players arrive on in
/// the end, centred on `origin`.
pub fn end_platform(origin: BlockPos) -> impl Iterator<Item = (BlockPos, BlockState)> {
    (-2..=2).flat_map(move |dz| {
        (-2..=2).flat_map(move |dx| {
            (-1..3).map(move |dy| {
                let block = if dy == -1 {
                    Block::Obsidian
                } else {
                    Block::Air
                };
                (
                    BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz),
                    block.default_state(),
                )
            })
        })
    })
}

/// Where an end portal takes a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndExit {
    /// Into the end, onto its obsidian platform.
    Platform,
    /// Out of the end for the first time: the credits roll, then the
    /// player respawns.
    Credits,
    /// Out of the end again: straight to the respawn point.
    RespawnPoint,
}

pub fn end_exit(in_end: bool, seen_credits: bool) -> EndExit {
    match (in_end, seen_credits) {
        (false, _) => EndExit::Platform,
        (true, false) => EndExit::Credits,
        (true, true) => EndExit::RespawnPoint,
    }
}

#[cfg(test)]
#[path = "portal_tests.rs"]
mod tests;
