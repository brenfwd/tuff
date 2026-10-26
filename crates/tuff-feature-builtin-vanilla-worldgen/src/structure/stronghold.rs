//! Strongholds: stone brick corridors and rooms grown from a spiral
//! staircase, each child picked by weight until a portal room exists, then
//! moved below sea level. The start chunks come from the concentric rings
//! placement of the structure set.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::json::{Result, obj};
use crate::structure::geometry::BoundingBox;
use crate::structure::piece::{Frame, Framed, structure_loot};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use tuff_item::random::RandomSource;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

const MAX_DEPTH: i32 = 50;
const START_Y: i32 = 64;
const MAX_REACH: i32 = 112;

struct Stronghold;

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "stronghold")?;
    Ok(Arc::new(Stronghold))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    Opening,
    Wood,
    Grates,
    Iron,
}

impl Door {
    fn random(random: &mut dyn RandomSource) -> Self {
        match random.next_int_bounded(5) {
            2 => Door::Wood,
            3 => Door::Grates,
            4 => Door::Iron,
            _ => Door::Opening,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Door::Opening => "OPENING",
            Door::Wood => "WOOD_DOOR",
            Door::Grates => "GRATES",
            Door::Iron => "IRON_DOOR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Straight,
    PrisonHall,
    LeftTurn,
    RightTurn,
    RoomCrossing,
    StraightStairsDown,
    StairsDown,
    FiveCrossing,
    ChestCorridor,
    Library,
    PortalRoom,
}

enum Kind {
    Start,
    StairsDown,
    Straight {
        left: bool,
        right: bool,
    },
    ChestCorridor {
        placed_chest: AtomicBool,
    },
    StraightStairsDown,
    LeftTurn,
    RightTurn,
    RoomCrossing {
        kind: i32,
    },
    PrisonHall,
    Library {
        tall: bool,
    },
    FiveCrossing {
        left_low: bool,
        left_high: bool,
        right_low: bool,
        right_high: bool,
    },
    PortalRoom {
        placed_spawner: AtomicBool,
    },
    Filler {
        steps: i32,
    },
}

struct StrongholdPiece {
    frame: Frame,
    door: Door,
    kind: Kind,
}

const SHAPES: [Shape; 11] = [
    Shape::Straight,
    Shape::PrisonHall,
    Shape::LeftTurn,
    Shape::RightTurn,
    Shape::RoomCrossing,
    Shape::StraightStairsDown,
    Shape::StairsDown,
    Shape::FiveCrossing,
    Shape::ChestCorridor,
    Shape::Library,
    Shape::PortalRoom,
];

/// Selection weight and placement cap (0 = unlimited), indexed like `SHAPES`.
const ODDS: [(i32, u8); 11] = [
    (40, 0),
    (5, 5),
    (20, 0),
    (20, 0),
    (10, 6),
    (5, 5),
    (5, 5),
    (5, 4),
    (5, 4),
    (10, 2),
    (20, 1),
];

const ALL_OPEN: u16 = (1 << SHAPES.len()) - 1;

const CAPPED: u16 = {
    let mut mask = 0;
    let mut i = 0;
    while i < ODDS.len() {
        if ODDS[i].1 > 0 {
            mask |= 1 << i;
        }
        i += 1;
    }
    mask
};

/// Which shapes may still be drawn and how often each was used.
struct Deck {
    open: u16,
    used: [u8; 11],
}

impl Deck {
    const fn fresh() -> Self {
        Self {
            open: ALL_OPEN,
            used: [0; 11],
        }
    }

    fn slots_from(&self, first: usize) -> impl Iterator<Item = usize> + use<> {
        let mut rest = self.open & !((1u16 << first) - 1);
        std::iter::from_fn(move || {
            (rest != 0).then(|| {
                let slot = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                slot
            })
        })
    }

    fn total(&self) -> i32 {
        self.slots_from(0).map(|slot| ODDS[slot].0).sum()
    }

    /// The open slot a draw of `ticket` lands on.
    fn landing(&self, mut ticket: i32) -> Option<usize> {
        self.slots_from(0).find(|&slot| {
            ticket -= ODDS[slot].0;
            ticket < 0
        })
    }

    fn record(&mut self, slot: usize) {
        self.used[slot] += 1;
        if self.used[slot] == ODDS[slot].1 {
            self.open &= !(1 << slot);
        }
    }
}

fn deep_enough(shape: Shape, depth: i32) -> bool {
    match shape {
        Shape::Library => depth > 4,
        Shape::PortalRoom => depth > 5,
        _ => true,
    }
}

/// The stronghold while it grows.
struct Builder {
    pieces: Vec<StrongholdPiece>,
    pending: Vec<usize>,
    deck: Deck,
    /// The shape of the last piece picked by weight.
    previous: Option<Shape>,
    imposed: Option<Shape>,
    has_portal_room: bool,
}

impl Builder {
    fn collision(&self, b: &BoundingBox) -> Option<&BoundingBox> {
        self.pieces
            .iter()
            .map(|p| &p.frame.bounding_box)
            .find(|p| p.intersects(b))
    }

    fn fits(&self, b: &BoundingBox) -> bool {
        b.min_y > 10 && self.collision(b).is_none()
    }

    fn bounding_box(&self) -> BoundingBox {
        let mut b = self.pieces[0].frame.bounding_box;
        for p in &self.pieces[1..] {
            b.encapsulate(&p.frame.bounding_box);
        }
        b
    }
}

fn piece(depth: i32, b: BoundingBox, direction: Facing, door: Door, kind: Kind) -> StrongholdPiece {
    StrongholdPiece {
        frame: Frame::new(depth, b, Some(direction)),
        door,
        kind,
    }
}

fn create(
    builder: &Builder,
    shape: Shape,
    random: &mut dyn RandomSource,
    foot: BlockPos,
    direction: Facing,
    depth: i32,
) -> Option<StrongholdPiece> {
    let orient = |off: [i32; 3], w, h, d| BoundingBox::orient(foot, off, w, h, d, direction);
    let b = match shape {
        Shape::Straight | Shape::ChestCorridor => orient([-1, -1, 0], 5, 5, 7),
        Shape::PrisonHall => orient([-1, -1, 0], 9, 5, 11),
        Shape::LeftTurn | Shape::RightTurn => orient([-1, -1, 0], 5, 5, 5),
        Shape::RoomCrossing => orient([-4, -1, 0], 11, 7, 11),
        Shape::StraightStairsDown => orient([-1, -7, 0], 5, 11, 8),
        Shape::StairsDown => orient([-1, -7, 0], 5, 11, 5),
        Shape::FiveCrossing => orient([-4, -3, 0], 10, 9, 11),
        Shape::PortalRoom => orient([-4, -1, 0], 11, 8, 16),
        Shape::Library => {
            let tall = orient([-4, -1, 0], 14, 11, 15);
            if builder.fits(&tall) {
                tall
            } else {
                orient([-4, -1, 0], 14, 6, 15)
            }
        }
    };
    if !builder.fits(&b) {
        return None;
    }
    if shape == Shape::PortalRoom {
        let kind = Kind::PortalRoom {
            placed_spawner: AtomicBool::new(false),
        };
        return Some(piece(depth, b, direction, Door::Opening, kind));
    }
    let door = Door::random(random);
    let kind = match shape {
        Shape::Straight => {
            let left = random.next_int_bounded(2) == 0;
            let right = random.next_int_bounded(2) == 0;
            Kind::Straight { left, right }
        }
        Shape::PrisonHall => Kind::PrisonHall,
        Shape::LeftTurn => Kind::LeftTurn,
        Shape::RightTurn => Kind::RightTurn,
        Shape::RoomCrossing => Kind::RoomCrossing {
            kind: random.next_int_bounded(5),
        },
        Shape::StraightStairsDown => Kind::StraightStairsDown,
        Shape::StairsDown => Kind::StairsDown,
        Shape::FiveCrossing => {
            let left_low = random.next_bool();
            let left_high = random.next_bool();
            let right_low = random.next_bool();
            let right_high = random.next_int_bounded(3) > 0;
            Kind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            }
        }
        Shape::ChestCorridor => Kind::ChestCorridor {
            placed_chest: AtomicBool::new(false),
        },
        Shape::Library => Kind::Library {
            tall: b.y_span() > 6,
        },
        Shape::PortalRoom => unreachable!(),
    };
    Some(piece(depth, b, direction, door, kind))
}

/// A short corridor filling the gap up to a piece at the same height, if
/// there is one within three blocks.
fn filler_box(builder: &Builder, foot: BlockPos, direction: Facing) -> Option<BoundingBox> {
    let b = BoundingBox::orient(foot, [-1, -1, 0], 5, 5, 4, direction);
    let collision = builder.collision(&b)?;
    if collision.min_y != b.min_y {
        return None;
    }
    (1..=2).rev().find_map(|depth| {
        let shorter = BoundingBox::orient(foot, [-1, -1, 0], 5, 5, depth, direction);
        (!collision.intersects(&shorter))
            .then(|| BoundingBox::orient(foot, [-1, -1, 0], 5, 5, depth + 1, direction))
    })
}

fn pick_piece(
    builder: &mut Builder,
    random: &mut dyn RandomSource,
    foot: BlockPos,
    direction: Facing,
    depth: i32,
) -> Option<StrongholdPiece> {
    if builder.deck.open & CAPPED == 0 {
        return None;
    }
    let total = builder.deck.total();
    if let Some(shape) = builder.imposed.take() {
        let made = create(builder, shape, random, foot, direction, depth);
        if made.is_some() {
            return made;
        }
    }
    let mut draws = 5;
    while draws > 0 {
        draws -= 1;
        let ticket = random.next_int_bounded(total);
        let Some(first) = builder.deck.landing(ticket) else {
            continue;
        };
        for slot in builder.deck.slots_from(first) {
            let shape = SHAPES[slot];
            if builder.previous == Some(shape) || !deep_enough(shape, depth) {
                break;
            }
            if let Some(made) = create(builder, shape, random, foot, direction, depth) {
                builder.deck.record(slot);
                builder.previous = Some(shape);
                return Some(made);
            }
        }
    }
    let b = filler_box(builder, foot, direction)?;
    (b.min_y > 1).then(|| {
        let steps = match direction {
            Facing::North | Facing::South => b.z_span(),
            _ => b.x_span(),
        };
        piece(depth, b, direction, Door::Opening, Kind::Filler { steps })
    })
}

#[derive(Clone, Copy)]
enum Side {
    Forward,
    Left,
    Right,
}

/// Grows a child off piece `index`: forward (`a` across, `b` up), or to
/// the left or right (`a` up, `b` along).
fn child(
    builder: &mut Builder,
    index: usize,
    random: &mut dyn RandomSource,
    side: Side,
    a: i32,
    b: i32,
) {
    let piece = &builder.pieces[index];
    let Some(orientation) = piece.frame.orientation else {
        return;
    };
    let bb = piece.frame.bounding_box;
    let depth = piece.frame.gen_depth;
    let (foot, direction) = match side {
        Side::Forward => {
            let foot = match orientation {
                Facing::North => BlockPos::new(bb.min_x + a, bb.min_y + b, bb.min_z - 1),
                Facing::South => BlockPos::new(bb.min_x + a, bb.min_y + b, bb.max_z + 1),
                Facing::West => BlockPos::new(bb.min_x - 1, bb.min_y + b, bb.min_z + a),
                Facing::East => BlockPos::new(bb.max_x + 1, bb.min_y + b, bb.min_z + a),
                _ => return,
            };
            (foot, orientation)
        }
        Side::Left => match orientation {
            Facing::North | Facing::South => (
                BlockPos::new(bb.min_x - 1, bb.min_y + a, bb.min_z + b),
                Facing::West,
            ),
            Facing::West | Facing::East => (
                BlockPos::new(bb.min_x + b, bb.min_y + a, bb.min_z - 1),
                Facing::North,
            ),
            _ => return,
        },
        Side::Right => match orientation {
            Facing::North | Facing::South => (
                BlockPos::new(bb.max_x + 1, bb.min_y + a, bb.min_z + b),
                Facing::East,
            ),
            Facing::West | Facing::East => (
                BlockPos::new(bb.min_x + b, bb.min_y + a, bb.max_z + 1),
                Facing::South,
            ),
            _ => return,
        },
    };
    let start = builder.pieces[0].frame.bounding_box;
    if depth > MAX_DEPTH
        || (foot.x - start.min_x).abs() > MAX_REACH
        || (foot.z - start.min_z).abs() > MAX_REACH
    {
        return;
    }
    if let Some(piece) = pick_piece(builder, random, foot, direction, depth + 1) {
        builder.pieces.push(piece);
        builder.pending.push(builder.pieces.len() - 1);
    }
}

fn add_children(builder: &mut Builder, index: usize, random: &mut dyn RandomSource) {
    use Side::*;
    let orientation = builder.pieces[index].frame.orientation;
    let north_or_east = matches!(orientation, Some(Facing::North | Facing::East));
    match builder.pieces[index].kind {
        Kind::Start => {
            builder.imposed = Some(Shape::FiveCrossing);
            child(builder, index, random, Forward, 1, 1);
        }
        Kind::StairsDown
        | Kind::ChestCorridor { .. }
        | Kind::StraightStairsDown
        | Kind::PrisonHall => child(builder, index, random, Forward, 1, 1),
        Kind::Straight { left, right } => {
            child(builder, index, random, Forward, 1, 1);
            if left {
                child(builder, index, random, Left, 1, 2);
            }
            if right {
                child(builder, index, random, Right, 1, 2);
            }
        }
        Kind::LeftTurn => {
            let side = if north_or_east { Left } else { Right };
            child(builder, index, random, side, 1, 1);
        }
        Kind::RightTurn => {
            let side = if north_or_east { Right } else { Left };
            child(builder, index, random, side, 1, 1);
        }
        Kind::RoomCrossing { .. } => {
            child(builder, index, random, Forward, 4, 1);
            child(builder, index, random, Left, 1, 4);
            child(builder, index, random, Right, 1, 4);
        }
        Kind::FiveCrossing {
            left_low,
            left_high,
            right_low,
            right_high,
        } => {
            let (a, b) = match orientation {
                Some(Facing::West | Facing::North) => (5, 3),
                _ => (3, 5),
            };
            child(builder, index, random, Forward, 5, 1);
            if left_low {
                child(builder, index, random, Left, a, 1);
            }
            if left_high {
                child(builder, index, random, Left, b, 7);
            }
            if right_low {
                child(builder, index, random, Right, a, 1);
            }
            if right_high {
                child(builder, index, random, Right, b, 7);
            }
        }
        Kind::PortalRoom { .. } => builder.has_portal_room = true,
        Kind::Library { .. } | Kind::Filler { .. } => {}
    }
}

fn grow(random: &mut dyn RandomSource, chunk: ChunkPos) -> Builder {
    let direction = Facing::HORIZONTAL[random.next_int_bounded(4) as usize];
    let (west, north) = ((chunk.x << 4) + 2, (chunk.z << 4) + 2);
    let start = BoundingBox::new(west, START_Y, north, west + 4, START_Y + 10, north + 4);
    let mut builder = Builder {
        pieces: vec![piece(0, start, direction, Door::Opening, Kind::Start)],
        pending: Vec::new(),
        deck: Deck::fresh(),
        previous: None,
        imposed: None,
        has_portal_room: false,
    };
    add_children(&mut builder, 0, random);
    while !builder.pending.is_empty() {
        let at = random.next_int_bounded(builder.pending.len() as i32) as usize;
        let index = builder.pending.remove(at);
        add_children(&mut builder, index, random);
    }
    builder
}

impl StructureKind for Stronghold {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let chunk = ctx.chunk;
        Some(GenerationStub {
            position: BlockPos::new(chunk.x << 4, 0, chunk.z << 4),
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let sea_level = ctx.sea_level();
                let min_y = ctx.generator.settings.min_y;
                let mut tries: i64 = 0;
                loop {
                    let random = &mut ctx.random;
                    random.set_large_feature_seed(ctx.seed.wrapping_add(tries), chunk.x, chunk.z);
                    tries += 1;
                    let builder = grow(random, chunk);
                    let b = builder.bounding_box();
                    let max_y = sea_level - 10;
                    let mut y = b.y_span() + min_y + 1;
                    if y < max_y {
                        y += random.next_int_bounded(max_y - y);
                    }
                    let dy = y - b.max_y;
                    if builder.has_portal_room {
                        return builder
                            .pieces
                            .into_iter()
                            .map(|mut p| {
                                p.frame.bounding_box = p.frame.bounding_box.moved(0, dy, 0);
                                Box::new(p) as Box<dyn Piece>
                            })
                            .collect();
                    }
                }
            }),
        })
    }
}

