//! Nether fortresses: bridges and castle corridors grown from a bridge
//! crossing, each child picked by weight from the bridge or castle piece
//! lists, then moved to a random height between 48 and 70.

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
use tuff_item::random::{LegacyRandom, RandomSource};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const MAX_DEPTH: i32 = 30;
const START_Y: i32 = 64;
/// How far (in blocks) bridges may reach from the start before they end.
const MAX_REACH: i32 = 112;

struct Fortress;

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "fortress")?;
    Ok(Arc::new(Fortress))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    BridgeStraight,
    BridgeCrossing,
    RoomCrossing,
    StairsRoom,
    MonsterThrone,
    CastleEntrance,
    SmallCorridor,
    SmallCorridorCrossing,
    RightTurn,
    LeftTurn,
    CorridorStairs,
    TBalcony,
    StalkRoom,
}

enum Kind {
    Start,
    BridgeStraight,
    BridgeEndFiller { seed: i32 },
    BridgeCrossing,
    RoomCrossing,
    StairsRoom,
    MonsterThrone { placed_spawner: AtomicBool },
    CastleEntrance,
    StalkRoom,
    SmallCorridor,
    SmallCorridorCrossing,
    RightTurn { needs_chest: AtomicBool },
    LeftTurn { needs_chest: AtomicBool },
    CorridorStairs,
    TBalcony,
}

struct FortressPiece {
    frame: Frame,
    kind: Kind,
}

/// One entry of a weighted piece list.
#[derive(Debug, Clone)]
struct Weight {
    shape: Shape,
    weight: i32,
    placed: i32,
    max: i32,
    allow_in_row: bool,
}

impl Weight {
    const fn new(shape: Shape, weight: i32, max: i32, allow_in_row: bool) -> Self {
        Self {
            shape,
            weight,
            placed: 0,
            max,
            allow_in_row,
        }
    }

    fn can_place(&self) -> bool {
        self.max == 0 || self.placed < self.max
    }
}

fn bridge_weights() -> Vec<Weight> {
    use Shape::*;
    vec![
        Weight::new(BridgeStraight, 30, 0, true),
        Weight::new(BridgeCrossing, 10, 4, false),
        Weight::new(RoomCrossing, 10, 4, false),
        Weight::new(StairsRoom, 10, 3, false),
        Weight::new(MonsterThrone, 5, 2, false),
        Weight::new(CastleEntrance, 5, 1, false),
    ]
}

fn castle_weights() -> Vec<Weight> {
    use Shape::*;
    vec![
        Weight::new(SmallCorridor, 25, 0, true),
        Weight::new(SmallCorridorCrossing, 15, 5, false),
        Weight::new(RightTurn, 5, 10, false),
        Weight::new(LeftTurn, 5, 10, false),
        Weight::new(CorridorStairs, 10, 3, true),
        Weight::new(TBalcony, 7, 2, false),
        Weight::new(StalkRoom, 5, 2, false),
    ]
}

/// The fortress while it grows.
struct Builder {
    pieces: Vec<FortressPiece>,
    pending: Vec<usize>,
    bridges: Vec<Weight>,
    castles: Vec<Weight>,
    /// The list (castle or not) and shape of the last piece picked by
    /// weight.
    previous: Option<(bool, Shape)>,
}

impl Builder {
    fn collides(&self, b: &BoundingBox) -> bool {
        self.pieces
            .iter()
            .any(|p| p.frame.bounding_box.intersects(b))
    }

    fn start_box(&self) -> BoundingBox {
        self.pieces[0].frame.bounding_box
    }

    fn fits(&self, b: &BoundingBox) -> bool {
        b.min_y > 10 && !self.collides(b)
    }

    fn piece(&self, index: usize) -> &FortressPiece {
        &self.pieces[index]
    }
}

fn frame(depth: i32, bounding_box: BoundingBox, direction: Facing) -> Frame {
    Frame::new(depth, bounding_box, Some(direction))
}

fn end_filler(
    builder: &Builder,
    random: &mut dyn RandomSource,
    foot: BlockPos,
    direction: Facing,
    depth: i32,
) -> Option<FortressPiece> {
    let b = BoundingBox::orient(foot, [-1, -3, 0], 5, 10, 8, direction);
    if !builder.fits(&b) {
        return None;
    }
    Some(FortressPiece {
        frame: frame(depth, b, direction),
        kind: Kind::BridgeEndFiller {
            seed: random.next_int(),
        },
    })
}

