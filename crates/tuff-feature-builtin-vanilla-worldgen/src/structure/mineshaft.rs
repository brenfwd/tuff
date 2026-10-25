//! Mineshafts: corridors, crossings, rooms and stairs grown from a room,
//! then sunk below sea level (or, for badlands mineshafts, moved to the
//! surface height).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, BlockState, Support};

use crate::feature::level::Region;
use crate::json::{Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, data_value_2d};
use crate::structure::piece::{Frame, Framed, is_replaceable_by_structures};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use tuff_item::random::RandomSource;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

const MAX_DEPTH: i32 = 8;
const START_Y: i32 = 50;

#[derive(Debug, Clone, Copy)]
struct Wood {
    index: usize,
    log: BlockState,
    planks: BlockState,
    fence: BlockState,
}

impl Wood {
    fn by_name(name: &str) -> Option<Self> {
        tuff_vanilla_data::world::MINESHAFT_TYPES
            .iter()
            .enumerate()
            .find(|(_, (n, ..))| *n == name)
            .map(|(index, (_, log, planks, fence))| Self {
                index,
                log: log.default_state(),
                planks: planks.default_state(),
                fence: fence.default_state(),
            })
    }

    fn is_mesa(self) -> bool {
        tuff_vanilla_data::world::MINESHAFT_TYPES[self.index].0 == "mesa"
    }
}

struct Mineshaft {
    wood: Wood,
}

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    let o = obj(config, &["mineshaft_type"], "mineshaft")?;
    let name = o.str("mineshaft_type")?;
    let wood = Wood::by_name(name).ok_or_else(|| format!("unknown mineshaft type {name}"))?;
    Ok(Arc::new(Mineshaft { wood }))
}

enum Kind {
    Room {
        entrances: Vec<BoundingBox>,
    },
    Corridor {
        has_rails: bool,
        spider: bool,
        placed_spider: AtomicBool,
        sections: i32,
    },
    Crossing {
        direction: Facing,
        two_floored: bool,
    },
    Stairs,
}

struct MinePiece {
    frame: Frame,
    wood: Wood,
    kind: Kind,
}

/// Pieces while the mineshaft grows.
struct Builder {
    pieces: Vec<MinePiece>,
    /// Bounds of `pieces`, packed densely for the collision scan.
    occupied: Vec<BoundingBox>,
}

impl Builder {
    fn new(root: MinePiece) -> Self {
        Self {
            occupied: vec![root.frame.bounding_box],
            pieces: vec![root],
        }
    }

    fn push(&mut self, piece: MinePiece) -> usize {
        self.occupied.push(piece.frame.bounding_box);
        self.pieces.push(piece);
        self.pieces.len() - 1
    }

    fn collides(&self, b: &BoundingBox) -> bool {
        let overlaps = |o: &BoundingBox| {
            (o.max_x >= b.min_x)
                & (o.min_x <= b.max_x)
                & (o.max_z >= b.min_z)
                & (o.min_z <= b.max_z)
                & (o.max_y >= b.min_y)
                & (o.min_y <= b.max_y)
        };
        self.occupied
            .chunks(8)
            .any(|group| group.iter().fold(false, |hit, o| hit | overlaps(o)))
    }

    fn bounding_box(&self) -> BoundingBox {
        let mut out = self.pieces[0].frame.bounding_box;
        for p in &self.pieces[1..] {
            out.encapsulate(&p.frame.bounding_box);
        }
        out
    }

    fn offset_vertically(&mut self, dy: i32) {
        for b in &mut self.occupied {
            *b = b.moved(0, dy, 0);
        }
        for piece in &mut self.pieces {
            piece.frame.bounding_box = piece.frame.bounding_box.moved(0, dy, 0);
            if let Kind::Room { entrances } = &mut piece.kind {
                for b in entrances {
                    *b = b.moved(0, dy, 0);
                }
            }
        }
    }

    fn move_below_sea_level(
        &mut self,
        sea_level: i32,
        min_y: i32,
        random: &mut dyn RandomSource,
        offset: i32,
    ) -> i32 {
        let max_y = sea_level - offset;
        let b = self.bounding_box();
        let mut y = b.y_span() + min_y + 1;
        if y < max_y {
            y += random.next_int_bounded(max_y - y);
        }
        let dy = y - b.max_y;
        self.offset_vertically(dy);
        dy
    }
}

type Foot = (i32, i32, i32);

/// A heading split into the axis it runs along and whether it counts up.
#[derive(Clone, Copy)]
struct Heading {
    along_z: bool,
    ascending: bool,
}

