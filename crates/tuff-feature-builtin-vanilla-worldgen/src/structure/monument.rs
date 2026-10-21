//! Ocean monuments: one building piece (the only piece a start saves) that
//! lays out its rooms on a 5x5x3 grid when it is built, fits single and
//! double rooms to that grid, and adds two wings and a penthouse, each
//! guarded by an elder guardian.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::level::Region;
use crate::json::{Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::BoundingBox;
use crate::structure::piece::{Frame, Framed};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const BIOME_RANGE: i32 = 29;

struct Monument;

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "ocean_monument")?;
    Ok(Arc::new(Monument))
}

impl StructureKind for Monument {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let chunk = ctx.chunk;
        let center = ((chunk.x << 4) + 9, ctx.sea_level(), (chunk.z << 4) + 9);
        let surrounding = |biome: u16| {
            crate::tags::biome_is(
                tuff_vanilla_data::tag::worldgen::biome::key::REQUIRED_OCEAN_MONUMENT_SURROUNDING,
                biome,
            )
        };
        if !ctx.all_biomes_within(center, BIOME_RANGE, surrounding) {
            return None;
        }
        let position = ctx.on_top_of_chunk_center(HeightmapKind::OceanFloorWg)?;
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let direction = Facing::HORIZONTAL[ctx.random.next_int_bounded(4) as usize];
                vec![Box::new(Building::new(
                    &mut ctx.random,
                    (chunk.x << 4) - 29,
                    (chunk.z << 4) - 29,
                    direction,
                )) as Box<dyn Piece>]
            }),
        })
    }
}

const DOWN: usize = 0;
const UP: usize = 1;
const NORTH: usize = 2;
const SOUTH: usize = 3;
const WEST: usize = 4;
const EAST: usize = 5;
const OPPOSITE: [usize; 6] = [UP, DOWN, SOUTH, NORTH, EAST, WEST];
const STEPS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

const GRID_SIZE: i32 = 75;
const LEFT_WING: i32 = 1001;
const RIGHT_WING: i32 = 1002;
const PENTHOUSE: i32 = 1003;

fn room_index(x: i32, y: i32, z: i32) -> i32 {
    y * 25 + z * 5 + x
}

/// A cell of the room grid (or one of the special rooms around it). Along
/// z the grid is linked the other way round: `NORTH` is the next row.
#[derive(Debug, Clone)]
struct Room {
    index: i32,
    connections: [Option<usize>; 6],
    openings: [bool; 6],
    claimed: bool,
    source: bool,
}

impl Room {
    fn new(index: i32) -> Self {
        Self {
            index,
            connections: [None; 6],
            openings: [false; 6],
            claimed: false,
            source: false,
        }
    }

    fn opening_count(&self) -> usize {
        self.openings.iter().filter(|o| **o).count()
    }
}

#[derive(Debug, Default)]
struct Graph {
    rooms: Vec<Room>,
}

impl Graph {
    fn add(&mut self, index: i32) -> usize {
        self.rooms.push(Room::new(index));
        self.rooms.len() - 1
    }

    fn connect(&mut self, a: usize, direction: usize, b: usize) {
        self.rooms[a].connections[direction] = Some(b);
        self.rooms[b].connections[OPPOSITE[direction]] = Some(a);
    }

    fn update_openings(&mut self, room: usize) {
        let r = &mut self.rooms[room];
        for i in 0..6 {
            r.openings[i] = r.connections[i].is_some();
        }
    }

    fn link(&self, room: usize, direction: usize) -> usize {
        self.rooms[room].connections[direction].expect("monument room link")
    }

    fn opening(&self, room: usize, direction: usize) -> bool {
        self.rooms[room].openings[direction]
    }

    fn has_link(&self, room: usize, direction: usize) -> bool {
        self.rooms[room].connections[direction].is_some()
    }