fn create(
    builder: &Builder,
    shape: Shape,
    random: &mut dyn RandomSource,
    foot: BlockPos,
    direction: Facing,
    depth: i32,
) -> Option<FortressPiece> {
    let (off, size) = match shape {
        Shape::BridgeStraight => ([-1, -3, 0], [5, 10, 19]),
        Shape::BridgeCrossing => ([-8, -3, 0], [19, 10, 19]),
        Shape::RoomCrossing => ([-2, 0, 0], [7, 9, 7]),
        Shape::StairsRoom => ([-2, 0, 0], [7, 11, 7]),
        Shape::MonsterThrone => ([-2, 0, 0], [7, 8, 9]),
        Shape::CastleEntrance | Shape::StalkRoom => ([-5, -3, 0], [13, 14, 13]),
        Shape::SmallCorridor
        | Shape::SmallCorridorCrossing
        | Shape::RightTurn
        | Shape::LeftTurn => ([-1, 0, 0], [5, 7, 5]),
        Shape::CorridorStairs => ([-1, -7, 0], [5, 14, 10]),
        Shape::TBalcony => ([-3, 0, 0], [9, 7, 9]),
    };
    let b = BoundingBox::orient(foot, off, size[0], size[1], size[2], direction);
    if !builder.fits(&b) {
        return None;
    }
    let kind = match shape {
        Shape::BridgeStraight => Kind::BridgeStraight,
        Shape::BridgeCrossing => Kind::BridgeCrossing,
        Shape::RoomCrossing => Kind::RoomCrossing,
        Shape::StairsRoom => Kind::StairsRoom,
        Shape::MonsterThrone => Kind::MonsterThrone {
            placed_spawner: AtomicBool::new(false),
        },
        Shape::CastleEntrance => Kind::CastleEntrance,
        Shape::StalkRoom => Kind::StalkRoom,
        Shape::SmallCorridor => Kind::SmallCorridor,
        Shape::SmallCorridorCrossing => Kind::SmallCorridorCrossing,
        Shape::RightTurn => Kind::RightTurn {
            needs_chest: AtomicBool::new(random.next_int_bounded(3) == 0),
        },
        Shape::LeftTurn => Kind::LeftTurn {
            needs_chest: AtomicBool::new(random.next_int_bounded(3) == 0),
        },
        Shape::CorridorStairs => Kind::CorridorStairs,
        Shape::TBalcony => Kind::TBalcony,
    };
    Some(FortressPiece {
        frame: frame(depth, b, direction),
        kind,
    })
}

/// Picks a piece by weight (five tries), else ends the bridge.
fn pick_piece(
    builder: &mut Builder,
    castle: bool,
    random: &mut dyn RandomSource,
    foot: BlockPos,
    direction: Facing,
    depth: i32,
) -> Option<FortressPiece> {
    let list = if castle {
        &builder.castles
    } else {
        &builder.bridges
    };
    let any_limited = list.iter().any(|w| w.max > 0 && w.placed < w.max);
    let total: i32 = list.iter().map(|w| w.weight).sum();
    let total = if any_limited { total } else { -1 };
    if total > 0 && depth <= MAX_DEPTH {
        for _ in 0..5 {
            let mut choice = random.next_int_bounded(total);
            let count = if castle {
                builder.castles.len()
            } else {
                builder.bridges.len()
            };
            for i in 0..count {
                let w = if castle {
                    &builder.castles[i]
                } else {
                    &builder.bridges[i]
                };
                choice -= w.weight;
                if choice >= 0 {
                    continue;
                }
                if !w.can_place()
                    || (builder.previous == Some((castle, w.shape)) && !w.allow_in_row)
                {
                    break;
                }
                let shape = w.shape;
                if let Some(piece) = create(builder, shape, random, foot, direction, depth) {
                    let list = if castle {
                        &mut builder.castles
                    } else {
                        &mut builder.bridges
                    };
                    list[i].placed += 1;
                    builder.previous = Some((castle, shape));
                    if !list[i].can_place() {
                        list.remove(i);
                    }
                    return Some(piece);
                }
            }
        }
    }
    end_filler(builder, random, foot, direction, depth)
}

fn add_piece(
    builder: &mut Builder,
    random: &mut dyn RandomSource,
    foot: BlockPos,
    direction: Facing,
    depth: i32,
    castle: bool,
) {
    let start = builder.start_box();
    if (foot.x - start.min_x).abs() > MAX_REACH || (foot.z - start.min_z).abs() > MAX_REACH {
        end_filler(builder, random, foot, direction, depth);
        return;
    }
    if let Some(piece) = pick_piece(builder, castle, random, foot, direction, depth + 1) {
        builder.pieces.push(piece);
        builder.pending.push(builder.pieces.len() - 1);
    }
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
    castle: bool,
) {
    let piece = builder.piece(index);
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
    add_piece(builder, random, foot, direction, depth, castle);
}