impl Heading {
    #[inline]
    fn of(facing: Facing) -> Self {
        match facing {
            Facing::South => Self {
                along_z: true,
                ascending: true,
            },
            Facing::West => Self {
                along_z: false,
                ascending: false,
            },
            Facing::East => Self {
                along_z: false,
                ascending: true,
            },
            _ => Self {
                along_z: true,
                ascending: false,
            },
        }
    }

    #[inline]
    fn left(self) -> Facing {
        if self.along_z {
            Facing::West
        } else {
            Facing::North
        }
    }

    #[inline]
    fn right(self) -> Facing {
        if self.along_z {
            Facing::East
        } else {
            Facing::South
        }
    }

    #[inline]
    fn at(self, lateral: i32, y: i32, forward: i32) -> Foot {
        if self.along_z {
            (lateral, y, forward)
        } else {
            (forward, y, lateral)
        }
    }

    #[inline]
    fn lateral_range(self, b: &BoundingBox) -> (i32, i32) {
        if self.along_z {
            (b.min_x, b.max_x)
        } else {
            (b.min_z, b.max_z)
        }
    }

    #[inline]
    fn forward_range(self, b: &BoundingBox) -> (i32, i32) {
        if self.along_z {
            (b.min_z, b.max_z)
        } else {
            (b.min_x, b.max_x)
        }
    }

    #[inline]
    fn far_end(self, b: &BoundingBox) -> i32 {
        let (lo, hi) = self.forward_range(b);
        if self.ascending { hi } else { lo }
    }

    #[inline]
    fn step(self) -> i32 {
        if self.ascending { 1 } else { -1 }
    }

    /// A box reaching `length` blocks forward from `foot`, `lateral` and
    /// `height` given relative to the foot.
    #[inline]
    fn reach(
        self,
        foot: Foot,
        lateral: (i32, i32),
        height: (i32, i32),
        length: i32,
    ) -> BoundingBox {
        let (f0, f1) = if self.ascending {
            (0, length - 1)
        } else {
            (1 - length, 0)
        };
        let (a, b) = (
            self.at(lateral.0, height.0, f0),
            self.at(lateral.1, height.1, f1),
        );
        BoundingBox::new(
            a.0 + foot.0,
            a.1 + foot.1,
            a.2 + foot.2,
            b.0 + foot.0,
            b.1 + foot.1,
            b.2 + foot.2,
        )
    }