    /// Whether the entry room is reachable from `room` through openings.
    fn reaches_source(&self, room: usize) -> bool {
        let mut seen = vec![false; self.rooms.len()];
        let mut stack = vec![room];
        seen[room] = true;
        while let Some(r) = stack.pop() {
            let current = &self.rooms[r];
            if current.source {
                return true;
            }
            for i in 0..6 {
                if let Some(next) = current.connections[i]
                    && current.openings[i]
                    && !seen[next]
                {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        false
    }

    /// Builds the grid, picks the core room and runs the connection
    /// checks; returns the rooms in fitting order, the entry room and the
    /// core room.
    fn generate(random: &mut WorldgenRandom) -> (Self, Vec<usize>, usize, usize) {
        let mut graph = Graph::default();
        let mut grid: [Option<usize>; GRID_SIZE as usize] = [None; GRID_SIZE as usize];
        let layers: [(i32, std::ops::Range<i32>, std::ops::Range<i32>); 3] =
            [(0, 0..5, 0..4), (1, 0..5, 0..4), (2, 1..4, 0..2)];
        for (y, xs, zs) in layers {
            for x in xs {
                for z in zs.clone() {
                    let index = room_index(x, y, z);
                    grid[index as usize] = Some(graph.add(index));
                }
            }
        }
        let source = grid[room_index(2, 0, 0) as usize].unwrap();
        for x in 0..5 {
            for z in 0..5 {
                for y in 0..3 {
                    let Some(room) = grid[room_index(x, y, z) as usize] else {
                        continue;
                    };
                    for (direction, (dx, dy, dz)) in STEPS.iter().enumerate() {
                        let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                        if !(0..5).contains(&nx) || !(0..5).contains(&nz) || !(0..3).contains(&ny) {
                            continue;
                        }
                        let Some(neighbour) = grid[room_index(nx, ny, nz) as usize] else {
                            continue;
                        };
                        let link = if nz == z {
                            direction
                        } else {
                            OPPOSITE[direction]
                        };
                        graph.connect(room, link, neighbour);
                    }
                }
            }
        }
        let roof = graph.add(PENTHOUSE);
        let left = graph.add(LEFT_WING);
        let right = graph.add(RIGHT_WING);
        graph.connect(grid[room_index(2, 2, 0) as usize].unwrap(), UP, roof);
        graph.connect(grid[room_index(0, 1, 0) as usize].unwrap(), SOUTH, left);
        graph.connect(grid[room_index(4, 1, 0) as usize].unwrap(), SOUTH, right);
        for special in [roof, left, right] {
            graph.rooms[special].claimed = true;
        }
        graph.rooms[source].source = true;
        let core = grid[room_index(random.next_int_bounded(4), 0, 2) as usize].unwrap();
        let east = graph.link(core, EAST);
        let north = graph.link(core, NORTH);
        let east_north = graph.link(east, NORTH);
        for room in [
            core,
            east,
            north,
            east_north,
            graph.link(core, UP),
            graph.link(east, UP),
            graph.link(north, UP),
            graph.link(east_north, UP),
        ] {
            graph.rooms[room].claimed = true;
        }
        let mut order = Vec::new();
        for room in grid.iter().flatten() {
            graph.update_openings(*room);
            order.push(*room);
        }
        graph.update_openings(roof);
        for i in (2..=order.len()).rev() {
            let j = random.next_int_bounded(i as i32) as usize;
            order.swap(i - 1, j);
        }
        for &room in &order {
            let mut closed = 0;
            let mut attempts = 0;
            while closed < 2 && attempts < 5 {
                attempts += 1;
                let face = random.next_int_bounded(6) as usize;
                if !graph.rooms[room].openings[face] {
                    continue;
                }
                let other = graph.link(room, face);
                let back = OPPOSITE[face];
                graph.rooms[room].openings[face] = false;
                graph.rooms[other].openings[back] = false;
                if graph.reaches_source(room) && graph.reaches_source(other) {
                    closed += 1;
                } else {
                    graph.rooms[room].openings[face] = true;
                    graph.rooms[other].openings[back] = true;
                }
            }
        }
        order.extend([roof, left, right]);
        (graph, order, source, core)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Entry,
    Simple { design: i32 },
    SimpleTop,
    DoubleY,
    DoubleX,
    DoubleZ,
    DoubleXY,
    DoubleYZ,
    Core,
    Wing { design: i32 },
    Penthouse,
}

/// A room of the building, placed with the building.
struct Child {
    kind: Kind,
    room: Option<usize>,
    frame: Frame,
}

impl Child {
    fn on_grid(kind: Kind, direction: Facing, graph: &Graph, room: usize) -> Self {
        let (width, height, depth) = match kind {
            Kind::DoubleY => (1, 2, 1),
            Kind::DoubleX => (2, 1, 1),
            Kind::DoubleZ => (1, 1, 2),
            Kind::DoubleXY => (2, 2, 1),
            Kind::DoubleYZ => (1, 2, 2),
            Kind::Core => (2, 2, 2),
            _ => (1, 1, 1),
        };
        let index = graph.rooms[room].index;
        let (rx, rz, ry) = (index % 5, index / 5 % 5, index / 25);
        let (w, h, d) = (width * 8, height * 4, depth * 8);
        let b = match direction {
            Facing::North | Facing::South => BoundingBox::new(0, 0, 0, w - 1, h - 1, d - 1),
            _ => BoundingBox::new(0, 0, 0, d - 1, h - 1, w - 1),
        };
        let b = match direction {
            Facing::North => b.moved(rx * 8, ry * 4, -(rz + depth) * 8 + 1),
            Facing::South => b.moved(rx * 8, ry * 4, rz * 8),
            Facing::West => b.moved(-(rz + depth) * 8 + 1, ry * 4, rx * 8),
            _ => b.moved(rz * 8, ry * 4, rx * 8),
        };
        Self {
            kind,
            room: Some(room),
            frame: Frame::new(1, b, Some(direction)),
        }
    }

    fn fixed(kind: Kind, direction: Facing, bounding_box: BoundingBox) -> Self {
        Self {
            kind,
            room: None,
            frame: Frame::new(1, bounding_box, Some(direction)),
        }
    }
}

/// The monument's one saved piece.
struct Building {
    frame: Frame,
    graph: Graph,
    children: Vec<Child>,
}

/// Picks the room kind for an unclaimed grid room and claims what it uses.
fn fit(graph: &mut Graph, room: usize, random: &mut WorldgenRandom) -> (Kind, usize) {
    let free = |g: &Graph, r: usize, d: usize| g.opening(r, d) && !g.rooms[g.link(r, d)].claimed;
    let claim = |g: &mut Graph, rooms: &[usize]| {
        for r in rooms {
            g.rooms[*r].claimed = true;
        }
    };
    if free(graph, room, EAST) && free(graph, room, UP) && free(graph, graph.link(room, EAST), UP) {
        let east = graph.link(room, EAST);
        let rooms = [room, east, graph.link(room, UP), graph.link(east, UP)];
        claim(graph, &rooms);
        return (Kind::DoubleXY, room);
    }
    if free(graph, room, NORTH) && free(graph, room, UP) && free(graph, graph.link(room, NORTH), UP)
    {
        let north = graph.link(room, NORTH);
        let rooms = [room, north, graph.link(room, UP), graph.link(north, UP)];
        claim(graph, &rooms);
        return (Kind::DoubleYZ, room);
    }
    if free(graph, room, NORTH) {
        claim(graph, &[room, graph.link(room, NORTH)]);
        return (Kind::DoubleZ, room);
    }
    if free(graph, room, EAST) {
        claim(graph, &[room, graph.link(room, EAST)]);
        return (Kind::DoubleX, room);
    }
    if free(graph, room, UP) {
        claim(graph, &[room, graph.link(room, UP)]);
        return (Kind::DoubleY, room);
    }
    let o = &graph.rooms[room].openings;
    if !(o[WEST] || o[EAST] || o[NORTH] || o[SOUTH] || o[UP]) {
        claim(graph, &[room]);
        return (Kind::SimpleTop, room);
    }
    claim(graph, &[room]);
    (
        Kind::Simple {
            design: random.next_int_bounded(3),
        },
        room,
    )
}

impl Building {
    fn new(random: &mut WorldgenRandom, west: i32, north: i32, direction: Facing) -> Self {
        let frame = Frame::new(
            0,
            BoundingBox::new(west, 39, north, west + 57, 39 + 22, north + 57),
            Some(direction),
        );
        let (mut graph, order, source, core) = Graph::generate(random);
        graph.rooms[source].claimed = true;
        let mut children = vec![
            Child::on_grid(Kind::Entry, direction, &graph, source),
            Child::on_grid(Kind::Core, direction, &graph, core),
        ];
        for room in order {
            if graph.rooms[room].claimed || graph.rooms[room].index >= GRID_SIZE {
                continue;
            }
            let (kind, room) = fit(&mut graph, room, random);
            children.push(Child::on_grid(kind, direction, &graph, room));
        }
        let offset = frame.world_pos(9, 0, 22);
        for child in &mut children {
            child.frame.bounding_box = child.frame.bounding_box.moved(offset.x, offset.y, offset.z);
        }
        let corners = |a: (i32, i32, i32), b: (i32, i32, i32)| {
            BoundingBox::from_corners(
                frame.world_pos(a.0, a.1, a.2),
                frame.world_pos(b.0, b.1, b.2),
            )
        };
        let left = corners((1, 1, 1), (23, 8, 21));
        let right = corners((34, 1, 1), (56, 8, 21));
        let penthouse = corners((22, 13, 22), (35, 17, 35));
        let wing = random.next_int();
        children.push(Child::fixed(
            Kind::Wing { design: wing & 1 },
            direction,
            left,
        ));
        children.push(Child::fixed(
            Kind::Wing {
                design: wing.wrapping_add(1) & 1,
            },
            direction,
            right,
        ));
        children.push(Child::fixed(Kind::Penthouse, direction, penthouse));
        Self {
            frame,
            graph,
            children,
        }
    }
}

const GRAY: BlockState = Block::Prismarine.default_state();
const LIGHT: BlockState = Block::PrismarineBricks.default_state();
const BLACK: BlockState = Block::DarkPrismarine.default_state();
const LAMP: BlockState = Block::SeaLantern.default_state();
const WATER: BlockState = Block::Water.default_state();

struct Framing(Frame);

impl Framed for Framing {
    fn frame(&self) -> &Frame {
        &self.0
    }
}

/// Draws one piece into the chunk being decorated.
struct Draw<'a, 'g> {
    piece: Framing,
    level: &'a mut Region<'g>,
    chunk: BoundingBox,
}

impl Draw<'_, '_> {
    fn solid(&mut self, [x0, y0, z0, x1, y1, z1]: ops::Cuboid, state: BlockState) {
        self.piece.generate_box(
            self.level,
            &self.chunk,
            (x0, y0, z0),
            (x1, y1, z1),
            state,
            state,
            false,
        );
    }

    fn put(&mut self, state: BlockState, x: i32, y: i32, z: i32) {
        self.piece
            .place_block(self.level, state, x, y, z, &self.chunk);
    }

    fn cells(b: ops::Cuboid) -> impl Iterator<Item = (i32, i32, i32)> {
        (b[1]..=b[4]).flat_map(move |y| {
            (b[0]..=b[3]).flat_map(move |x| (b[2]..=b[5]).map(move |z| (x, y, z)))
        })
    }

    /// Water below sea level and air above, leaving ice and water alone.
    fn flood(&mut self, b: ops::Cuboid) {
        let sea_level = self.level.sea_level();
        for (x, y, z) in Self::cells(b) {
            let here = self.piece.get_block(self.level, x, y, z, &self.chunk);
            if matches!(
                here.block(),
                Block::Ice | Block::PackedIce | Block::BlueIce | Block::Water
            ) {
                continue;
            }
            let dry = self.piece.0.world_y(y) >= sea_level && here != WATER;
            let state = if dry {
                Block::Air.default_state()
            } else {
                WATER
            };
            self.put(state, x, y, z);
        }
    }

    /// Swaps plain water in the box for `state`.
    fn cap(&mut self, b: ops::Cuboid, state: BlockState) {
        for (x, y, z) in Self::cells(b) {
            if self.piece.get_block(self.level, x, y, z, &self.chunk) == WATER {
                self.put(state, x, y, z);
            }
        }
    }

    /// An 8x8 floor; over a downward opening it leaves a light-rimmed hole.
    fn floor(&mut self, x: i32, z: i32, hole: bool) {
        let slab = |x0: i32, z0: i32, x1: i32, z1: i32| [x + x0, 0, z + z0, x + x1, 0, z + z1];
        if !hole {
            self.solid(slab(0, 0, 7, 7), GRAY);
            return;
        }
        for (b, state) in [
            (slab(0, 0, 2, 7), GRAY),
            (slab(5, 0, 7, 7), GRAY),
            (slab(3, 0, 4, 2), GRAY),
            (slab(3, 5, 4, 7), GRAY),
            (slab(3, 2, 4, 2), LIGHT),
            (slab(3, 5, 4, 5), LIGHT),
            (slab(2, 3, 2, 4), LIGHT),
            (slab(5, 3, 5, 4), LIGHT),
        ] {
            self.solid(b, state);
        }
    }

    /// Whether the chunk meets the piece-local column rectangle.
    fn meets(&self, x0: i32, z0: i32, x1: i32, z1: i32) -> bool {
        let f = &self.piece.0;
        let (ax, az) = (f.world_x(x0, z0), f.world_z(x0, z0));
        let (bx, bz) = (f.world_x(x1, z1), f.world_z(x1, z1));
        self.chunk
            .intersects_xz(ax.min(bx), az.min(bz), ax.max(bx), az.max(bz))
    }

    /// The elder guardian of a wing or the penthouse, left in the chunk's
    /// entities for the mob that loads it.
    fn elder(&mut self, x: i32, y: i32, z: i32) {
        let pos = self.piece.0.world_pos(x, y, z);
        if self.chunk.contains(pos) {
            self.level.add_entity(pos, elder_guardian(pos));
        }
    }
}

fn elder_guardian(pos: BlockPos) -> Compound {
    let at = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
    Compound::new()
        .with("id", "minecraft:elder_guardian")
        .with(
            "Pos",
            Tag::List(at.iter().map(|v| Tag::Double(*v)).collect()),
        )
        .with("Motion", Tag::List(vec![Tag::Double(0.0); 3]))
        .with(
            "Rotation",
            Tag::List(vec![Tag::Float(0.0), Tag::Float(0.0)]),
        )
        .with(super::template::FINALIZE_SPAWN, true)
}

impl Piece for Building {
    fn kind(&self) -> PieceType {
        PieceType::Omb
    }

    fn bounding_box(&self) -> BoundingBox {
        self.frame.bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        self.frame.orientation
    }

    fn save_extra(&self, _tag: &mut Compound) {}

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        _reference: BlockPos,
    ) {
        let chunk = *chunk_box;
        {
            let mut d = Draw {
                piece: Framing(self.frame),
                level: ctx.level,
                chunk,
            };
            shell::draw_shell(&mut d, self.frame.bounding_box.min_y);
        }
        for child in &self.children {
            if child.frame.bounding_box.intersects(&chunk) {
                let mut d = Draw {
                    piece: Framing(child.frame),
                    level: ctx.level,
                    chunk,
                };
                room(&mut d, &self.graph, child, ctx.random);
            }
        }
    }
}

fn room(d: &mut Draw, g: &Graph, child: &Child, random: &mut WorldgenRandom) {
    match child.kind {
        Kind::Entry => entry_room(d, g, child.room.unwrap()),
        Kind::Simple { design } => simple_room(d, g, child.room.unwrap(), design, random),
        Kind::SimpleTop => simple_top_room(d, g, child.room.unwrap(), random),
        Kind::DoubleY => double_y_room(d, g, child.room.unwrap()),
        Kind::DoubleX => double_x_room(d, g, child.room.unwrap()),
        Kind::DoubleZ => double_z_room(d, g, child.room.unwrap()),
        Kind::DoubleXY => double_xy_room(d, g, child.room.unwrap()),
        Kind::DoubleYZ => double_yz_room(d, g, child.room.unwrap()),
        Kind::Core => core_room(d),
        Kind::Wing { design } => wing_room(d, design),
        Kind::Penthouse => penthouse(d),
    }
}

fn above_grid(g: &Graph, room: usize) -> bool {
    g.rooms[room].index / 25 > 0
}

mod ops;
mod rooms;
mod shell;
use rooms::*;

#[cfg(test)]
#[path = "monument_tests.rs"]
mod tests;
