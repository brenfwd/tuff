//! Breaking and placing on the way: what a step needs cleared or filled,
//! and the hand actions a bot sends for it.

use crate::path::Pos;
use crate::world::{Blocks, Pending};

/// A block face, named by the direction it points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Face {
    pub fn normal(self) -> Pos {
        match self {
            Face::Down => (0, -1, 0),
            Face::Up => (0, 1, 0),
            Face::North => (0, 0, -1),
            Face::South => (0, 0, 1),
            Face::West => (-1, 0, 0),
            Face::East => (1, 0, 0),
        }
    }

    /// The horizontal face pointing along (dx, dz).
    pub fn toward(dx: i32, dz: i32) -> Option<Face> {
        match (dx.signum(), dz.signum()) {
            (1, 0) => Some(Face::East),
            (-1, 0) => Some(Face::West),
            (0, 1) => Some(Face::South),
            (0, -1) => Some(Face::North),
            _ => None,
        }
    }

    pub const ALL: [Face; 6] = [
        Face::Down,
        Face::North,
        Face::South,
        Face::West,
        Face::East,
        Face::Up,
    ];
}

/// A hand action the host performs for a bot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Selects a hotbar slot (0 to 8).
    Select(u8),
    /// Starts digging a block; the body keeps digging until it breaks.
    Dig(Pos),
    /// Uses the held item on a face of a block.
    Place { against: Pos, face: Face },
    /// Uses the held item on a side face at this height (0 to 1) of the
    /// clicked block, so half blocks land in the upper half.
    PlaceAt {
        against: Pos,
        face: Face,
        cursor_y: f32,
    },
    /// Turns the body before the next action.
    Look { yaw: f32, pitch: f32 },
}

/// What the bot carries for placing, read from its inventory each tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kit {
    /// Throwaway blocks in the hotbar.
    pub throwaway: u32,
    /// A hotbar slot (0 to 8) that holds one.
    pub throwaway_slot: Option<u8>,
}

/// What a step from `at` to `next` needs before the bot can walk it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Clear,
    Dig(Pos),
    Bridge,
    Pillar,
}

/// The cells a step passes through, top first, in the order they are dug.
fn cells(at: Pos, next: Pos) -> Vec<Pos> {
    let (x, y, z) = at;
    let (nx, ny, nz) = next;
    let horizontal = (nx - x).abs() + (nz - z).abs() == 1;
    match ny - y {
        0 if horizontal => vec![(nx, y + 1, nz), (nx, y, nz)],
        1 if horizontal => vec![(x, y + 2, z), (nx, y + 2, nz), (nx, y + 1, nz)],
        1 if (nx, nz) == (x, z) => vec![(x, y + 2, z)],
        d if d < 0 && horizontal => (ny..=y + 1).rev().map(|cy| (nx, cy, nz)).collect(),
        _ => Vec::new(),
    }
}

pub fn need(w: &mut dyn Blocks, at: Pos, next: Pos) -> Result<Need, Pending> {
    for (x, y, z) in cells(at, next) {
        if !w.class_at(x, y, z)?.passable() {
            return Ok(Need::Dig((x, y, z)));
        }
    }
    let (x, y, z) = at;
    let (nx, ny, nz) = next;
    let feet = w.class_at(x, y, z)?;
    let afloat = feet.water() || feet.climbable();
    if ny == y + 1 && (nx, nz) == (x, z) && !afloat {
        return Ok(Need::Pillar);
    }
    if ny == y && (nx - x).abs() + (nz - z).abs() == 1 && !afloat {
        let target = w.class_at(nx, ny, nz)?;
        let ground = w.class_at(nx, ny - 1, nz)?;
        if !(target.water() || target.climbable() || ground.standable()) {
            return Ok(Need::Bridge);
        }
    }
    Ok(Need::Clear)
}
