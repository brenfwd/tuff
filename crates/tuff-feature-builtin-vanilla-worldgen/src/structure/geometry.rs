//! Boxes, rotations and mirrors as structures use them.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;

use tuff_nbt::Tag;
use tuff_world_model::BlockPos;

/// An inclusive block box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundingBox {
    pub min_x: i32,
    pub min_y: i32,
    pub min_z: i32,
    pub max_x: i32,
    pub max_y: i32,
    pub max_z: i32,
}

impl BoundingBox {
    /// A box from its corners; inverted bounds are swapped.
    pub fn new(min_x: i32, min_y: i32, min_z: i32, max_x: i32, max_y: i32, max_z: i32) -> Self {
        Self {
            min_x: min_x.min(max_x),
            min_y: min_y.min(max_y),
            min_z: min_z.min(max_z),
            max_x: min_x.max(max_x),
            max_y: min_y.max(max_y),
            max_z: min_z.max(max_z),
        }
    }

    pub fn at(pos: BlockPos) -> Self {
        Self::new(pos.x, pos.y, pos.z, pos.x, pos.y, pos.z)
    }

    pub fn from_corners(a: BlockPos, b: BlockPos) -> Self {
        Self::new(a.x, a.y, a.z, b.x, b.y, b.z)
    }

    /// The box of a piece `width` wide, `height` tall and `depth` deep whose
    /// foot is at `foot`, extending toward `direction`, offset by `off`.
    #[allow(clippy::too_many_arguments)]
    pub fn orient(
        foot: BlockPos,
        off: [i32; 3],
        width: i32,
        height: i32,
        depth: i32,
        direction: Facing,
    ) -> Self {
        let (x, y, z) = (foot.x, foot.y, foot.z);
        let [ox, oy, oz] = off;
        match direction {
            Facing::North => Self::new(
                x + ox,
                y + oy,
                z - depth + 1 + oz,
                x + width - 1 + ox,
                y + height - 1 + oy,
                z + oz,
            ),
            Facing::West => Self::new(
                x - depth + 1 + oz,
                y + oy,
                z + ox,
                x + oz,
                y + height - 1 + oy,
                z + width - 1 + ox,
            ),
            Facing::East => Self::new(
                x + oz,
                y + oy,
                z + ox,
                x + depth - 1 + oz,
                y + height - 1 + oy,
                z + width - 1 + ox,
            ),
            _ => Self::new(
                x + ox,
                y + oy,
                z + oz,
                x + width - 1 + ox,
                y + height - 1 + oy,
                z + depth - 1 + oz,
            ),
        }
    }

    /// The column box of a chunk between two heights.
    pub fn chunk(chunk_x: i32, chunk_z: i32, min_y: i32, max_y: i32) -> Self {
        Self::new(
            chunk_x << 4,
            min_y,
            chunk_z << 4,
            (chunk_x << 4) + 15,
            max_y,
            (chunk_z << 4) + 15,
        )
    }

    pub fn intersects(&self, o: &BoundingBox) -> bool {
        self.max_x >= o.min_x
            && self.min_x <= o.max_x
            && self.max_z >= o.min_z
            && self.min_z <= o.max_z
            && self.max_y >= o.min_y
            && self.min_y <= o.max_y
    }