fn add_children(builder: &mut Builder, index: usize, random: &mut dyn RandomSource) {
    use Side::*;
    match &builder.pieces[index].kind {
        Kind::Start | Kind::BridgeCrossing => {
            child(builder, index, random, Forward, 8, 3, false);
            child(builder, index, random, Left, 3, 8, false);
            child(builder, index, random, Right, 3, 8, false);
        }
        Kind::BridgeStraight => child(builder, index, random, Forward, 1, 3, false),
        Kind::RoomCrossing => {
            child(builder, index, random, Forward, 2, 0, false);
            child(builder, index, random, Left, 0, 2, false);
            child(builder, index, random, Right, 0, 2, false);
        }
        Kind::StairsRoom => child(builder, index, random, Right, 6, 2, false),
        Kind::CastleEntrance => child(builder, index, random, Forward, 5, 3, true),
        Kind::StalkRoom => {
            child(builder, index, random, Forward, 5, 3, true);
            child(builder, index, random, Forward, 5, 11, true);
        }
        Kind::SmallCorridor | Kind::CorridorStairs => {
            child(builder, index, random, Forward, 1, 0, true);
        }
        Kind::SmallCorridorCrossing => {
            child(builder, index, random, Forward, 1, 0, true);
            child(builder, index, random, Left, 0, 1, true);
            child(builder, index, random, Right, 0, 1, true);
        }
        Kind::RightTurn { .. } => child(builder, index, random, Right, 0, 1, true),
        Kind::LeftTurn { .. } => child(builder, index, random, Left, 0, 1, true),
        Kind::TBalcony => {
            let along = match builder.pieces[index].frame.orientation {
                Some(Facing::West | Facing::North) => 5,
                _ => 1,
            };
            let castle = random.next_int_bounded(8) > 0;
            child(builder, index, random, Left, 0, along, castle);
            let castle = random.next_int_bounded(8) > 0;
            child(builder, index, random, Right, 0, along, castle);
        }
        Kind::BridgeEndFiller { .. } | Kind::MonsterThrone { .. } => {}
    }
}

impl StructureKind for Fortress {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let chunk = ctx.chunk;
        Some(GenerationStub {
            position: BlockPos::new(chunk.x << 4, START_Y, chunk.z << 4),
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let random = &mut ctx.random;
                let direction = Facing::HORIZONTAL[random.next_int_bounded(4) as usize];
                let (west, north) = ((chunk.x << 4) + 2, (chunk.z << 4) + 2);
                let start =
                    BoundingBox::new(west, START_Y, north, west + 18, START_Y + 9, north + 18);
                let mut builder = Builder {
                    pieces: vec![FortressPiece {
                        frame: frame(0, start, direction),
                        kind: Kind::Start,
                    }],
                    pending: Vec::new(),
                    bridges: bridge_weights(),
                    castles: castle_weights(),
                    previous: None,
                };
                add_children(&mut builder, 0, random);
                while !builder.pending.is_empty() {
                    let at = random.next_int_bounded(builder.pending.len() as i32) as usize;
                    let index = builder.pending.remove(at);
                    add_children(&mut builder, index, random);
                }
                let mut b = builder.pieces[0].frame.bounding_box;
                for p in &builder.pieces[1..] {
                    b.encapsulate(&p.frame.bounding_box);
                }
                let (lowest, highest) = (48, 70);
                let span = highest - lowest + 1 - b.y_span();
                let y = if span > 1 {
                    lowest + random.next_int_bounded(span)
                } else {
                    lowest
                };
                let dy = y - b.min_y;
                builder
                    .pieces
                    .into_iter()
                    .map(|mut p| {
                        p.frame.bounding_box = p.frame.bounding_box.moved(0, dy, 0);
                        Box::new(p) as Box<dyn Piece>
                    })
                    .collect()
            }),
        })
    }
}

fn bricks() -> BlockState {
    Block::NetherBricks.default_state()
}

fn air() -> BlockState {
    Block::Air.default_state()
}

/// A nether brick fence connected on the named sides (`n`, `e`, `s`, `w`).
fn fence(sides: &str) -> BlockState {
    let mut state = Block::NetherBrickFence.default_state();
    for side in sides.chars() {
        let name = match side {
            'n' => "north",
            'e' => "east",
            's' => "south",
            _ => "west",
        };
        state = state.with(name, "true").unwrap_or(state);
    }
    state
}

fn stairs(facing: &str) -> BlockState {
    let state = Block::NetherBrickStairs.default_state();
    state.with("facing", facing).unwrap_or(state)
}

type Corner = (i32, i32, i32);

impl Framed for FortressPiece {
    fn frame(&self) -> &Frame {
        &self.frame
    }
}

impl FortressPiece {
    fn fill(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        from: Corner,
        to: Corner,
        state: BlockState,
    ) {
        self.generate_box(level, chunk, from, to, state, state, false);
    }

    fn set(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        state: BlockState,
        x: i32,
        y: i32,
        z: i32,
    ) {
        self.place_block(level, state, x, y, z, chunk);
    }

    fn pillars(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        xs: std::ops::RangeInclusive<i32>,
        zs: std::ops::RangeInclusive<i32>,
    ) {
        for x in xs {
            for z in zs.clone() {
                self.fill_column_down(level, bricks(), x, -1, z, chunk);
            }
        }
    }