fn state(block: Block, properties: &[(&str, &str)]) -> BlockState {
    properties
        .iter()
        .fold(block.default_state(), |s, (k, v)| s.with(k, v).unwrap_or(s))
}

fn bricks() -> BlockState {
    Block::StoneBricks.default_state()
}

fn cave_air() -> BlockState {
    Block::CaveAir.default_state()
}

fn smooth_slab() -> BlockState {
    Block::SmoothStoneSlab.default_state()
}

fn wall_torch(facing: &str) -> BlockState {
    state(Block::WallTorch, &[("facing", facing)])
}

fn bars(sides: &str) -> BlockState {
    let names: Vec<(&str, &str)> = sides
        .chars()
        .map(|c| match c {
            'n' => ("north", "true"),
            'e' => ("east", "true"),
            's' => ("south", "true"),
            _ => ("west", "true"),
        })
        .collect();
    state(Block::IronBars, &names)
}

fn fence(sides: &str) -> BlockState {
    let names: Vec<(&str, &str)> = sides
        .chars()
        .map(|c| match c {
            'n' => ("north", "true"),
            'e' => ("east", "true"),
            's' => ("south", "true"),
            _ => ("west", "true"),
        })
        .collect();
    state(Block::OakFence, &names)
}

type Corner = (i32, i32, i32);