    /// Just past the far end of `b`, aligned with its low lateral edge.
    #[inline]
    fn straight_exit(self, b: &BoundingBox, y: i32) -> Foot {
        self.at(self.lateral_range(b).0, y, self.far_end(b) + self.step())
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Corridor,
    Crossing,
    Stairs,
}

fn pick_shape(roll: i32) -> Shape {
    match roll {
        80.. => Shape::Crossing,
        70.. => Shape::Stairs,
        _ => Shape::Corridor,
    }
}

fn free(builder: &Builder, b: BoundingBox) -> Option<BoundingBox> {
    (!builder.collides(&b)).then_some(b)
}

fn make_piece(
    builder: &Builder,
    random: &mut dyn RandomSource,
    foot: Foot,
    facing: Facing,
    depth: i32,
    wood: Wood,
) -> Option<MinePiece> {
    let heading = Heading::of(facing);
    let (bounds, orientation, kind) = match pick_shape(random.next_int_bounded(100)) {
        Shape::Crossing => {
            let top = if random.next_int_bounded(4) == 0 {
                6
            } else {
                2
            };
            let bounds = free(builder, heading.reach(foot, (-1, 3), (0, top), 5))?;
            let kind = Kind::Crossing {
                direction: facing,
                two_floored: bounds.y_span() > 3,
            };
            (bounds, None, kind)
        }
        Shape::Stairs => {
            let bounds = free(builder, heading.reach(foot, (0, 2), (-5, 2), 9))?;
            (bounds, Some(facing), Kind::Stairs)
        }
        Shape::Corridor => {
            let longest = random.next_int_bounded(3) + 2;
            let bounds = (1..=longest)
                .rev()
                .find_map(|n| free(builder, heading.reach(foot, (0, 2), (0, 2), n * 5)))?;
            let has_rails = random.next_int_bounded(3) == 0;
            let spider = !has_rails && random.next_int_bounded(23) == 0;
            let (lo, hi) = heading.forward_range(&bounds);
            let kind = Kind::Corridor {
                has_rails,
                spider,
                placed_spider: AtomicBool::new(false),
                sections: (hi - lo + 1) / 5,
            };
            (bounds, Some(facing), kind)
        }
    };
    Some(MinePiece {
        frame: Frame::new(depth, bounds, orientation),
        wood,
        kind,
    })
}

/// Grows a piece at `foot` facing `facing` and its children; the index of
/// the new piece.
fn add_piece(
    builder: &mut Builder,
    random: &mut dyn RandomSource,
    foot: Foot,
    facing: Facing,
    depth: i32,
) -> Option<usize> {
    let origin = builder.pieces[0].frame.bounding_box;
    let too_far = (foot.0 - origin.min_x).abs() > 80 || (foot.2 - origin.min_z).abs() > 80;
    if depth > MAX_DEPTH || too_far {
        return None;
    }
    let wood = builder.pieces[0].wood;
    let piece = make_piece(builder, random, foot, facing, depth + 1, wood)?;
    let index = builder.push(piece);
    add_children(builder, index, random);
    Some(index)
}

fn add_children(builder: &mut Builder, index: usize, random: &mut dyn RandomSource) {
    let frame = builder.pieces[index].frame;
    match builder.pieces[index].kind {
        Kind::Room { .. } => room_children(builder, index, random),
        Kind::Corridor { .. } => {
            if let Some(facing) = frame.orientation {
                corridor_children(builder, random, frame.bounding_box, facing, frame.gen_depth);
            }
        }
        Kind::Crossing {
            direction,
            two_floored,
        } => crossing_children(
            builder,
            random,
            frame.bounding_box,
            direction,
            two_floored,
            frame.gen_depth,
        ),
        Kind::Stairs => {
            if let Some(facing) = frame.orientation {
                let b = frame.bounding_box;
                let exit = Heading::of(facing).straight_exit(&b, b.min_y);
                add_piece(builder, random, exit, facing, frame.gen_depth);
            }
        }
    }
}

fn corridor_children(
    builder: &mut Builder,
    random: &mut dyn RandomSource,
    b: BoundingBox,
    facing: Facing,
    depth: i32,
) {
    let heading = Heading::of(facing);
    let (lat_lo, lat_hi) = heading.lateral_range(&b);
    let (fwd_lo, fwd_hi) = heading.forward_range(&b);
    let choice = random.next_int_bounded(4);
    let y = b.min_y - 1 + random.next_int_bounded(3);
    let last_segment = if heading.ascending {
        fwd_hi - 3
    } else {
        fwd_lo
    };
    let (exit, exit_facing) = match choice {
        0 | 1 => (heading.straight_exit(&b, y), facing),
        2 => (heading.at(lat_lo - 1, y, last_segment), heading.left()),
        _ => (heading.at(lat_hi + 1, y, last_segment), heading.right()),
    };
    add_piece(builder, random, exit, exit_facing, depth);

    if depth >= MAX_DEPTH {
        return;
    }
    let mut along = fwd_lo + 3;
    while along + 3 <= fwd_hi {
        let side = match random.next_int_bounded(5) {
            0 => Some((lat_lo - 1, heading.left())),
            1 => Some((lat_hi + 1, heading.right())),
            _ => None,
        };
        if let Some((lateral, side_facing)) = side {
            let exit = heading.at(lateral, b.min_y, along);
            add_piece(builder, random, exit, side_facing, depth + 1);
        }
        along += 5;
    }
}

fn crossing_children(
    builder: &mut Builder,
    random: &mut dyn RandomSource,
    b: BoundingBox,
    entered: Facing,
    two_floored: bool,
    depth: i32,
) {
    let exits = [
        (Facing::North, (b.min_x + 1, b.min_y, b.min_z - 1)),
        (Facing::South, (b.min_x + 1, b.min_y, b.max_z + 1)),
        (Facing::West, (b.min_x - 1, b.min_y, b.min_z + 1)),
        (Facing::East, (b.max_x + 1, b.min_y, b.min_z + 1)),
    ];
    let back = match entered {
        Facing::South => Facing::North,
        Facing::West => Facing::East,
        Facing::East => Facing::West,
        _ => Facing::South,
    };
    for &(facing, foot) in &exits {
        if facing != back {
            add_piece(builder, random, foot, facing, depth);
        }
    }
    if two_floored {
        for slot in [0, 2, 3, 1] {
            let (facing, (x, y, z)) = exits[slot];
            if random.next_bool() {
                add_piece(builder, random, (x, y + 4, z), facing, depth);
            }
        }
    }
}

fn room_children(builder: &mut Builder, index: usize, random: &mut dyn RandomSource) {
    let room = builder.pieces[index].frame.bounding_box;
    let depth = builder.pieces[index].frame.gen_depth;
    let door_heights = (room.y_span() - 4).max(1);
    for wall in [Facing::North, Facing::South, Facing::West, Facing::East] {
        let heading = Heading::of(wall);
        let (lat_lo, lat_hi) = heading.lateral_range(&room);
        let width = lat_hi - lat_lo + 1;
        let outside = heading.far_end(&room) + heading.step();
        let wall_line = heading.far_end(&room);
        let wall_inner = wall_line - heading.step();
        let mut cursor = 0;
        while cursor < width {
            let door = cursor + random.next_int_bounded(width);
            if door + 3 > width {
                break;
            }
            let y = room.min_y + random.next_int_bounded(door_heights) + 1;
            let foot = heading.at(lat_lo + door, y, outside);
            if let Some(child) = add_piece(builder, random, foot, wall, depth) {
                let c = builder.pieces[child].frame.bounding_box;
                let (c_lo, c_hi) = heading.lateral_range(&c);
                let (w0, w1) = (wall_inner.min(wall_line), wall_inner.max(wall_line));
                let a = heading.at(c_lo, c.min_y, w0);
                let z = heading.at(c_hi, c.max_y, w1);
                let entrance = BoundingBox::new(a.0, a.1, a.2, z.0, z.1, z.2);
                if let Kind::Room { entrances } = &mut builder.pieces[index].kind {
                    entrances.push(entrance);
                }
            }
            cursor = door + 4;
        }
    }
}

fn grow(chunk: ChunkPos, wood: Wood, random: &mut dyn RandomSource) -> Builder {
    let west = (chunk.x << 4) + 2;
    let north = (chunk.z << 4) + 2;
    let max_x = west + 7 + random.next_int_bounded(6);
    let max_y = 54 + random.next_int_bounded(6);
    let max_z = north + 7 + random.next_int_bounded(6);
    let mut builder = Builder::new(MinePiece {
        frame: Frame::new(
            0,
            BoundingBox::new(west, START_Y, north, max_x, max_y, max_z),
            None,
        ),
        wood,
        kind: Kind::Room {
            entrances: Vec::new(),
        },
    });
    add_children(&mut builder, 0, random);
    builder
}

impl StructureKind for Mineshaft {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        ctx.random.next_double();
        let chunk = ctx.chunk;
        let start = BlockPos::new((chunk.x << 4) + 8, START_Y, chunk.z << 4);
        let mut builder = grow(chunk, self.wood, &mut ctx.random);
        let sea_level = ctx.generator.settings.sea_level;
        let dy = if self.wood.is_mesa() {
            let center = builder.bounding_box().center();
            let surface = ctx.base_height(center.x, center.z, HeightmapKind::WorldSurfaceWg);
            let target = if surface <= sea_level {
                sea_level
            } else {
                ctx.random.next_int_between_inclusive(sea_level, surface)
            };
            let dy = target - center.y;
            builder.offset_vertically(dy);
            dy
        } else {
            let min_y = ctx.generator.settings.min_y;
            builder.move_below_sea_level(sea_level, min_y, &mut ctx.random, 10)
        };
        let pieces: Vec<Box<dyn Piece>> = builder
            .pieces
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn Piece>)
            .collect();
        Some(GenerationStub {
            position: BlockPos::new(start.x, start.y + dy, start.z),
            pieces: Box::new(move |_| pieces),
        })
    }
}