    fn bridge_straight(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.fill(level, chunk, (0, 3, 0), (4, 4, 18), b);
        self.fill(level, chunk, (1, 5, 0), (3, 7, 18), air());
        self.fill(level, chunk, (0, 5, 0), (0, 5, 18), b);
        self.fill(level, chunk, (4, 5, 0), (4, 5, 18), b);
        self.fill(level, chunk, (0, 2, 0), (4, 2, 5), b);
        self.fill(level, chunk, (0, 2, 13), (4, 2, 18), b);
        self.fill(level, chunk, (0, 0, 0), (4, 1, 3), b);
        self.fill(level, chunk, (0, 0, 15), (4, 1, 18), b);
        self.pillars(level, chunk, 0..=4, 0..=2);
        self.pillars(level, chunk, 0..=4, 16..=18);
        let (east, west) = (fence("nse"), fence("nsw"));
        for (x, state) in [(0, east), (4, west)] {
            self.fill(level, chunk, (x, 1, 1), (x, 4, 1), state);
            self.fill(level, chunk, (x, 3, 4), (x, 4, 4), state);
            self.fill(level, chunk, (x, 3, 14), (x, 4, 14), state);
            self.fill(level, chunk, (x, 1, 17), (x, 4, 17), state);
        }
    }

    fn bridge_end_filler(&self, level: &mut Region, chunk: &BoundingBox, seed: i32) {
        let b = bricks();
        let mut random = LegacyRandom::new(seed as i64);
        for x in 0..=4 {
            for y in 3..=4 {
                let z = random.next_int_bounded(8);
                self.fill(level, chunk, (x, y, 0), (x, y, z), b);
            }
        }
        let z = random.next_int_bounded(8);
        self.fill(level, chunk, (0, 5, 0), (0, 5, z), b);
        let z = random.next_int_bounded(8);
        self.fill(level, chunk, (4, 5, 0), (4, 5, z), b);
        for x in 0..=4 {
            let z = random.next_int_bounded(5);
            self.fill(level, chunk, (x, 2, 0), (x, 2, z), b);
        }
        for x in 0..=4 {
            for y in 0..=1 {
                let z = random.next_int_bounded(3);
                self.fill(level, chunk, (x, y, 0), (x, y, z), b);
            }
        }
    }

    fn bridge_crossing(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.fill(level, chunk, (7, 3, 0), (11, 4, 18), b);
        self.fill(level, chunk, (0, 3, 7), (18, 4, 11), b);
        self.fill(level, chunk, (8, 5, 0), (10, 7, 18), air());
        self.fill(level, chunk, (0, 5, 8), (18, 7, 10), air());
        for (from, to) in [
            ((7, 5, 0), (7, 5, 7)),
            ((7, 5, 11), (7, 5, 18)),
            ((11, 5, 0), (11, 5, 7)),
            ((11, 5, 11), (11, 5, 18)),
            ((0, 5, 7), (7, 5, 7)),
            ((11, 5, 7), (18, 5, 7)),
            ((0, 5, 11), (7, 5, 11)),
            ((11, 5, 11), (18, 5, 11)),
            ((7, 2, 0), (11, 2, 5)),
            ((7, 2, 13), (11, 2, 18)),
            ((7, 0, 0), (11, 1, 3)),
            ((7, 0, 15), (11, 1, 18)),
        ] {
            self.fill(level, chunk, from, to, b);
        }
        self.pillars(level, chunk, 7..=11, 0..=2);
        self.pillars(level, chunk, 7..=11, 16..=18);
        self.fill(level, chunk, (0, 2, 7), (5, 2, 11), b);
        self.fill(level, chunk, (13, 2, 7), (18, 2, 11), b);
        self.fill(level, chunk, (0, 0, 7), (3, 1, 11), b);
        self.fill(level, chunk, (15, 0, 7), (18, 1, 11), b);
        self.pillars(level, chunk, 0..=2, 7..=11);
        self.pillars(level, chunk, 16..=18, 7..=11);
    }

    fn room_crossing(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.fill(level, chunk, (0, 0, 0), (6, 1, 6), b);
        self.fill(level, chunk, (0, 2, 0), (6, 7, 6), air());
        for (from, to) in [
            ((0, 2, 0), (1, 6, 0)),
            ((0, 2, 6), (1, 6, 6)),
            ((5, 2, 0), (6, 6, 0)),
            ((5, 2, 6), (6, 6, 6)),
            ((0, 2, 0), (0, 6, 1)),
            ((0, 2, 5), (0, 6, 6)),
            ((6, 2, 0), (6, 6, 1)),
            ((6, 2, 5), (6, 6, 6)),
        ] {
            self.fill(level, chunk, from, to, b);
        }
        let (we, ns) = (fence("we"), fence("ns"));
        self.fill(level, chunk, (2, 6, 0), (4, 6, 0), b);
        self.fill(level, chunk, (2, 5, 0), (4, 5, 0), we);
        self.fill(level, chunk, (2, 6, 6), (4, 6, 6), b);
        self.fill(level, chunk, (2, 5, 6), (4, 5, 6), we);
        self.fill(level, chunk, (0, 6, 2), (0, 6, 4), b);
        self.fill(level, chunk, (0, 5, 2), (0, 5, 4), ns);
        self.fill(level, chunk, (6, 6, 2), (6, 6, 4), b);
        self.fill(level, chunk, (6, 5, 2), (6, 5, 4), ns);
        self.pillars(level, chunk, 0..=6, 0..=6);
    }