    pub fn intersects_xz(&self, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> bool {
        self.max_x >= min_x && self.min_x <= max_x && self.max_z >= min_z && self.min_z <= max_z
    }

    pub fn contains(&self, pos: BlockPos) -> bool {
        self.contains_xyz(pos.x, pos.y, pos.z)
    }

    pub fn contains_xyz(&self, x: i32, y: i32, z: i32) -> bool {
        x >= self.min_x
            && x <= self.max_x
            && z >= self.min_z
            && z <= self.max_z
            && y >= self.min_y
            && y <= self.max_y
    }

    pub fn encapsulate(&mut self, o: &BoundingBox) {
        self.min_x = self.min_x.min(o.min_x);
        self.min_y = self.min_y.min(o.min_y);
        self.min_z = self.min_z.min(o.min_z);
        self.max_x = self.max_x.max(o.max_x);
        self.max_y = self.max_y.max(o.max_y);
        self.max_z = self.max_z.max(o.max_z);
    }

    pub fn encapsulating(a: &BoundingBox, b: &BoundingBox) -> Self {
        let mut out = *a;
        out.encapsulate(b);
        out
    }

    pub fn moved(&self, dx: i32, dy: i32, dz: i32) -> Self {
        Self {
            min_x: self.min_x + dx,
            min_y: self.min_y + dy,
            min_z: self.min_z + dz,
            max_x: self.max_x + dx,
            max_y: self.max_y + dy,
            max_z: self.max_z + dz,
        }
    }

    pub fn inflated(&self, by: i32) -> Self {
        Self::new(
            self.min_x - by,
            self.min_y - by,
            self.min_z - by,
            self.max_x + by,
            self.max_y + by,
            self.max_z + by,
        )
    }

    pub fn x_span(&self) -> i32 {
        self.max_x - self.min_x + 1
    }

    pub fn y_span(&self) -> i32 {
        self.max_y - self.min_y + 1
    }

    pub fn z_span(&self) -> i32 {
        self.max_z - self.min_z + 1
    }

    pub fn center(&self) -> BlockPos {
        BlockPos::new(
            self.min_x + (self.max_x - self.min_x + 1) / 2,
            self.min_y + (self.max_y - self.min_y + 1) / 2,
            self.min_z + (self.max_z - self.min_z + 1) / 2,
        )
    }

    pub fn from_ints(values: &[i32]) -> Option<Self> {
        let [min_x, min_y, min_z, max_x, max_y, max_z] = *values else {
            return None;
        };
        Some(Self::new(min_x, min_y, min_z, max_x, max_y, max_z))
    }

    pub fn to_nbt(&self) -> Tag {
        Tag::IntArray(vec![
            self.min_x, self.min_y, self.min_z, self.max_x, self.max_y, self.max_z,
        ])
    }

    /// Every position inside, x fastest, then y, then z.
    pub fn positions(&self) -> impl Iterator<Item = BlockPos> + use<> {
        let b = *self;
        (b.min_z..=b.max_z).flat_map(move |z| {
            (b.min_y..=b.max_y)
                .flat_map(move |y| (b.min_x..=b.max_x).map(move |x| BlockPos::new(x, y, z)))
        })
    }
}

/// The smallest box around every box, or `None` for none.
pub fn encapsulating_boxes<'a>(
    boxes: impl IntoIterator<Item = &'a BoundingBox>,
) -> Option<BoundingBox> {
    let mut iter = boxes.into_iter();
    let mut out = *iter.next()?;
    for b in iter {
        out.encapsulate(b);
    }
    Some(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rotation {
    None,
    Clockwise90,
    Clockwise180,
    CounterClockwise90,
}

impl Rotation {
    /// In the game's order, which random picks index into.
    pub const ALL: [Rotation; 4] = [
        Rotation::None,
        Rotation::Clockwise90,
        Rotation::Clockwise180,
        Rotation::CounterClockwise90,
    ];

    fn quarter_turns(self) -> usize {
        match self {
            Rotation::None => 0,
            Rotation::Clockwise90 => 1,
            Rotation::Clockwise180 => 2,
            Rotation::CounterClockwise90 => 3,
        }
    }

    pub fn rotate_state(self, state: BlockState) -> BlockState {
        let mut state = state;
        for _ in 0..self.quarter_turns() {
            state = state.rotated_clockwise();
        }
        state
    }

    pub fn rotate_facing(self, facing: Facing) -> Facing {
        let mut facing = facing;
        for _ in 0..self.quarter_turns() {
            facing = facing.clockwise();
        }
        facing
    }

    /// This rotation followed by `other`.
    pub fn then(self, other: Rotation) -> Rotation {
        Rotation::ALL[(self.quarter_turns() + other.quarter_turns()) % 4]
    }

    /// The name the game saves.
    pub fn name(self) -> &'static str {
        match self {
            Rotation::None => "NONE",
            Rotation::Clockwise90 => "CLOCKWISE_90",
            Rotation::Clockwise180 => "CLOCKWISE_180",
            Rotation::CounterClockwise90 => "COUNTERCLOCKWISE_90",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mirror {
    None,
    LeftRight,
    FrontBack,
}

impl Mirror {
    pub fn mirror_state(self, state: BlockState) -> BlockState {
        match self {
            Mirror::None => state,
            Mirror::LeftRight => state.mirrored_left_right(),
            Mirror::FrontBack => state.mirrored_front_back(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mirror::None => "NONE",
            Mirror::LeftRight => "LEFT_RIGHT",
            Mirror::FrontBack => "FRONT_BACK",
        }
    }
}

/// A template-relative position mirrored, then rotated about `pivot`.
pub fn transform(pos: BlockPos, mirror: Mirror, rotation: Rotation, pivot: BlockPos) -> BlockPos {
    let (mut x, y, mut z) = (pos.x, pos.y, pos.z);
    match mirror {
        Mirror::LeftRight => z = -z,
        Mirror::FrontBack => x = -x,
        Mirror::None => {}
    }
    let (px, pz) = (pivot.x, pivot.z);
    match rotation {
        Rotation::CounterClockwise90 => BlockPos::new(px - pz + z, y, px + pz - x),
        Rotation::Clockwise90 => BlockPos::new(px + pz - z, y, pz - px + x),
        Rotation::Clockwise180 => BlockPos::new(px + px - x, y, pz + pz - z),
        Rotation::None => BlockPos::new(x, y, z),
    }
}

/// The game's 2D data value of a horizontal direction (south 0, west 1,
/// north 2, east 3), as pieces save their orientation.
pub fn data_value_2d(facing: Facing) -> i32 {
    match facing {
        Facing::South => 0,
        Facing::West => 1,
        Facing::North => 2,
        Facing::East => 3,
        _ => -1,
    }
}

pub fn offset(pos: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}
