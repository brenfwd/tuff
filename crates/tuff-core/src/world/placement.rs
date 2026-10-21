//! The vocabulary block placement is described in: a [`Level`] lookup, the
//! direction helpers, a player's click ([`Context`]) and what remains of a
//! removed block. Which state a block places as, whether it survives and
//! how it reacts to its neighbours are the feature's rules (see
//! [`crate::world::block_rules`]).

use tuff_vanilla_data::block::{Facing, Type};
use tuff_vanilla_data::{Block, BlockState, Item};

use crate::world::block_entity::ContainerKind;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::{BlockPos, Face};

/// Looks up the block at a position; unknown blocks read as air.
pub type Level<'a> = &'a dyn Fn(BlockPos) -> BlockState;

/// The order a changed block updates its neighbours in.
pub const UPDATE_ORDER: [Facing; 6] = [
    Facing::West,
    Facing::East,
    Facing::North,
    Facing::South,
    Facing::Down,
    Facing::Up,
];

pub fn facing(face: Face) -> Facing {
    match face {
        Face::Down => Facing::Down,
        Face::Up => Facing::Up,
        Face::North => Facing::North,
        Face::South => Facing::South,
        Face::West => Facing::West,
        Face::East => Facing::East,
    }
}

pub fn step(pos: BlockPos, direction: Facing) -> BlockPos {
    let (x, y, z) = direction.step();
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

/// A player using a block item on a block.
#[derive(Debug, Clone)]
pub struct Context {
    /// The block that was clicked.
    pub clicked: BlockPos,
    pub face: Facing,
    /// Where on the clicked block the cursor was, each axis in 0..=1.
    pub cursor: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub sneaking: bool,
    pub item: Item,
    /// The position to fill: the clicked block if it can be replaced,
    /// otherwise its neighbour on the clicked face.
    pub pos: BlockPos,
    pub replace_clicked: bool,
    /// The vertical extent of the world being placed in.
    pub shape: ChunkShape,
    /// The item finds nowhere to go (scaffolding extending past its reach).
    pub blocked: bool,
}

impl Context {
    /// A click on `clicked` filling that same position, before any rules
    /// decide where the block really goes.
    #[allow(clippy::too_many_arguments)]
    pub fn at_clicked(
        clicked: BlockPos,
        face: Facing,
        cursor: [f32; 3],
        yaw: f32,
        pitch: f32,
        sneaking: bool,
        item: Item,
        shape: ChunkShape,
    ) -> Self {
        Self {
            clicked,
            face,
            cursor,
            yaw,
            pitch,
            sneaking,
            item,
            pos: clicked,
            replace_clicked: true,
            shape,
            blocked: false,
        }
    }

    /// The click location relative to the position being filled.
    pub fn click(&self) -> [f64; 3] {
        [
            (self.clicked.x - self.pos.x) as f64 + self.cursor[0] as f64,
            (self.clicked.y - self.pos.y) as f64 + self.cursor[1] as f64,
            (self.clicked.z - self.pos.z) as f64 + self.cursor[2] as f64,
        ]
    }

    /// Whether a half-height block should go in the upper half.
    pub fn upper_half(&self) -> bool {
        self.face == Facing::Down || (self.face != Facing::Up && self.click()[1] > 0.5)
    }

    /// The horizontal direction the player faces.
    pub fn horizontal(&self) -> Facing {
        from_yaw(self.yaw)
    }

    /// Directions ordered by how directly the player looks along them.
    pub fn nearest(&self) -> [Facing; 6] {
        let pitch = self.pitch.to_radians();
        let yaw = -self.yaw.to_radians();
        let (pitch_sin, pitch_cos) = pitch.sin_cos();
        let (yaw_sin, yaw_cos) = yaw.sin_cos();
        let x = if yaw_sin > 0.0 {
            Facing::East
        } else {
            Facing::West
        };
        let y = if pitch_sin < 0.0 {
            Facing::Up
        } else {
            Facing::Down
        };
        let z = if yaw_cos > 0.0 {
            Facing::South
        } else {
            Facing::North
        };
        let (dx, dy, dz) = (yaw_sin.abs(), pitch_sin.abs(), yaw_cos.abs());
        let (hx, hz) = (dx * pitch_cos, dz * pitch_cos);
        let [a, b, c] = if dx > dz {
            if dy > hx {
                [y, x, z]
            } else if hz > dy {
                [x, z, y]
            } else {
                [x, y, z]
            }
        } else if dy > hz {
            [y, z, x]
        } else if hx > dy {
            [z, x, y]
        } else {
            [z, y, x]
        };
        [a, b, c, c.opposite(), b.opposite(), a.opposite()]
    }

    /// [`Self::nearest`], but when placing against a face, attaching to that
    /// face comes first.
    pub fn nearest_for_placement(&self) -> [Facing; 6] {
        let mut directions = self.nearest();
        if !self.replace_clicked {
            let against = self.face.opposite();
            if let Some(index) = directions.iter().position(|d| *d == against) {
                directions[..=index].rotate_right(1);
            }
        }
        directions
    }
}

pub fn from_yaw(yaw: f32) -> Facing {
    match ((yaw as f64 / 90.0 + 0.5).floor() as i64) & 3 {
        0 => Facing::South,
        1 => Facing::West,
        2 => Facing::North,
        _ => Facing::East,
    }
}

/// What remains when a block is removed: the liquid it held, if any.
pub fn remains(state: BlockState) -> BlockState {
    state.fluid_state().block()
}

/// Whether `state` placed at `pos` would collide with the box
/// `[min_x, min_y, min_z, max_x, max_y, max_z]`.
pub fn obstructs(state: BlockState, pos: BlockPos, other: [f64; 6]) -> bool {
    crate::world::physics::block_boxes(Some(state), pos)
        .any(|b| (0..3).all(|axis| b[axis] < other[axis + 3] && other[axis] < b[axis + 3]))
}

/// Chests that pair into double chests.
pub fn is_chest(block: Block) -> bool {
    ContainerKind::of_block(block).is_some_and(ContainerKind::pairs)
}

/// The side a double chest's other half lies on.
pub fn chest_connected_direction(state: BlockState) -> Option<Facing> {
    let facing = state.facing()?;
    Some(match state.r#type()? {
        Type::Left => facing.clockwise(),
        Type::Right => facing.counter_clockwise(),
        _ => return None,
    })
}

/// The position of the other half of a double chest at `pos`.
pub fn chest_partner(state: BlockState, pos: BlockPos) -> Option<BlockPos> {
    chest_connected_direction(state).map(|direction| step(pos, direction))
}