    fn stairs_room(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.fill(level, chunk, (0, 0, 0), (6, 1, 6), b);
        self.fill(level, chunk, (0, 2, 0), (6, 10, 6), air());
        self.fill(level, chunk, (0, 2, 0), (1, 8, 0), b);
        self.fill(level, chunk, (5, 2, 0), (6, 8, 0), b);
        self.fill(level, chunk, (0, 2, 1), (0, 8, 6), b);
        self.fill(level, chunk, (6, 2, 1), (6, 8, 6), b);
        self.fill(level, chunk, (1, 2, 6), (5, 8, 6), b);
        let (we, ns) = (fence("we"), fence("ns"));
        self.fill(level, chunk, (0, 3, 2), (0, 5, 4), ns);
        self.fill(level, chunk, (6, 3, 2), (6, 5, 2), ns);
        self.fill(level, chunk, (6, 3, 4), (6, 5, 4), ns);
        self.set(level, chunk, b, 5, 2, 5);
        for (x, top) in [(4, 3), (3, 4), (2, 5), (1, 6)] {
            self.fill(level, chunk, (x, 2, 5), (x, top, 5), b);
        }
        self.fill(level, chunk, (1, 7, 1), (5, 7, 4), b);
        self.fill(level, chunk, (6, 8, 2), (6, 8, 4), air());
        self.fill(level, chunk, (2, 6, 0), (4, 8, 0), b);
        self.fill(level, chunk, (2, 5, 0), (4, 5, 0), we);
        self.pillars(level, chunk, 0..=6, 0..=6);
    }

    fn monster_throne(&self, level: &mut Region, chunk: &BoundingBox, placed_spawner: &AtomicBool) {
        let b = bricks();
        self.fill(level, chunk, (0, 2, 0), (6, 7, 7), air());
        for (from, to) in [
            ((1, 0, 0), (5, 1, 7)),
            ((1, 2, 1), (5, 2, 7)),
            ((1, 3, 2), (5, 3, 7)),
            ((1, 4, 3), (5, 4, 7)),
            ((1, 2, 0), (1, 4, 2)),
            ((5, 2, 0), (5, 4, 2)),
            ((1, 5, 2), (1, 5, 3)),
            ((5, 5, 2), (5, 5, 3)),
            ((0, 5, 3), (0, 5, 8)),
            ((6, 5, 3), (6, 5, 8)),
            ((1, 5, 8), (5, 5, 8)),
        ] {
            self.fill(level, chunk, from, to, b);
        }
        let (we, ns) = (fence("we"), fence("ns"));
        self.set(level, chunk, fence("w"), 1, 6, 3);
        self.set(level, chunk, fence("e"), 5, 6, 3);
        self.set(level, chunk, fence("en"), 0, 6, 3);
        self.set(level, chunk, fence("wn"), 6, 6, 3);
        self.fill(level, chunk, (0, 6, 4), (0, 6, 7), ns);
        self.fill(level, chunk, (6, 6, 4), (6, 6, 7), ns);
        self.set(level, chunk, fence("es"), 0, 6, 8);
        self.set(level, chunk, fence("ws"), 6, 6, 8);
        self.fill(level, chunk, (1, 6, 8), (5, 6, 8), we);
        self.set(level, chunk, fence("e"), 1, 7, 8);
        self.fill(level, chunk, (2, 7, 8), (4, 7, 8), we);
        self.set(level, chunk, fence("w"), 5, 7, 8);
        self.set(level, chunk, fence("e"), 2, 8, 8);
        self.set(level, chunk, we, 3, 8, 8);
        self.set(level, chunk, fence("w"), 4, 8, 8);
        if !placed_spawner.load(Ordering::Relaxed) {
            let pos = self.frame.world_pos(3, 5, 5);
            if chunk.contains(pos) {
                placed_spawner.store(true, Ordering::Relaxed);
                level.set_block(pos, Block::Spawner.default_state());
                level.set_block_entity(
                    pos,
                    Compound::new()
                        .with("id", "minecraft:mob_spawner")
                        .with(
                            "SpawnData",
                            Compound::new()
                                .with("entity", Compound::new().with("id", "minecraft:blaze")),
                        )
                        .with("SpawnPotentials", Tag::List(Vec::new())),
                );
            }
        }
        self.pillars(level, chunk, 0..=6, 0..=6);
    }