impl Framed for StrongholdPiece {
    fn frame(&self) -> &Frame {
        &self.frame
    }
}

struct Place<'a, 'g, 'r> {
    level: &'a mut Region<'g>,
    random: &'r mut dyn RandomSource,
    chunk: BoundingBox,
}

impl StrongholdPiece {
    fn set(&self, p: &mut Place, state: BlockState, x: i32, y: i32, z: i32) {
        self.place_block(p.level, state, x, y, z, &p.chunk);
    }

    fn fill(&self, p: &mut Place, from: Corner, to: Corner, state: BlockState) {
        self.generate_box(p.level, &p.chunk, from, to, state, state, false);
    }

    /// A box of random stone bricks on its edges and cave air inside.
    fn stone_box(&self, p: &mut Place, (x0, y0, z0): Corner, (x1, y1, z1): Corner, skip_air: bool) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if skip_air && is_air(self.get_block(p.level, x, y, z, &p.chunk)) {
                        continue;
                    }
                    let edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    let state = if edge {
                        let selection = p.random.next_float();
                        if selection < 0.2 {
                            Block::CrackedStoneBricks.default_state()
                        } else if selection < 0.5 {
                            Block::MossyStoneBricks.default_state()
                        } else if selection < 0.55 {
                            Block::InfestedStoneBricks.default_state()
                        } else {
                            bricks()
                        }
                    } else {
                        cave_air()
                    };
                    self.set(p, state, x, y, z);
                }
            }
        }
    }

    fn small_door(&self, p: &mut Place, door: Door, x: i32, y: i32, z: i32) {
        match door {
            Door::Opening => self.fill(p, (x, y, z), (x + 2, y + 2, z), cave_air()),
            Door::Wood | Door::Iron => {
                for (dx, dy) in [(0, 0), (0, 1), (0, 2), (1, 2), (2, 2), (2, 1), (2, 0)] {
                    self.set(p, bricks(), x + dx, y + dy, z);
                }
                let block = if door == Door::Wood {
                    Block::OakDoor
                } else {
                    Block::IronDoor
                };
                self.set(p, block.default_state(), x + 1, y, z);
                self.set(p, state(block, &[("half", "upper")]), x + 1, y + 1, z);
                if door == Door::Iron {
                    let button = |facing| state(Block::StoneButton, &[("facing", facing)]);
                    self.set(p, button("north"), x + 2, y + 1, z + 1);
                    self.set(p, button("south"), x + 2, y + 1, z - 1);
                }
            }
            Door::Grates => {
                self.set(p, cave_air(), x + 1, y, z);
                self.set(p, cave_air(), x + 1, y + 1, z);
                self.set(p, bars("w"), x, y, z);
                self.set(p, bars("w"), x, y + 1, z);
                self.set(p, bars("ew"), x, y + 2, z);
                self.set(p, bars("ew"), x + 1, y + 2, z);
                self.set(p, bars("ew"), x + 2, y + 2, z);
                self.set(p, bars("e"), x + 2, y + 1, z);
                self.set(p, bars("e"), x + 2, y, z);
            }
        }
    }

    fn stairs_down(&self, p: &mut Place) {
        let (b, slab) = (bricks(), smooth_slab());
        self.stone_box(p, (0, 0, 0), (4, 10, 4), true);
        self.small_door(p, self.door, 1, 7, 0);
        self.small_door(p, Door::Opening, 1, 1, 4);
        for (state, x, y, z) in [
            (b, 2, 6, 1),
            (b, 1, 5, 1),
            (slab, 1, 6, 1),
            (b, 1, 5, 2),
            (b, 1, 4, 3),
            (slab, 1, 5, 3),
            (b, 2, 4, 3),
            (b, 3, 3, 3),
            (slab, 3, 4, 3),
            (b, 3, 3, 2),
            (b, 3, 2, 1),
            (slab, 3, 3, 1),
            (b, 2, 2, 1),
            (b, 1, 1, 1),
            (slab, 1, 2, 1),
            (b, 1, 1, 2),
            (slab, 1, 1, 3),
        ] {
            self.set(p, state, x, y, z);
        }
    }

    fn straight(&self, p: &mut Place, left: bool, right: bool) {
        self.stone_box(p, (0, 0, 0), (4, 4, 6), true);
        self.small_door(p, self.door, 1, 1, 0);
        self.small_door(p, Door::Opening, 1, 1, 6);
        let (east, west) = (wall_torch("east"), wall_torch("west"));
        for (x, z, torch) in [(1, 1, east), (3, 1, west), (1, 5, east), (3, 5, west)] {
            self.maybe_generate_block(p.level, &p.chunk, p.random, 0.1, x, 2, z, torch);
        }
        if left {
            self.fill(p, (0, 1, 2), (0, 3, 4), cave_air());
        }
        if right {
            self.fill(p, (4, 1, 2), (4, 3, 4), cave_air());
        }
    }

    fn chest_corridor(&self, p: &mut Place, placed_chest: &AtomicBool) {
        let slab = Block::StoneBrickSlab.default_state();
        self.stone_box(p, (0, 0, 0), (4, 4, 6), true);
        self.small_door(p, self.door, 1, 1, 0);
        self.small_door(p, Door::Opening, 1, 1, 6);
        self.fill(p, (3, 1, 2), (3, 1, 4), bricks());
        self.set(p, slab, 3, 1, 1);
        self.set(p, slab, 3, 1, 5);
        self.set(p, slab, 3, 2, 2);
        self.set(p, slab, 3, 2, 4);
        for z in 2..=4 {
            self.set(p, slab, 2, 1, z);
        }
        if !placed_chest.load(Ordering::Relaxed) && p.chunk.contains(self.frame.world_pos(3, 2, 3))
        {
            placed_chest.store(true, Ordering::Relaxed);
            let table = structure_loot("stronghold_corridor");
            self.create_chest(p.level, &p.chunk, p.random, 3, 2, 3, table);
        }
    }

    fn straight_stairs_down(&self, p: &mut Place) {
        self.stone_box(p, (0, 0, 0), (4, 10, 7), true);
        self.small_door(p, self.door, 1, 7, 0);
        self.small_door(p, Door::Opening, 1, 1, 7);
        let stairs = state(Block::CobblestoneStairs, &[("facing", "south")]);
        for i in 0..6 {
            for x in 1..=3 {
                self.set(p, stairs, x, 6 - i, 1 + i);
            }
            if i < 5 {
                for x in 1..=3 {
                    self.set(p, bricks(), x, 5 - i, 1 + i);
                }
            }
        }
    }

    fn turn(&self, p: &mut Place, left: bool) {
        self.stone_box(p, (0, 0, 0), (4, 4, 4), true);
        self.small_door(p, self.door, 1, 1, 0);
        let north_or_east = matches!(self.frame.orientation, Some(Facing::North | Facing::East));
        let x = if north_or_east == left { 0 } else { 4 };
        self.fill(p, (x, 1, 1), (x, 3, 3), cave_air());
    }

    fn room_crossing(&self, p: &mut Place, kind: i32) {
        let b = bricks();
        self.stone_box(p, (0, 0, 0), (10, 6, 10), true);
        self.small_door(p, self.door, 4, 1, 0);
        self.fill(p, (4, 1, 10), (6, 3, 10), cave_air());
        self.fill(p, (0, 1, 4), (0, 3, 6), cave_air());
        self.fill(p, (10, 1, 4), (10, 3, 6), cave_air());
        match kind {
            0 => {
                let slab = smooth_slab();
                for (state, x, y, z) in [
                    (b, 5, 1, 5),
                    (b, 5, 2, 5),
                    (b, 5, 3, 5),
                    (wall_torch("west"), 4, 3, 5),
                    (wall_torch("east"), 6, 3, 5),
                    (wall_torch("south"), 5, 3, 4),
                    (wall_torch("north"), 5, 3, 6),
                    (slab, 4, 1, 4),
                    (slab, 4, 1, 5),
                    (slab, 4, 1, 6),
                    (slab, 6, 1, 4),
                    (slab, 6, 1, 5),
                    (slab, 6, 1, 6),
                    (slab, 5, 1, 4),
                    (slab, 5, 1, 6),
                ] {
                    self.set(p, state, x, y, z);
                }
            }
            1 => {
                for i in 0..5 {
                    self.set(p, b, 3, 1, 3 + i);
                    self.set(p, b, 7, 1, 3 + i);
                    self.set(p, b, 3 + i, 1, 3);
                    self.set(p, b, 3 + i, 1, 7);
                }
                self.set(p, b, 5, 1, 5);
                self.set(p, b, 5, 2, 5);
                self.set(p, b, 5, 3, 5);
                self.set(p, Block::Water.default_state(), 5, 4, 5);
            }
            2 => {
                let cobble = Block::Cobblestone.default_state();
                for z in 1..=9 {
                    self.set(p, cobble, 1, 3, z);
                    self.set(p, cobble, 9, 3, z);
                }
                for x in 1..=9 {
                    self.set(p, cobble, x, 3, 1);
                    self.set(p, cobble, x, 3, 9);
                }
                for (x, y, z) in [
                    (5, 1, 4),
                    (5, 1, 6),
                    (5, 3, 4),
                    (5, 3, 6),
                    (4, 1, 5),
                    (6, 1, 5),
                    (4, 3, 5),
                    (6, 3, 5),
                ] {
                    self.set(p, cobble, x, y, z);
                }
                for y in 1..=3 {
                    self.set(p, cobble, 4, y, 4);
                    self.set(p, cobble, 6, y, 4);
                    self.set(p, cobble, 4, y, 6);
                    self.set(p, cobble, 6, y, 6);
                }
                self.set(p, Block::WallTorch.default_state(), 5, 3, 5);
                let planks = Block::OakPlanks.default_state();
                for z in 2..=8 {
                    self.set(p, planks, 2, 3, z);
                    self.set(p, planks, 3, 3, z);
                    if z <= 3 || z >= 7 {
                        self.set(p, planks, 4, 3, z);
                        self.set(p, planks, 5, 3, z);
                        self.set(p, planks, 6, 3, z);
                    }
                    self.set(p, planks, 7, 3, z);
                    self.set(p, planks, 8, 3, z);
                }
                let ladder = state(Block::Ladder, &[("facing", "west")]);
                for y in 1..=3 {
                    self.set(p, ladder, 9, y, 3);
                }
                let table = structure_loot("stronghold_crossing");
                self.create_chest(p.level, &p.chunk, p.random, 3, 4, 8, table);
            }
            _ => {}
        }
    }

    fn prison_hall(&self, p: &mut Place) {
        self.stone_box(p, (0, 0, 0), (8, 4, 10), true);
        self.small_door(p, self.door, 1, 1, 0);
        self.fill(p, (1, 1, 10), (3, 3, 10), cave_air());
        for z in [1, 3, 7, 9] {
            self.stone_box(p, (4, 1, z), (4, 3, z), false);
        }
        for y in 1..=3 {
            self.set(p, bars("ns"), 4, y, 4);
            self.set(p, bars("nse"), 4, y, 5);
            self.set(p, bars("ns"), 4, y, 6);
            self.set(p, bars("we"), 5, y, 5);
            self.set(p, bars("we"), 6, y, 5);
            self.set(p, bars("we"), 7, y, 5);
        }
        self.set(p, bars("ns"), 4, 3, 2);
        self.set(p, bars("ns"), 4, 3, 8);
        let bottom = state(Block::IronDoor, &[("facing", "west")]);
        let top = state(Block::IronDoor, &[("facing", "west"), ("half", "upper")]);
        self.set(p, bottom, 4, 1, 2);
        self.set(p, top, 4, 2, 2);
        self.set(p, bottom, 4, 1, 8);
        self.set(p, top, 4, 2, 8);
    }

    fn library(&self, p: &mut Place, tall: bool) {
        let height = if tall { 11 } else { 6 };
        let (planks, shelf) = (
            Block::OakPlanks.default_state(),
            Block::Bookshelf.default_state(),
        );
        self.stone_box(p, (0, 0, 0), (13, height - 1, 14), true);
        self.small_door(p, self.door, 4, 1, 0);
        let web = Block::Cobweb.default_state();
        self.generate_maybe_box(
            p.level,
            &p.chunk,
            p.random,
            0.07,
            (2, 1, 1),
            (11, 4, 13),
            web,
            web,
            false,
            false,
        );
        for d in 1..=13 {
            if (d - 1) % 4 == 0 {
                self.fill(p, (1, 1, d), (1, 4, d), planks);
                self.fill(p, (12, 1, d), (12, 4, d), planks);
                self.set(p, wall_torch("east"), 2, 3, d);
                self.set(p, wall_torch("west"), 11, 3, d);
                if tall {
                    self.fill(p, (1, 6, d), (1, 9, d), planks);
                    self.fill(p, (12, 6, d), (12, 9, d), planks);
                }
            } else {
                self.fill(p, (1, 1, d), (1, 4, d), shelf);
                self.fill(p, (12, 1, d), (12, 4, d), shelf);
                if tall {
                    self.fill(p, (1, 6, d), (1, 9, d), shelf);
                    self.fill(p, (12, 6, d), (12, 9, d), shelf);
                }
            }
        }
        for d in (3..12).step_by(2) {
            self.fill(p, (3, 1, d), (4, 3, d), shelf);
            self.fill(p, (6, 1, d), (7, 3, d), shelf);
            self.fill(p, (9, 1, d), (10, 3, d), shelf);
        }
        if tall {
            self.fill(p, (1, 5, 1), (3, 5, 13), planks);
            self.fill(p, (10, 5, 1), (12, 5, 13), planks);
            self.fill(p, (4, 5, 1), (9, 5, 2), planks);
            self.fill(p, (4, 5, 12), (9, 5, 13), planks);
            self.set(p, planks, 9, 5, 11);
            self.set(p, planks, 8, 5, 11);
            self.set(p, planks, 9, 5, 10);
            let (we, ns) = (fence("we"), fence("ns"));
            self.fill(p, (3, 6, 3), (3, 6, 11), ns);
            self.fill(p, (10, 6, 3), (10, 6, 9), ns);
            self.fill(p, (4, 6, 2), (9, 6, 2), we);
            self.fill(p, (4, 6, 12), (7, 6, 12), we);
            self.set(p, fence("ne"), 3, 6, 2);
            self.set(p, fence("se"), 3, 6, 12);
            self.set(p, fence("nw"), 10, 6, 2);
            for i in 0..=2 {
                self.set(p, fence("sw"), 8 + i, 6, 12 - i);
                if i != 2 {
                    self.set(p, fence("ne"), 8 + i, 6, 11 - i);
                }
            }
            let ladder = state(Block::Ladder, &[("facing", "south")]);
            for y in 1..=7 {
                self.set(p, ladder, 10, y, 13);
            }
            let (e, w) = (fence("e"), fence("w"));
            self.set(p, e, 6, 9, 7);
            self.set(p, w, 7, 9, 7);
            self.set(p, e, 6, 8, 7);
            self.set(p, w, 7, 8, 7);
            self.set(p, fence("nswe"), 6, 7, 7);
            self.set(p, fence("nswe"), 7, 7, 7);
            self.set(p, e, 5, 7, 7);
            self.set(p, w, 8, 7, 7);
            self.set(p, fence("en"), 6, 7, 6);
            self.set(p, fence("es"), 6, 7, 8);
            self.set(p, fence("wn"), 7, 7, 6);
            self.set(p, fence("ws"), 7, 7, 8);
            let torch = Block::Torch.default_state();
            for (x, z) in [(5, 7), (8, 7), (6, 6), (6, 8), (7, 6), (7, 8)] {
                self.set(p, torch, x, 8, z);
            }
        }
        let table = structure_loot("stronghold_library");
        self.create_chest(p.level, &p.chunk, p.random, 3, 3, 5, table);
        if tall {
            self.set(p, cave_air(), 12, 9, 1);
            self.create_chest(p.level, &p.chunk, p.random, 12, 8, 1, table);
        }
    }

    fn five_crossing(
        &self,
        p: &mut Place,
        left_low: bool,
        left_high: bool,
        right_low: bool,
        right_high: bool,
    ) {
        let slab = smooth_slab();
        self.stone_box(p, (0, 0, 0), (9, 8, 10), true);
        self.small_door(p, self.door, 4, 3, 0);
        if left_low {
            self.fill(p, (0, 3, 1), (0, 5, 3), cave_air());
        }
        if right_low {
            self.fill(p, (9, 3, 1), (9, 5, 3), cave_air());
        }
        if left_high {
            self.fill(p, (0, 5, 7), (0, 7, 9), cave_air());
        }
        if right_high {
            self.fill(p, (9, 5, 7), (9, 7, 9), cave_air());
        }
        self.fill(p, (5, 1, 10), (7, 3, 10), cave_air());
        self.stone_box(p, (1, 2, 1), (8, 2, 6), false);
        self.stone_box(p, (4, 1, 5), (4, 4, 9), false);
        self.stone_box(p, (8, 1, 5), (8, 4, 9), false);
        self.stone_box(p, (1, 4, 7), (3, 4, 9), false);
        self.stone_box(p, (1, 3, 5), (3, 3, 6), false);
        self.fill(p, (1, 3, 4), (3, 3, 4), slab);
        self.fill(p, (1, 4, 6), (3, 4, 6), slab);
        self.stone_box(p, (5, 1, 7), (7, 1, 8), false);
        self.fill(p, (5, 1, 9), (7, 1, 9), slab);
        self.fill(p, (5, 2, 7), (7, 2, 7), slab);
        self.fill(p, (4, 5, 7), (4, 5, 9), slab);
        self.fill(p, (8, 5, 7), (8, 5, 9), slab);
        let double = state(Block::SmoothStoneSlab, &[("type", "double")]);
        self.fill(p, (5, 5, 7), (7, 5, 9), double);
        self.set(p, wall_torch("south"), 6, 5, 6);
    }

    fn portal_room(&self, p: &mut Place, placed_spawner: &AtomicBool) {
        let lava = Block::Lava.default_state();
        self.stone_box(p, (0, 0, 0), (10, 7, 15), false);
        self.small_door(p, Door::Grates, 4, 1, 0);
        self.stone_box(p, (1, 6, 1), (1, 6, 14), false);
        self.stone_box(p, (9, 6, 1), (9, 6, 14), false);
        self.stone_box(p, (2, 6, 1), (8, 6, 2), false);
        self.stone_box(p, (2, 6, 14), (8, 6, 14), false);
        self.stone_box(p, (1, 1, 1), (2, 1, 4), false);
        self.stone_box(p, (8, 1, 1), (9, 1, 4), false);
        self.fill(p, (1, 1, 1), (1, 1, 3), lava);
        self.fill(p, (9, 1, 1), (9, 1, 3), lava);
        self.stone_box(p, (3, 1, 8), (7, 1, 12), false);
        self.fill(p, (4, 1, 9), (6, 1, 11), lava);
        let (ns, we) = (bars("ns"), bars("we"));
        for z in (3..14).step_by(2) {
            self.fill(p, (0, 3, z), (0, 4, z), ns);
            self.fill(p, (10, 3, z), (10, 4, z), ns);
        }
        for x in (2..9).step_by(2) {
            self.fill(p, (x, 3, 15), (x, 4, 15), we);
        }
        let stairs = state(Block::StoneBrickStairs, &[("facing", "north")]);
        self.stone_box(p, (4, 1, 5), (6, 1, 7), false);
        self.stone_box(p, (4, 2, 6), (6, 2, 7), false);
        self.stone_box(p, (4, 3, 7), (6, 3, 7), false);
        for x in 4..=6 {
            self.set(p, stairs, x, 1, 4);
            self.set(p, stairs, x, 2, 5);
            self.set(p, stairs, x, 3, 6);
        }
        let mut eyes = [false; 12];
        for eye in &mut eyes {
            *eye = p.random.next_float() > 0.9;
        }
        let frame = |facing: &str, eye: bool| {
            state(
                Block::EndPortalFrame,
                &[
                    ("facing", facing),
                    ("eye", if eye { "true" } else { "false" }),
                ],
            )
        };
        for (i, (facing, x, z)) in [
            ("north", 4, 8),
            ("north", 5, 8),
            ("north", 6, 8),
            ("south", 4, 12),
            ("south", 5, 12),
            ("south", 6, 12),
            ("east", 3, 9),
            ("east", 3, 10),
            ("east", 3, 11),
            ("west", 7, 9),
            ("west", 7, 10),
            ("west", 7, 11),
        ]
        .into_iter()
        .enumerate()
        {
            self.set(p, frame(facing, eyes[i]), x, 3, z);
        }
        if eyes.iter().all(|e| *e) {
            let portal = Block::EndPortal.default_state();
            for z in 9..=11 {
                for x in 4..=6 {
                    self.set(p, portal, x, 3, z);
                }
            }
        }
        if !placed_spawner.load(Ordering::Relaxed) {
            let pos = self.frame.world_pos(5, 3, 6);
            if p.chunk.contains(pos) {
                placed_spawner.store(true, Ordering::Relaxed);
                p.level.set_block(pos, Block::Spawner.default_state());
                p.level.set_block_entity(
                    pos,
                    Compound::new()
                        .with("id", "minecraft:mob_spawner")
                        .with(
                            "SpawnData",
                            Compound::new()
                                .with("entity", Compound::new().with("id", "minecraft:silverfish")),
                        )
                        .with("SpawnPotentials", Tag::List(Vec::new())),
                );
            }
        }
    }

    fn filler(&self, p: &mut Place, steps: i32) {
        let b = bricks();
        for i in 0..steps {
            for x in 0..=4 {
                self.set(p, b, x, 0, i);
            }
            for y in 1..=3 {
                self.set(p, b, 0, y, i);
                for x in 1..=3 {
                    self.set(p, cave_air(), x, y, i);
                }
                self.set(p, b, 4, y, i);
            }
            for x in 0..=4 {
                self.set(p, b, x, 4, i);
            }
        }
    }
}