fn cave_air() -> BlockState {
    Block::CaveAir.default_state()
}

impl Framed for MinePiece {
    fn frame(&self) -> &Frame {
        &self.frame
    }

    fn can_be_replaced(&self, level: &Region, x: i32, y: i32, z: i32, chunk: &BoundingBox) -> bool {
        let state = self.get_block(level, x, y, z, chunk);
        let block = state.block();
        !(block == self.wood.planks.block()
            || block == self.wood.log.block()
            || block == self.wood.fence.block()
            || block == Block::IronChain)
    }
}

impl MinePiece {
    fn is_in_invalid_location(&self, level: &mut Region, chunk: &BoundingBox) -> bool {
        let b = self.frame.bounding_box;
        let x0 = (b.min_x - 1).max(chunk.min_x);
        let y0 = (b.min_y - 1).max(chunk.min_y);
        let z0 = (b.min_z - 1).max(chunk.min_z);
        let x1 = (b.max_x + 1).min(chunk.max_x);
        let y1 = (b.max_y + 1).min(chunk.max_y);
        let z1 = (b.max_z + 1).min(chunk.max_z);
        let center = BlockPos::new((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2);
        let biome = level.biome(center);
        if crate::tags::biome_is(
            tuff_vanilla_data::tag::worldgen::biome::key::MINESHAFT_BLOCKING,
            biome,
        ) {
            return true;
        }
        let liquid = |level: &Region, x, y, z| level.block(BlockPos::new(x, y, z)).is_liquid();
        for x in x0..=x1 {
            for z in z0..=z1 {
                if liquid(level, x, y0, z) || liquid(level, x, y1, z) {
                    return true;
                }
            }
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                if liquid(level, x, y, z0) || liquid(level, x, y, z1) {
                    return true;
                }
            }
        }
        for z in z0..=z1 {
            for y in y0..=y1 {
                if liquid(level, x0, y, z) || liquid(level, x1, y, z) {
                    return true;
                }
            }
        }
        false
    }