    /// The walls, battlements and railings castle entrances and stalk
    /// rooms share.
    fn castle_shell(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.fill(level, chunk, (0, 3, 0), (12, 4, 12), b);
        self.fill(level, chunk, (0, 5, 0), (12, 13, 12), air());
        for (from, to) in [
            ((0, 5, 0), (1, 12, 12)),
            ((11, 5, 0), (12, 12, 12)),
            ((2, 5, 11), (4, 12, 12)),
            ((8, 5, 11), (10, 12, 12)),
            ((5, 9, 11), (7, 12, 12)),
            ((2, 5, 0), (4, 12, 1)),
            ((8, 5, 0), (10, 12, 1)),
            ((5, 9, 0), (7, 12, 1)),
            ((2, 11, 2), (10, 12, 10)),
        ] {
            self.fill(level, chunk, from, to, b);
        }
    }

    fn castle_battlements(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        let (we, ns) = (fence("we"), fence("ns"));
        for i in (1..=11).step_by(2) {
            self.fill(level, chunk, (i, 10, 0), (i, 11, 0), we);
            self.fill(level, chunk, (i, 10, 12), (i, 11, 12), we);
            self.fill(level, chunk, (0, 10, i), (0, 11, i), ns);
            self.fill(level, chunk, (12, 10, i), (12, 11, i), ns);
            self.set(level, chunk, b, i, 13, 0);
            self.set(level, chunk, b, i, 13, 12);
            self.set(level, chunk, b, 0, 13, i);
            self.set(level, chunk, b, 12, 13, i);
            if i != 11 {
                self.set(level, chunk, we, i + 1, 13, 0);
                self.set(level, chunk, we, i + 1, 13, 12);
                self.set(level, chunk, ns, 0, 13, i + 1);
                self.set(level, chunk, ns, 12, 13, i + 1);
            }
        }
        self.set(level, chunk, fence("ne"), 0, 13, 0);
        self.set(level, chunk, fence("se"), 0, 13, 12);
        self.set(level, chunk, fence("sw"), 12, 13, 12);
        self.set(level, chunk, fence("nw"), 12, 13, 0);
        for z in (3..=9).step_by(2) {
            self.fill(level, chunk, (1, 7, z), (1, 8, z), fence("nsw"));
            self.fill(level, chunk, (11, 7, z), (11, 8, z), fence("nse"));
        }
    }

    fn castle_floor(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        for (from, to) in [
            ((4, 2, 0), (8, 2, 12)),
            ((0, 2, 4), (12, 2, 8)),
            ((4, 0, 0), (8, 1, 3)),
            ((4, 0, 9), (8, 1, 12)),
            ((0, 0, 4), (3, 1, 8)),
            ((9, 0, 4), (12, 1, 8)),
        ] {
            self.fill(level, chunk, from, to, b);
        }
        self.pillars(level, chunk, 4..=8, 0..=2);
        self.pillars(level, chunk, 4..=8, 10..=12);
        self.pillars(level, chunk, 0..=2, 4..=8);
        self.pillars(level, chunk, 10..=12, 4..=8);
    }

    fn castle_entrance(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.castle_shell(level, chunk);
        let plain = Block::NetherBrickFence.default_state();
        self.fill(level, chunk, (5, 8, 0), (7, 8, 0), plain);
        self.castle_battlements(level, chunk);
        self.castle_floor(level, chunk);
        self.fill(level, chunk, (5, 5, 5), (7, 5, 7), b);
        self.fill(level, chunk, (6, 1, 6), (6, 4, 6), air());
        self.set(level, chunk, b, 6, 0, 6);
        self.set(level, chunk, Block::Lava.default_state(), 6, 5, 6);
    }

    fn stalk_room(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.castle_shell(level, chunk);
        self.castle_battlements(level, chunk);
        let north = stairs("north");
        for i in 0..=6 {
            let z = i + 4;
            for x in 5..=7 {
                self.set(level, chunk, north, x, 5 + i, z);
            }
            if (5..=8).contains(&z) {
                self.fill(level, chunk, (5, 5, z), (7, i + 4, z), b);
            } else if (9..=10).contains(&z) {
                self.fill(level, chunk, (5, 8, z), (7, i + 4, z), b);
            }
            if i >= 1 {
                self.fill(level, chunk, (5, 6 + i, z), (7, 9 + i, z), air());
            }
        }
        for x in 5..=7 {
            self.set(level, chunk, north, x, 12, 11);
        }
        self.fill(level, chunk, (5, 6, 7), (5, 7, 7), fence("nse"));
        self.fill(level, chunk, (7, 6, 7), (7, 7, 7), fence("nsw"));
        self.fill(level, chunk, (5, 13, 12), (7, 13, 12), air());
        for (from, to) in [
            ((2, 5, 2), (3, 5, 3)),
            ((2, 5, 9), (3, 5, 10)),
            ((2, 5, 4), (2, 5, 8)),
            ((9, 5, 2), (10, 5, 3)),
            ((9, 5, 9), (10, 5, 10)),
            ((10, 5, 4), (10, 5, 8)),
        ] {
            self.fill(level, chunk, from, to, b);
        }
        let (east, west) = (stairs("east"), stairs("west"));
        for z in [2, 3, 9, 10] {
            self.set(level, chunk, west, 4, 5, z);
        }
        for z in [2, 3, 9, 10] {
            self.set(level, chunk, east, 8, 5, z);
        }
        let (sand, wart) = (
            Block::SoulSand.default_state(),
            Block::NetherWart.default_state(),
        );
        self.fill(level, chunk, (3, 4, 4), (4, 4, 8), sand);
        self.fill(level, chunk, (8, 4, 4), (9, 4, 8), sand);
        self.fill(level, chunk, (3, 5, 4), (4, 5, 8), wart);
        self.fill(level, chunk, (8, 5, 4), (9, 5, 8), wart);
        self.castle_floor(level, chunk);
    }