impl Piece for StrongholdPiece {
    fn kind(&self) -> PieceType {
        match self.kind {
            Kind::Start => PieceType::Shstart,
            Kind::StairsDown => PieceType::Shsd,
            Kind::Straight { .. } => PieceType::Shs,
            Kind::ChestCorridor { .. } => PieceType::Shcc,
            Kind::StraightStairsDown => PieceType::Shssd,
            Kind::LeftTurn => PieceType::Shlt,
            Kind::RightTurn => PieceType::Shrt,
            Kind::RoomCrossing { .. } => PieceType::Shrc,
            Kind::PrisonHall => PieceType::Shph,
            Kind::Library { .. } => PieceType::Shli,
            Kind::FiveCrossing { .. } => PieceType::Sh5c,
            Kind::PortalRoom { .. } => PieceType::Shpr,
            Kind::Filler { .. } => PieceType::Shfc,
        }
    }

    fn bounding_box(&self) -> BoundingBox {
        self.frame.bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        self.frame.orientation
    }

    fn gen_depth(&self) -> i32 {
        self.frame.gen_depth
    }

    fn load_progress(&self, tag: &Compound) {
        match &self.kind {
            Kind::ChestCorridor { placed_chest } if tag.get_bool("Chest") == Some(true) => {
                placed_chest.store(true, Ordering::Relaxed);
            }
            Kind::PortalRoom { placed_spawner } if tag.get_bool("Mob") == Some(true) => {
                placed_spawner.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("EntryDoor", self.door.name());
        match &self.kind {
            Kind::Start => {
                tag.insert("Source", true);
            }
            Kind::StairsDown => {
                tag.insert("Source", false);
            }
            Kind::Straight { left, right } => {
                tag.insert("Left", *left);
                tag.insert("Right", *right);
            }
            Kind::ChestCorridor { placed_chest } => {
                tag.insert("Chest", placed_chest.load(Ordering::Relaxed));
            }
            Kind::RoomCrossing { kind } => {
                tag.insert("Type", *kind);
            }
            Kind::Library { tall } => {
                tag.insert("Tall", *tall);
            }
            Kind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            } => {
                tag.insert("leftLow", *left_low);
                tag.insert("leftHigh", *left_high);
                tag.insert("rightLow", *right_low);
                tag.insert("rightHigh", *right_high);
            }
            Kind::PortalRoom { placed_spawner } => {
                tag.insert("Mob", placed_spawner.load(Ordering::Relaxed));
            }
            Kind::Filler { steps } => {
                tag.insert("Steps", *steps);
            }
            Kind::StraightStairsDown | Kind::LeftTurn | Kind::RightTurn | Kind::PrisonHall => {}
        }
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        _reference: BlockPos,
    ) {
        let mut p = Place {
            level: &mut *ctx.level,
            random: &mut *ctx.random,
            chunk: *chunk_box,
        };
        let p = &mut p;
        match &self.kind {
            Kind::Start | Kind::StairsDown => self.stairs_down(p),
            Kind::Straight { left, right } => self.straight(p, *left, *right),
            Kind::ChestCorridor { placed_chest } => self.chest_corridor(p, placed_chest),
            Kind::StraightStairsDown => self.straight_stairs_down(p),
            Kind::LeftTurn => self.turn(p, true),
            Kind::RightTurn => self.turn(p, false),
            Kind::RoomCrossing { kind } => self.room_crossing(p, *kind),
            Kind::PrisonHall => self.prison_hall(p),
            Kind::Library { tall } => self.library(p, *tall),
            Kind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            } => self.five_crossing(p, *left_low, *left_high, *right_low, *right_high),
            Kind::PortalRoom { placed_spawner } => self.portal_room(p, placed_spawner),
            Kind::Filler { steps } => self.filler(p, *steps),
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use tuff_item::random::WorldgenRandom;

    struct Fnv(u64);

    impl Hasher for Fnv {
        fn finish(&self) -> u64 {
            self.0
        }

        fn write(&mut self, bytes: &[u8]) {
            for b in bytes {
                self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
            }
        }
    }

    fn digest(seeds: impl Iterator<Item = (i64, i32, i32)>) -> (u64, usize) {
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        let mut count = 0;
        for (seed, x, z) in seeds {
            let mut random = WorldgenRandom::legacy(0);
            random.set_large_feature_seed(seed, x, z);
            let builder = grow(&mut random, ChunkPos::new(x, z));
            builder.has_portal_room.hash(&mut h);
            for p in &builder.pieces {
                count += 1;
                p.frame.bounding_box.hash(&mut h);
                p.frame.gen_depth.hash(&mut h);
                p.frame.orientation.hash(&mut h);
                std::mem::discriminant(&p.door).hash(&mut h);
                std::mem::discriminant(&p.kind).hash(&mut h);
                fields(&p.kind).hash(&mut h);
            }
            random.next_int().hash(&mut h);
        }
        (h.finish(), count)
    }

    fn fields(kind: &Kind) -> [i32; 4] {
        match *kind {
            Kind::Straight { left, right } => [left as i32, right as i32, 0, 0],
            Kind::RoomCrossing { kind } => [kind, 0, 0, 0],
            Kind::Library { tall } => [tall as i32, 0, 0, 0],
            Kind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            } => [
                left_low as i32,
                left_high as i32,
                right_low as i32,
                right_high as i32,
            ],
            Kind::Filler { steps } => [steps, 0, 0, 0],
            _ => [0; 4],
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn stronghold_layouts_are_pinned_for_fixed_seeds() {
        let seeds = (0..64i64).map(|i| {
            (
                i * 7919 - 100_000,
                (i as i32) * 37 - 900,
                450 - (i as i32) * 23,
            )
        });
        assert_eq!(digest(seeds), (1_649_728_420_780_739_855, 9332));
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "timing run: cargo test --release -- --ignored stronghold_layout_timing --nocapture"]
    fn stronghold_layout_timing() {
        let started = std::time::Instant::now();
        let (hash, count) = digest((0..4000i64).map(|i| (i * 31, i as i32 % 97, i as i32 / 97)));
        println!(
            "stronghold layouts: {count} pieces in {:?} ({hash})",
            started.elapsed()
        );
    }
}