    fn set_planks(&self, level: &mut Region, chunk: &BoundingBox, x: i32, y: i32, z: i32) {
        if !self.is_interior(level, x, y, z, chunk) {
            return;
        }
        let pos = self.frame.world_pos(x, y, z);
        if !level.block(pos).is_face_sturdy(Facing::Up, Support::Full) {
            level.set_block(pos, self.wood.planks);
        }
    }

    fn is_supporting_box(
        &self,
        level: &Region,
        chunk: &BoundingBox,
        x0: i32,
        x1: i32,
        y1: i32,
        z: i32,
    ) -> bool {
        (x0..=x1).all(|x| !is_air(self.get_block(level, x, y1 + 1, z, chunk)))
    }

    fn place_room(&self, level: &mut Region, chunk: &BoundingBox, entrances: &[BoundingBox]) {
        let b = self.frame.bounding_box;
        let air = cave_air();
        self.generate_box(
            level,
            chunk,
            (b.min_x, b.min_y + 1, b.min_z),
            (b.max_x, (b.min_y + 3).min(b.max_y), b.max_z),
            air,
            air,
            false,
        );
        for e in entrances {
            self.generate_box(
                level,
                chunk,
                (e.min_x, e.max_y - 2, e.min_z),
                (e.max_x, e.max_y, e.max_z),
                air,
                air,
                false,
            );
        }
        self.generate_upper_half_sphere(
            level,
            chunk,
            (b.min_x, b.min_y + 4, b.min_z),
            (b.max_x, b.max_y, b.max_z),
            air,
            false,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn place_corridor(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        has_rails: bool,
        spider: bool,
        placed_spider: &AtomicBool,
        sections: i32,
    ) {
        let length = sections * 5 - 1;
        let air = cave_air();
        self.generate_box(level, chunk, (0, 0, 0), (2, 1, length), air, air, false);
        self.generate_maybe_box(
            level,
            chunk,
            random,
            0.8,
            (0, 2, 0),
            (2, 2, length),
            air,
            air,
            false,
            false,
        );
        if spider {
            let cobweb = Block::Cobweb.default_state();
            self.generate_maybe_box(
                level,
                chunk,
                random,
                0.6,
                (0, 0, 0),
                (2, 1, length),
                cobweb,
                air,
                false,
                true,
            );
        }
        for section in 0..sections {
            let z = 2 + section * 5;
            self.place_support(level, chunk, z, (0, 2, 2), random);
            for (p, x, dz) in [
                (0.1, 0, -1),
                (0.1, 2, -1),
                (0.1, 0, 1),
                (0.1, 2, 1),
                (0.05, 0, -2),
                (0.05, 2, -2),
                (0.05, 0, 2),
                (0.05, 2, 2),
            ] {
                self.maybe_place_cobweb(level, chunk, random, p, (x, 2, z + dz));
            }
            if random.next_int_bounded(100) == 0 {
                self.create_minecart(level, chunk, random, 2, 0, z - 1);
            }
            if random.next_int_bounded(100) == 0 {
                self.create_minecart(level, chunk, random, 0, 0, z + 1);
            }
            if spider && !placed_spider.load(Ordering::Relaxed) {
                let new_z = z - 1 + random.next_int_bounded(3);
                let pos = self.frame.world_pos(1, 0, new_z);
                if chunk.contains(pos) && self.is_interior(level, 1, 0, new_z, chunk) {
                    placed_spider.store(true, Ordering::Relaxed);
                    level.set_block(pos, Block::Spawner.default_state());
                    level.set_block_entity(
                        pos,
                        Compound::new()
                            .with("id", "minecraft:mob_spawner")
                            .with(
                                "SpawnData",
                                Compound::new().with(
                                    "entity",
                                    Compound::new().with("id", "minecraft:cave_spider"),
                                ),
                            )
                            .with("SpawnPotentials", Tag::List(Vec::new())),
                    );
                }
            }
        }
        for x in 0..=2 {
            for z in 0..=length {
                self.set_planks(level, chunk, x, -1, z);
            }
        }
        self.place_double_support(level, chunk, 0, -1, 2);
        if sections > 1 {
            self.place_double_support(level, chunk, 0, -1, length - 2);
        }
        if has_rails {
            let rail = Block::Rail
                .default_state()
                .with("shape", "north_south")
                .expect("rails have a shape");
            for z in 0..=length {
                let floor = self.get_block(level, 1, -1, z, chunk);
                if !is_air(floor) && floor.is_solid_render() {
                    let p = if self.is_interior(level, 1, 0, z, chunk) {
                        0.7
                    } else {
                        0.9
                    };
                    self.maybe_generate_block(level, chunk, random, p, 1, 0, z, rail);
                }
            }
        }
    }

    /// Where the game puts a chest minecart: a rail (the cart itself is an
    /// entity, which generation does not keep; its loot seed is drawn).
    fn create_minecart(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        x: i32,
        y: i32,
        z: i32,
    ) {
        let pos = self.frame.world_pos(x, y, z);
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        if chunk.contains(pos) && is_air(level.block(pos)) && !is_air(level.block(below)) {
            let shape = if random.next_bool() {
                "north_south"
            } else {
                "east_west"
            };
            let rail = Block::Rail
                .default_state()
                .with("shape", shape)
                .expect("rails have a shape");
            self.place_block(level, rail, x, y, z, chunk);
            random.next_long();
        }
    }

    fn place_double_support(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        x: i32,
        y: i32,
        z: i32,
    ) {
        let planks = self.wood.planks.block();
        if self.get_block(level, x, y, z, chunk).block() == planks {
            self.pillar_down_or_chain_up(level, x, y, z, chunk);
        }
        if self.get_block(level, x + 2, y, z, chunk).block() == planks {
            self.pillar_down_or_chain_up(level, x + 2, y, z, chunk);
        }
    }

    fn pillar_down_or_chain_up(
        &self,
        level: &mut Region,
        x: i32,
        y: i32,
        z: i32,
        chunk: &BoundingBox,
    ) {
        let pos = self.frame.world_pos(x, y, z);
        if !chunk.contains(pos) {
            return;
        }
        let world_y = pos.y;
        let at = |y: i32| BlockPos::new(pos.x, y, pos.z);
        let mut distance = 1;
        let (mut below, mut above) = (true, true);
        while below || above {
            if below {
                let p = at(world_y - distance);
                let state = level.block(p);
                let empty = is_replaceable_by_structures(state) && state.block() != Block::Lava;
                if !empty && state.is_face_sturdy(Facing::Up, Support::Full) {
                    for y in world_y - distance + 1..world_y {
                        level.set_block(at(y), self.wood.log);
                    }
                    return;
                }
                below = distance <= 20 && empty && p.y > level.min_y() + 1;
            }
            if above {
                let p = at(world_y + distance);
                let state = level.block(p);
                let empty = is_replaceable_by_structures(state);
                if !empty
                    && state.is_face_sturdy(Facing::Down, Support::Center)
                    && !state.block().is_falling()
                {
                    level.set_block(at(world_y + 1), self.wood.fence);
                    for y in world_y + 2..world_y + distance {
                        level.set_block(at(y), Block::IronChain.default_state());
                    }
                    return;
                }
                above = distance <= 50 && empty && p.y < level.max_y();
            }
            distance += 1;
        }
    }

    /// A timber frame across the corridor at depth `z`: two posts from the
    /// floor up to `top`, then a beam (or, one time in four, only the two
    /// post caps with an occasional lantern torch hanging off the beam).
    fn place_support(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        z: i32,
        (left, right, top): (i32, i32, i32),
        random: &mut dyn RandomSource,
    ) {
        if !self.is_supporting_box(level, chunk, left, right, top, z) {
            return;
        }
        let air = cave_air();
        for (x, side) in [(left, "west"), (right, "east")] {
            let post = self.wood.fence.with(side, "true").expect("fence side");
            self.generate_box(level, chunk, (x, 0, z), (x, top - 1, z), post, air, false);
        }
        let planks = self.wood.planks;
        let caps_only = random.next_int_bounded(4) == 0;
        if caps_only {
            for x in [left, right] {
                self.generate_box(level, chunk, (x, top, z), (x, top, z), planks, air, false);
            }
            return;
        }
        self.generate_box(
            level,
            chunk,
            (left, top, z),
            (right, top, z),
            planks,
            air,
            false,
        );
        for (dz, facing) in [(-1, "south"), (1, "north")] {
            if random.next_float() < 0.05 {
                let torch = Block::WallTorch
                    .default_state()
                    .with("facing", facing)
                    .expect("torch facing");
                self.place_block(level, torch, left + 1, top, z + dz, chunk);
            }
        }
    }

    /// Drops a cobweb in open air touching at least two solid faces.
    fn maybe_place_cobweb(
        &self,
        level: &mut Region,
        chunk: &BoundingBox,
        random: &mut dyn RandomSource,
        chance: f32,
        (x, y, z): (i32, i32, i32),
    ) {
        if !self.is_interior(level, x, y, z, chunk) || random.next_float() >= chance {
            return;
        }
        let centre = self.frame.world_pos(x, y, z);
        let anchored = crate::feature::predicate::DIRECTIONS
            .into_iter()
            .filter(|side| {
                let (dx, dy, dz) = side.step();
                let at = BlockPos::new(centre.x + dx, centre.y + dy, centre.z + dz);
                chunk.contains(at)
                    && level
                        .block(at)
                        .is_face_sturdy(side.opposite(), Support::Full)
            })
            .nth(1)
            .is_some();
        if anchored {
            self.place_block(level, Block::Cobweb.default_state(), x, y, z, chunk);
        }
    }

    fn place_crossing(&self, level: &mut Region, chunk: &BoundingBox, two_floored: bool) {
        let b = self.frame.bounding_box;
        let air = cave_air();
        if two_floored {
            self.generate_box(
                level,
                chunk,
                (b.min_x + 1, b.min_y, b.min_z),
                (b.max_x - 1, b.min_y + 3 - 1, b.max_z),
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                chunk,
                (b.min_x, b.min_y, b.min_z + 1),
                (b.max_x, b.min_y + 3 - 1, b.max_z - 1),
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                chunk,
                (b.min_x + 1, b.max_y - 2, b.min_z),
                (b.max_x - 1, b.max_y, b.max_z),
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                chunk,
                (b.min_x, b.max_y - 2, b.min_z + 1),
                (b.max_x, b.max_y, b.max_z - 1),
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                chunk,
                (b.min_x + 1, b.min_y + 3, b.min_z + 1),
                (b.max_x - 1, b.min_y + 3, b.max_z - 1),
                air,
                air,
                false,
            );
        } else {
            self.generate_box(
                level,
                chunk,
                (b.min_x + 1, b.min_y, b.min_z),
                (b.max_x - 1, b.max_y, b.max_z),
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                chunk,
                (b.min_x, b.min_y, b.min_z + 1),
                (b.max_x, b.max_y, b.max_z - 1),
                air,
                air,
                false,
            );
        }
        for (x, z) in [
            (b.min_x + 1, b.min_z + 1),
            (b.min_x + 1, b.max_z - 1),
            (b.max_x - 1, b.min_z + 1),
            (b.max_x - 1, b.max_z - 1),
        ] {
            if !is_air(self.get_block(level, x, b.max_y + 1, z, chunk)) {
                self.generate_box(
                    level,
                    chunk,
                    (x, b.min_y, z),
                    (x, b.max_y, z),
                    self.wood.planks,
                    air,
                    false,
                );
            }
        }
        for x in b.min_x..=b.max_x {
            for z in b.min_z..=b.max_z {
                self.set_planks(level, chunk, x, b.min_y - 1, z);
            }
        }
    }

    fn place_stairs(&self, level: &mut Region, chunk: &BoundingBox) {
        let air = cave_air();
        self.generate_box(level, chunk, (0, 5, 0), (2, 7, 1), air, air, false);
        self.generate_box(level, chunk, (0, 0, 7), (2, 2, 8), air, air, false);
        for i in 0..5 {
            let low = 5 - i - if i < 4 { 1 } else { 0 };
            self.generate_box(
                level,
                chunk,
                (0, low, 2 + i),
                (2, 7 - i, 2 + i),
                air,
                air,
                false,
            );
        }
    }
}

impl Piece for MinePiece {
    fn kind(&self) -> PieceType {
        match self.kind {
            Kind::Room { .. } => PieceType::Msroom,
            Kind::Corridor { .. } => PieceType::Mscorridor,
            Kind::Crossing { .. } => PieceType::Mscrossing,
            Kind::Stairs => PieceType::Msstairs,
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
        if let Kind::Corridor { placed_spider, .. } = &self.kind
            && tag.get_bool("hps") == Some(true)
        {
            placed_spider.store(true, Ordering::Relaxed);
        }
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("MST", self.wood.index as i32);
        match &self.kind {
            Kind::Room { entrances } => {
                tag.insert(
                    "Entrances",
                    Tag::List(entrances.iter().map(BoundingBox::to_nbt).collect()),
                );
            }
            Kind::Corridor {
                has_rails,
                spider,
                placed_spider,
                sections,
            } => {
                tag.insert("hr", *has_rails);
                tag.insert("sc", *spider);
                tag.insert("hps", placed_spider.load(Ordering::Relaxed));
                tag.insert("Num", *sections);
            }
            Kind::Crossing {
                direction,
                two_floored,
            } => {
                tag.insert("tf", *two_floored);
                tag.insert("D", data_value_2d(*direction) as i8);
            }
            Kind::Stairs => {}
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
        if self.is_in_invalid_location(level, &chunk) {
            return;
        }
        match &self.kind {
            Kind::Room { entrances } => self.place_room(level, &chunk, entrances),
            Kind::Corridor {
                has_rails,
                spider,
                placed_spider,
                sections,
            } => self.place_corridor(
                level,
                &chunk,
                &mut *ctx.random,
                *has_rails,
                *spider,
                placed_spider,
                *sections,
            ),
            Kind::Crossing { two_floored, .. } => self.place_crossing(level, &chunk, *two_floored),
            Kind::Stairs => self.place_stairs(level, &chunk),
        }
    }
}

#[cfg(test)]
pub(crate) fn grown_for_test(chunk: ChunkPos, seed: i64, wood: &str) -> Vec<Box<dyn Piece>> {
    let mut random = tuff_item::random::LegacyRandom::new(seed);
    grow(chunk, Wood::by_name(wood).unwrap(), &mut random)
        .pieces
        .into_iter()
        .map(|p| Box::new(p) as Box<dyn Piece>)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::random::LegacyRandom;

    fn fingerprint(seed: i64) -> u64 {
        let mut random = LegacyRandom::new(seed);
        let chunk = ChunkPos::new((seed % 97) as i32 - 48, (seed % 89) as i32 - 44);
        let builder = grow(chunk, Wood::by_name("normal").unwrap(), &mut random);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: i64| {
            h ^= v as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        };
        for p in &builder.pieces {
            let b = p.frame.bounding_box;
            for v in [
                b.min_x,
                b.min_y,
                b.min_z,
                b.max_x,
                b.max_y,
                b.max_z,
                p.frame.gen_depth,
            ] {
                mix(v as i64);
            }
            mix(p.frame.orientation.map_or(-1, |f| f as i64));
            match &p.kind {
                Kind::Room { entrances } => {
                    mix(1);
                    for e in entrances {
                        for v in [e.min_x, e.min_y, e.min_z, e.max_x, e.max_y, e.max_z] {
                            mix(v as i64);
                        }
                    }
                }
                Kind::Corridor {
                    has_rails,
                    spider,
                    sections,
                    ..
                } => {
                    mix(2);
                    mix(*has_rails as i64);
                    mix(*spider as i64);
                    mix(*sections as i64);
                }
                Kind::Crossing {
                    direction,
                    two_floored,
                } => {
                    mix(3);
                    mix(*direction as i64);
                    mix(*two_floored as i64);
                }
                Kind::Stairs => mix(4),
            }
        }
        mix(random.next_long());
        h
    }

    #[tuff_core::watchdog]
    #[test]
    fn layouts_are_pinned_for_fixed_seeds() {
        let mut all: u64 = 0;
        for seed in 0..200 {
            all = all.rotate_left(7) ^ fingerprint(seed * 7919 + 13);
        }
        assert_eq!(all, 9843697560222855662);
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "timing run: cargo test --release -p tuff-feature-builtin-vanilla-worldgen mineshaft_layout_timing -- --ignored --nocapture"]
    fn mineshaft_layout_timing() {
        let start = std::time::Instant::now();
        let mut acc = 0u64;
        for seed in 0..20_000 {
            acc ^= fingerprint(seed);
        }
        eprintln!("mineshaft layout: {:?} ({acc})", start.elapsed());
    }
}