    fn small_corridor_base(&self, level: &mut Region, chunk: &BoundingBox) {
        self.fill(level, chunk, (0, 0, 0), (4, 1, 4), bricks());
        self.fill(level, chunk, (0, 2, 0), (4, 5, 4), air());
    }

    fn small_corridor_roof(&self, level: &mut Region, chunk: &BoundingBox) {
        self.fill(level, chunk, (0, 6, 0), (4, 6, 4), bricks());
        self.pillars(level, chunk, 0..=4, 0..=4);
    }

    fn small_corridor(&self, level: &mut Region, chunk: &BoundingBox) {
        let (b, ns) = (bricks(), fence("ns"));
        self.small_corridor_base(level, chunk);
        self.fill(level, chunk, (0, 2, 0), (0, 5, 4), b);
        self.fill(level, chunk, (4, 2, 0), (4, 5, 4), b);
        for (x, z) in [(0, 1), (0, 3), (4, 1), (4, 3)] {
            self.fill(level, chunk, (x, 3, z), (x, 4, z), ns);
        }
        self.small_corridor_roof(level, chunk);
    }

    fn small_corridor_crossing(&self, level: &mut Region, chunk: &BoundingBox) {
        let b = bricks();
        self.small_corridor_base(level, chunk);
        for (x, z) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
            self.fill(level, chunk, (x, 2, z), (x, 5, z), b);
        }
        self.small_corridor_roof(level, chunk);
    }

    fn turn(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        needs_chest: &AtomicBool,
        right: bool,
    ) {
        let (b, we, ns) = (bricks(), fence("we"), fence("ns"));
        self.small_corridor_base(level, chunk);
        let (wall, open) = if right { (0, 4) } else { (4, 0) };
        self.fill(level, chunk, (wall, 2, 0), (wall, 5, 4), b);
        self.fill(level, chunk, (wall, 3, 1), (wall, 4, 1), ns);
        self.fill(level, chunk, (wall, 3, 3), (wall, 4, 3), ns);
        self.fill(level, chunk, (open, 2, 0), (open, 5, 0), b);
        let back = if right { (1, 4) } else { (0, 3) };
        self.fill(level, chunk, (back.0, 2, 4), (back.1, 5, 4), b);
        self.fill(level, chunk, (1, 3, 4), (1, 4, 4), we);
        self.fill(level, chunk, (3, 3, 4), (3, 4, 4), we);
        let chest_x = if right { 1 } else { 3 };
        if needs_chest.load(Ordering::Relaxed)
            && chunk.contains(self.frame.world_pos(chest_x, 2, 3))
        {
            needs_chest.store(false, Ordering::Relaxed);
            self.create_chest(
                level,
                chunk,
                random,
                chest_x,
                2,
                3,
                structure_loot("fortress"),
            );
        }
        self.small_corridor_roof(level, chunk);
    }

    fn corridor_stairs(&self, level: &mut Region, chunk: &BoundingBox) {
        let (b, south, ns) = (bricks(), stairs("south"), fence("ns"));
        for step in 0..=9 {
            let floor = (7 - step).max(1);
            let roof = (floor + 5).max(14 - step).min(13);
            let z = step;
            self.fill(level, chunk, (0, 0, z), (4, floor, z), b);
            self.fill(level, chunk, (1, floor + 1, z), (3, roof - 1, z), air());
            if step <= 6 {
                for x in 1..=3 {
                    self.set(level, chunk, south, x, floor + 1, z);
                }
            }
            self.fill(level, chunk, (0, roof, z), (4, roof, z), b);
            self.fill(level, chunk, (0, floor + 1, z), (0, roof - 1, z), b);
            self.fill(level, chunk, (4, floor + 1, z), (4, roof - 1, z), b);
            if step & 1 == 0 {
                self.fill(level, chunk, (0, floor + 2, z), (0, floor + 3, z), ns);
                self.fill(level, chunk, (4, floor + 2, z), (4, floor + 3, z), ns);
            }
            for x in 0..=4 {
                self.fill_column_down(level, b, x, -1, z, chunk);
            }
        }
    }

    fn t_balcony(&self, level: &mut Region, chunk: &BoundingBox) {
        let (b, ns, we) = (bricks(), fence("ns"), fence("we"));
        self.fill(level, chunk, (0, 0, 0), (8, 1, 8), b);
        self.fill(level, chunk, (0, 2, 0), (8, 5, 8), air());
        self.fill(level, chunk, (0, 6, 0), (8, 6, 5), b);
        self.fill(level, chunk, (0, 2, 0), (2, 5, 0), b);
        self.fill(level, chunk, (6, 2, 0), (8, 5, 0), b);
        self.fill(level, chunk, (1, 3, 0), (1, 4, 0), we);
        self.fill(level, chunk, (7, 3, 0), (7, 4, 0), we);
        self.fill(level, chunk, (0, 2, 4), (8, 2, 8), b);
        self.fill(level, chunk, (1, 1, 4), (2, 2, 4), air());
        self.fill(level, chunk, (6, 1, 4), (7, 2, 4), air());
        self.fill(level, chunk, (1, 3, 8), (7, 3, 8), we);
        self.set(level, chunk, fence("es"), 0, 3, 8);
        self.set(level, chunk, fence("ws"), 8, 3, 8);
        self.fill(level, chunk, (0, 3, 6), (0, 3, 7), ns);
        self.fill(level, chunk, (8, 3, 6), (8, 3, 7), ns);
        self.fill(level, chunk, (0, 3, 4), (0, 5, 5), b);
        self.fill(level, chunk, (8, 3, 4), (8, 5, 5), b);
        self.fill(level, chunk, (1, 3, 5), (2, 5, 5), b);
        self.fill(level, chunk, (6, 3, 5), (7, 5, 5), b);
        self.fill(level, chunk, (1, 4, 5), (1, 5, 5), we);
        self.fill(level, chunk, (7, 4, 5), (7, 5, 5), we);
        for z in 0..=5 {
            for x in 0..=8 {
                self.fill_column_down(level, b, x, -1, z, chunk);
            }
        }
    }
}

impl Piece for FortressPiece {
    fn kind(&self) -> PieceType {
        match self.kind {
            Kind::Start | Kind::BridgeCrossing => PieceType::Nebcr,
            Kind::BridgeStraight => PieceType::Nebs,
            Kind::BridgeEndFiller { .. } => PieceType::Nebef,
            Kind::RoomCrossing => PieceType::Nerc,
            Kind::StairsRoom => PieceType::Nesr,
            Kind::MonsterThrone { .. } => PieceType::Nemt,
            Kind::CastleEntrance => PieceType::Nece,
            Kind::StalkRoom => PieceType::Necsr,
            Kind::SmallCorridor => PieceType::Nesc,
            Kind::SmallCorridorCrossing => PieceType::Nescsc,
            Kind::RightTurn { .. } => PieceType::Nescrt,
            Kind::LeftTurn { .. } => PieceType::Nesclt,
            Kind::CorridorStairs => PieceType::Neccs,
            Kind::TBalcony => PieceType::Nectb,
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
            Kind::MonsterThrone { placed_spawner } => {
                if tag.get_bool("Mob") == Some(true) {
                    placed_spawner.store(true, Ordering::Relaxed);
                }
            }
            Kind::RightTurn { needs_chest } | Kind::LeftTurn { needs_chest }
                if tag.get_bool("Chest") == Some(false) =>
            {
                needs_chest.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn save_extra(&self, tag: &mut Compound) {
        match &self.kind {
            Kind::BridgeEndFiller { seed } => {
                tag.insert("Seed", *seed);
            }
            Kind::MonsterThrone { placed_spawner } => {
                tag.insert("Mob", placed_spawner.load(Ordering::Relaxed));
            }
            Kind::RightTurn { needs_chest } | Kind::LeftTurn { needs_chest } => {
                tag.insert("Chest", needs_chest.load(Ordering::Relaxed));
            }
            _ => {}
        }
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        _reference: BlockPos,
    ) {
        let chunk = *chunk_box;
        let level = &mut *ctx.level;
        match &self.kind {
            Kind::Start | Kind::BridgeCrossing => self.bridge_crossing(level, &chunk),
            Kind::BridgeStraight => self.bridge_straight(level, &chunk),
            Kind::BridgeEndFiller { seed } => self.bridge_end_filler(level, &chunk, *seed),
            Kind::RoomCrossing => self.room_crossing(level, &chunk),
            Kind::StairsRoom => self.stairs_room(level, &chunk),
            Kind::MonsterThrone { placed_spawner } => {
                self.monster_throne(level, &chunk, placed_spawner)
            }
            Kind::CastleEntrance => self.castle_entrance(level, &chunk),
            Kind::StalkRoom => self.stalk_room(level, &chunk),
            Kind::SmallCorridor => self.small_corridor(level, &chunk),
            Kind::SmallCorridorCrossing => self.small_corridor_crossing(level, &chunk),
            Kind::RightTurn { needs_chest } => {
                self.turn(level, &chunk, &mut *ctx.random, needs_chest, true)
            }
            Kind::LeftTurn { needs_chest } => {
                self.turn(level, &chunk, &mut *ctx.random, needs_chest, false)
            }
            Kind::CorridorStairs => self.corridor_stairs(level, &chunk),
            Kind::TBalcony => self.t_balcony(level, &chunk),
        }
    }
}
