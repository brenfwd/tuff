//! Woodland mansions: an 11x11 grid of corridors and rooms laid out at
//! random, then dressed with `woodland_mansion/` templates floor by floor,
//! and set on cobblestone down to the ground.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;

use crate::json::{Result, obj};
use crate::structure::geometry::{BoundingBox, Mirror, Rotation};
use crate::structure::piece::structure_loot;
use crate::structure::processor::{BlockIgnore, ProcessorList};
use crate::structure::ruined_portal::place_template_with_markers;
use crate::structure::template::{self, PlaceSettings, Template};
use crate::structure::{
    GenerationContext, GenerationStub, Piece, PieceContext, StructureKind, StructureStart,
};
use tuff_item::random::RandomSource;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

struct Mansion;

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    obj(config, &[], "woodland_mansion")?;
    Ok(Arc::new(Mansion))
}

const CLEAR: i32 = 0;
const CORRIDOR: i32 = 1;
const ROOM: i32 = 2;
const START_ROOM: i32 = 3;
const BLOCKED: i32 = 5;
const ROOM_1X1: i32 = 65536;
const ROOM_1X2: i32 = 131072;
const ROOM_2X2: i32 = 262144;
const ORIGIN_FLAG: i32 = 1048576;
const DOOR_FLAG: i32 = 2097152;
const STAIRS_FLAG: i32 = 4194304;
const CORRIDOR_FLAG: i32 = 8388608;
const TYPE_MASK: i32 = 983040;
const ID_MASK: i32 = 65535;
const ENTRANCE_X: i32 = 7;
const ENTRANCE_Y: i32 = 4;

fn from_2d(value: i32) -> Facing {
    [Facing::South, Facing::West, Facing::North, Facing::East][value as usize]
}

fn step(facing: Facing) -> (i32, i32) {
    let (x, _, z) = facing.step();
    (x, z)
}

#[derive(Clone)]
struct Grid {
    cells: Vec<i32>,
    width: i32,
    height: i32,
    outside: i32,
}

impl Grid {
    fn new(width: i32, height: i32, outside: i32) -> Self {
        Self {
            cells: vec![0; (width * height) as usize],
            width,
            height,
            outside,
        }
    }

    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && x < self.width && y >= 0 && y < self.height
    }

    fn get(&self, x: i32, y: i32) -> i32 {
        if self.inside(x, y) {
            self.cells[(x * self.height + y) as usize]
        } else {
            self.outside
        }
    }

    fn set(&mut self, x: i32, y: i32, value: i32) {
        if self.inside(x, y) {
            self.cells[(x * self.height + y) as usize] = value;
        }
    }

    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, value: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.set(x, y, value);
            }
        }
    }

    fn set_if(&mut self, x: i32, y: i32, if_value: i32, value: i32) {
        if self.get(x, y) == if_value {
            self.set(x, y, value);
        }
    }

    fn edges_to(&self, x: i32, y: i32, value: i32) -> bool {
        self.get(x - 1, y) == value
            || self.get(x + 1, y) == value
            || self.get(x, y + 1) == value
            || self.get(x, y - 1) == value
    }
}

fn is_house(grid: &Grid, x: i32, y: i32) -> bool {
    matches!(grid.get(x, y), 1..=4)
}

/// The mansion's layout: the ground and second floor share `base`, the
/// third floor has its own, and each floor has its rooms.
struct Layout {
    base: Grid,
    third: Grid,
    rooms: [Grid; 3],
}

impl Layout {
    fn new(random: &mut dyn RandomSource) -> Self {
        let (ex, ey) = (ENTRANCE_X, ENTRANCE_Y);
        let mut base = Grid::new(11, 11, BLOCKED);
        base.fill(ex, ey, ex + 1, ey + 1, START_ROOM);
        base.fill(ex - 1, ey, ex - 1, ey + 1, ROOM);
        base.fill(ex + 2, ey - 2, ex + 3, ey + 3, BLOCKED);
        base.fill(ex + 1, ey - 2, ex + 1, ey - 1, CORRIDOR);
        base.fill(ex + 1, ey + 2, ex + 1, ey + 3, CORRIDOR);
        base.set(ex - 1, ey - 1, CORRIDOR);
        base.set(ex - 1, ey + 2, CORRIDOR);
        base.fill(0, 0, 11, 1, BLOCKED);
        base.fill(0, 9, 11, 11, BLOCKED);
        corridor(&mut base, random, ex, ey - 2, Facing::West, 6);
        corridor(&mut base, random, ex, ey + 3, Facing::West, 6);
        corridor(&mut base, random, ex - 2, ey - 1, Facing::West, 3);
        corridor(&mut base, random, ex - 2, ey + 2, Facing::West, 3);
        while clean_edges(&mut base) {}
        let mut rooms = [
            Grid::new(11, 11, BLOCKED),
            Grid::new(11, 11, BLOCKED),
            Grid::new(11, 11, BLOCKED),
        ];
        identify_rooms(&base, &mut rooms[0], random);
        identify_rooms(&base, &mut rooms[1], random);
        rooms[0].fill(ex + 1, ey, ex + 1, ey + 1, CORRIDOR_FLAG);
        rooms[1].fill(ex + 1, ey, ex + 1, ey + 1, CORRIDOR_FLAG);
        let third = Grid::new(base.width, base.height, BLOCKED);
        let mut layout = Self { base, third, rooms };
        layout.setup_third_floor(random);
        let third = layout.third.clone();
        identify_rooms(&third, &mut layout.rooms[2], random);
        layout
    }

    fn is_room_id(&self, x: i32, y: i32, floor: usize, id: i32) -> bool {
        (self.rooms[floor].get(x, y) & ID_MASK) == id
    }

    fn room_direction(&self, x: i32, y: i32, floor: usize, id: i32) -> Option<Facing> {
        Facing::HORIZONTAL.into_iter().find(|d| {
            let (dx, dz) = step(*d);
            self.is_room_id(x + dx, y + dz, floor, id)
        })
    }

    fn setup_third_floor(&mut self, random: &mut dyn RandomSource) {
        let mut candidates = Vec::new();
        for y in 0..self.third.height {
            for x in 0..self.third.width {
                let data = self.rooms[1].get(x, y);
                if data & TYPE_MASK == ROOM_1X2 && data & DOOR_FLAG == DOOR_FLAG {
                    candidates.push((x, y));
                }
            }
        }
        let (width, height) = (self.third.width, self.third.height);
        if candidates.is_empty() {
            self.third.fill(0, 0, width, height, BLOCKED);
            return;
        }
        let (rx, ry) = candidates[random.next_int_bounded(candidates.len() as i32) as usize];
        let data = self.rooms[1].get(rx, ry);
        self.rooms[1].set(rx, ry, data | STAIRS_FLAG);
        let direction = self
            .room_direction(rx, ry, 1, data & ID_MASK)
            .expect("a 1x2 room has a second cell");
        let (dx, dz) = step(direction);
        let (end_x, end_y) = (rx + dx, ry + dz);
        for y in 0..height {
            for x in 0..width {
                if !is_house(&self.base, x, y) {
                    self.third.set(x, y, BLOCKED);
                } else if x == rx && y == ry {
                    self.third.set(x, y, START_ROOM);
                } else if x == end_x && y == end_y {
                    self.third.set(x, y, START_ROOM);
                    self.rooms[2].set(x, y, CORRIDOR_FLAG);
                }
            }
        }
        let corridors: Vec<Facing> = Facing::HORIZONTAL
            .into_iter()
            .filter(|d| {
                let (dx, dz) = step(*d);
                self.third.get(end_x + dx, end_y + dz) == CLEAR
            })
            .collect();
        if corridors.is_empty() {
            self.third.fill(0, 0, width, height, BLOCKED);
            self.rooms[1].set(rx, ry, data);
        } else {
            let direction = corridors[random.next_int_bounded(corridors.len() as i32) as usize];
            let (dx, dz) = step(direction);
            corridor(
                &mut self.third,
                random,
                end_x + dx,
                end_y + dz,
                direction,
                4,
            );
            while clean_edges(&mut self.third) {}
        }
    }
}

fn corridor(
    grid: &mut Grid,
    random: &mut dyn RandomSource,
    x: i32,
    y: i32,
    heading: Facing,
    depth: i32,
) {
    if depth <= 0 {
        return;
    }
    let (hx, hz) = step(heading);
    grid.set(x, y, CORRIDOR);
    grid.set_if(x + hx, y + hz, CLEAR, CORRIDOR);
    for _ in 0..8 {
        let next = from_2d(random.next_int_bounded(4));
        if next == heading.opposite() || (next == Facing::East && random.next_bool()) {
            continue;
        }
        let (nx, ny) = (x + hx, y + hz);
        let (sx, sz) = step(next);
        if grid.get(nx + sx, ny + sz) == CLEAR && grid.get(nx + sx * 2, ny + sz * 2) == CLEAR {
            corridor(grid, random, nx + sx, ny + sz, next, depth - 1);
            break;
        }
    }
    let (cx, cz) = step(heading.clockwise());
    let (ax, az) = step(heading.counter_clockwise());
    grid.set_if(x + cx, y + cz, CLEAR, ROOM);
    grid.set_if(x + ax, y + az, CLEAR, ROOM);
    grid.set_if(x + hx + cx, y + hz + cz, CLEAR, ROOM);
    grid.set_if(x + hx + ax, y + hz + az, CLEAR, ROOM);
    grid.set_if(x + hx * 2, y + hz * 2, CLEAR, ROOM);
    grid.set_if(x + cx * 2, y + cz * 2, CLEAR, ROOM);
    grid.set_if(x + ax * 2, y + az * 2, CLEAR, ROOM);
}

fn clean_edges(grid: &mut Grid) -> bool {
    const SIDES: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    const CORNERS: [(i32, i32); 4] = [(1, 1), (-1, 1), (1, -1), (-1, -1)];
    let mut filled_any = false;
    for row in 0..grid.height {
        for col in 0..grid.width {
            if grid.get(col, row) != CLEAR {
                continue;
            }
            let walls = |offsets: &[(i32, i32); 4]| {
                offsets
                    .iter()
                    .filter(|(dx, dy)| is_house(grid, col + dx, row + dy))
                    .count()
            };
            let enclosed = match walls(&SIDES) {
                0 | 1 => false,
                2 => walls(&CORNERS) < 2,
                _ => true,
            };
            if enclosed {
                grid.set(col, row, ROOM);
                filled_any = true;
            }
        }
    }
    filled_any
}

/// Footprints tried for a room growing from a seed cell, best first: the
/// three cells (besides the seed) it must claim and the room kind.
const FOOTPRINTS: [(&[(i32, i32)], i32); 8] = [
    (&[(1, 0), (0, 1), (1, 1)], ROOM_2X2),
    (&[(-1, 0), (0, 1), (-1, 1)], ROOM_2X2),
    (&[(-1, 0), (0, -1), (-1, -1)], ROOM_2X2),
    (&[(1, 0)], ROOM_1X2),
    (&[(0, 1)], ROOM_1X2),
    (&[(-1, 0)], ROOM_1X2),
    (&[(0, -1)], ROOM_1X2),
    (&[], ROOM_1X1),
];

/// Corner flips tried, in order, to find a door cell next to a corridor.
const DOOR_FLIPS: [(bool, bool); 4] = [(false, false), (true, true), (true, false), (false, true)];

fn identify_rooms(plan: &Grid, rooms: &mut Grid, random: &mut dyn RandomSource) {
    let mut seeds = Vec::with_capacity(plan.cells.len());
    for row in 0..plan.height {
        for col in 0..plan.width {
            if plan.get(col, row) == ROOM {
                seeds.push((col, row));
            }
        }
    }
    for len in (2..=seeds.len()).rev() {
        let pick = random.next_int_bounded(len as i32) as usize;
        seeds.swap(len - 1, pick);
    }
    let mut next_id = 10;
    for (col, row) in seeds {
        if rooms.get(col, row) != 0 {
            continue;
        }
        let claimable = |&(dx, dy): &(i32, i32)| {
            rooms.get(col + dx, row + dy) == 0 && plan.get(col + dx, row + dy) == ROOM
        };
        let (cells, kind) = FOOTPRINTS
            .iter()
            .find(|(cells, _)| cells.iter().all(claimable))
            .copied()
            .unwrap_or((&[], ROOM_1X1));
        let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (col, col, row, row);
        for &(dx, dy) in cells {
            lo_x = lo_x.min(col + dx);
            hi_x = hi_x.max(col + dx);
            lo_y = lo_y.min(row + dy);
            hi_y = hi_y.max(row + dy);
        }
        let prefer_lo_x = random.next_bool();
        let prefer_lo_y = random.next_bool();
        let door = DOOR_FLIPS.iter().find_map(|&(flip_x, flip_y)| {
            let dx = if prefer_lo_x != flip_x { lo_x } else { hi_x };
            let dy = if prefer_lo_y != flip_y { lo_y } else { hi_y };
            plan.edges_to(dx, dy, CORRIDOR).then_some((dx, dy))
        });
        let (marker, marked) = match door {
            Some(cell) => (ORIGIN_FLAG | DOOR_FLAG, cell),
            None => (ORIGIN_FLAG, (lo_x, lo_y)),
        };
        let base = kind | next_id;
        for y in lo_y..=hi_y {
            for x in lo_x..=hi_x {
                let extra = if (x, y) == marked { marker } else { 0 };
                rooms.set(x, y, base | extra);
            }
        }
        next_id += 1;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Floor {
    First,
    Second,
    Third,
}

impl Floor {
    fn one_by_one(self, random: &mut dyn RandomSource) -> String {
        match self {
            Floor::First => format!("1x1_a{}", random.next_int_bounded(5) + 1),
            _ => format!("1x1_b{}", random.next_int_bounded(5) + 1),
        }
    }

    fn one_by_one_secret(self, random: &mut dyn RandomSource) -> String {
        format!("1x1_as{}", random.next_int_bounded(4) + 1)
    }

    fn side_entrance(self, random: &mut dyn RandomSource, stairs: bool) -> String {
        match self {
            Floor::First => format!("1x2_a{}", random.next_int_bounded(9) + 1),
            _ if stairs => "1x2_c_stairs".into(),
            _ => format!("1x2_c{}", random.next_int_bounded(4) + 1),
        }
    }

    fn front_entrance(self, random: &mut dyn RandomSource, stairs: bool) -> String {
        match self {
            Floor::First => format!("1x2_b{}", random.next_int_bounded(5) + 1),
            _ if stairs => "1x2_d_stairs".into(),
            _ => format!("1x2_d{}", random.next_int_bounded(5) + 1),
        }
    }

    fn one_by_two_secret(self, random: &mut dyn RandomSource) -> String {
        match self {
            Floor::First => format!("1x2_s{}", random.next_int_bounded(2) + 1),
            _ => format!("1x2_se{}", random.next_int_bounded(1) + 1),
        }
    }

    fn two_by_two(self, random: &mut dyn RandomSource) -> String {
        match self {
            Floor::First => format!("2x2_a{}", random.next_int_bounded(4) + 1),
            _ => format!("2x2_b{}", random.next_int_bounded(5) + 1),
        }
    }
}

fn relative(pos: BlockPos, facing: Facing, distance: i32) -> BlockPos {
    let (dx, dy, dz) = facing.step();
    BlockPos::new(
        pos.x + dx * distance,
        pos.y + dy * distance,
        pos.z + dz * distance,
    )
}

fn above(pos: BlockPos, distance: i32) -> BlockPos {
    BlockPos::new(pos.x, pos.y + distance, pos.z)
}

/// A wall walker's position, heading and wall template.
struct Walker {
    rotation: Rotation,
    position: BlockPos,
    wall: &'static str,
}

struct Placer<'r> {
    random: &'r mut dyn RandomSource,
    pieces: Vec<MansionPiece>,
    start_x: i32,
    start_y: i32,
}

impl Placer<'_> {
    fn add(&mut self, name: &str, position: BlockPos, rotation: Rotation) {
        self.add_mirrored(name, position, rotation, Mirror::None);
    }

    fn add_mirrored(&mut self, name: &str, position: BlockPos, rotation: Rotation, mirror: Mirror) {
        self.pieces
            .push(MansionPiece::new(name, position, rotation, mirror));
    }

    fn east(&self, rotation: Rotation) -> Facing {
        rotation.rotate_facing(Facing::East)
    }

    fn south(&self, rotation: Rotation) -> Facing {
        rotation.rotate_facing(Facing::South)
    }

    /// Where grid cell `(x, y)` begins on a floor starting at `origin`.
    fn cell(&self, origin: BlockPos, rotation: Rotation, x: i32, y: i32) -> BlockPos {
        let south = relative(origin, self.south(rotation), 8 + (y - self.start_y) * 8);
        relative(south, self.east(rotation), (x - self.start_x) * 8)
    }

    fn wall_piece(&mut self, walker: &mut Walker) {
        let r = walker.rotation;
        let at = relative(walker.position, self.east(r), 7);
        self.add(walker.wall, at, r);
        walker.position = relative(walker.position, self.south(r), 8);
    }

    fn turn(&mut self, walker: &mut Walker) {
        let r = walker.rotation;
        walker.position = relative(walker.position, self.south(r), -1);
        self.add("wall_corner", walker.position, r);
        walker.position = relative(walker.position, self.south(r), -7);
        walker.position = relative(walker.position, r.rotate_facing(Facing::West), -6);
        walker.rotation = r.then(Rotation::Clockwise90);
    }

    fn inner_turn(&mut self, walker: &mut Walker) {
        let r = walker.rotation;
        walker.position = relative(walker.position, self.south(r), 6);
        walker.position = relative(walker.position, self.east(r), 8);
        walker.rotation = r.then(Rotation::CounterClockwise90);
    }

    #[allow(clippy::too_many_arguments)]
    fn outer_walls(
        &mut self,
        walker: &mut Walker,
        grid: &Grid,
        mut direction: Facing,
        start_x: i32,
        start_y: i32,
        end_x: i32,
        end_y: i32,
    ) {
        let (mut x, mut y) = (start_x, start_y);
        let start = direction;
        loop {
            let (dx, dz) = step(direction);
            let (ax, az) = step(direction.counter_clockwise());
            if !is_house(grid, x + dx, y + dz) {
                self.turn(walker);
                direction = direction.clockwise();
                if x != end_x || y != end_y || start != direction {
                    self.wall_piece(walker);
                }
            } else if is_house(grid, x + dx + ax, y + dz + az) {
                self.inner_turn(walker);
                x += dx;
                y += dz;
                direction = direction.counter_clockwise();
            } else {
                x += dx;
                y += dz;
                if x != end_x || y != end_y || start != direction {
                    self.wall_piece(walker);
                }
            }
            if x == end_x && y == end_y && start == direction {
                return;
            }
        }
    }

    fn create(&mut self, origin: BlockPos, rotation: Rotation, layout: &mut Layout) {
        let mut walker = Walker {
            rotation,
            position: origin,
            wall: "wall_flat",
        };
        let west = rotation.rotate_facing(Facing::West);
        self.add("entrance", relative(walker.position, west, 9), rotation);
        walker.position = relative(walker.position, self.south(rotation), 16);
        let mut second = Walker {
            rotation,
            position: above(walker.position, 8),
            wall: "wall_window",
        };
        self.start_x = ENTRANCE_X + 1;
        self.start_y = ENTRANCE_Y + 1;
        let (end_x, end_y) = (ENTRANCE_X + 1, ENTRANCE_Y);
        let base = layout.base.clone();
        let third = layout.third.clone();
        let (sx, sy) = (self.start_x, self.start_y);
        self.outer_walls(&mut walker, &base, Facing::South, sx, sy, end_x, end_y);
        self.outer_walls(&mut second, &base, Facing::South, sx, sy, end_x, end_y);
        let mut top = Walker {
            rotation,
            position: above(walker.position, 19),
            wall: "wall_window",
        };
        'find: for y in 0..third.height {
            for x in (0..third.width).rev() {
                if is_house(&third, x, y) {
                    top.position = relative(top.position, self.south(rotation), 8 + (y - sy) * 8);
                    top.position = relative(top.position, self.east(rotation), (x - sx) * 8);
                    self.wall_piece(&mut top);
                    self.outer_walls(&mut top, &third, Facing::South, x, y, x, y);
                    break 'find;
                }
            }
        }
        self.roof(above(origin, 16), rotation, &base, Some(&third));
        self.roof(above(origin, 27), rotation, &third, None);
        for (index, floor) in [Floor::First, Floor::Second, Floor::Third]
            .into_iter()
            .enumerate()
        {
            let floor_origin = above(origin, 8 * index as i32 + if index == 2 { 3 } else { 0 });
            let grid = if index == 2 { &third } else { &base };
            self.corridors(layout, index, floor_origin, rotation, grid);
            self.rooms(layout, index, floor, floor_origin, rotation, grid);
        }
    }

    fn corridors(
        &mut self,
        layout: &Layout,
        index: usize,
        origin: BlockPos,
        rotation: Rotation,
        grid: &Grid,
    ) {
        let rooms = &layout.rooms[index];
        let (south_piece, west_piece) = if index == 0 {
            ("carpet_south_1", "carpet_west_1")
        } else {
            ("carpet_south_2", "carpet_west_2")
        };
        let (east, south) = (self.east(rotation), self.south(rotation));
        let (west, north) = (east.opposite(), south.opposite());
        let open = |x: i32, y: i32| {
            grid.get(x, y) == CORRIDOR || rooms.get(x, y) & CORRIDOR_FLAG == CORRIDOR_FLAG
        };
        for y in 0..grid.height {
            for x in 0..grid.width {
                if grid.get(x, y) != CORRIDOR {
                    continue;
                }
                let at = self.cell(origin, rotation, x, y);
                self.add("corridor_floor", at, rotation);
                if open(x, y - 1) {
                    self.add("carpet_north", above(relative(at, east, 1), 1), rotation);
                }
                if open(x + 1, y) {
                    let p = relative(relative(at, south, 1), east, 5);
                    self.add("carpet_east", above(p, 1), rotation);
                }
                if open(x, y + 1) {
                    self.add(
                        south_piece,
                        relative(relative(at, south, 5), west, 1),
                        rotation,
                    );
                }
                if open(x - 1, y) {
                    self.add(
                        west_piece,
                        relative(relative(at, west, 1), north, 1),
                        rotation,
                    );
                }
            }
        }
    }

    fn rooms(
        &mut self,
        layout: &Layout,
        index: usize,
        floor: Floor,
        origin: BlockPos,
        rotation: Rotation,
        grid: &Grid,
    ) {
        let rooms = &layout.rooms[index];
        let (wall, door) = if index == 0 {
            ("indoors_wall_1", "indoors_door_1")
        } else {
            ("indoors_wall_2", "indoors_door_2")
        };
        let (east, south) = (self.east(rotation), self.south(rotation));
        let north = south.opposite();
        for y in 0..grid.height {
            for x in 0..grid.width {
                let third_start = index == 2 && grid.get(x, y) == START_ROOM;
                if grid.get(x, y) != ROOM && !third_start {
                    continue;
                }
                let data = rooms.get(x, y);
                let kind = data & TYPE_MASK;
                let id = data & ID_MASK;
                let third_start = third_start && data & CORRIDOR_FLAG == CORRIDOR_FLAG;
                let mut doors = Vec::new();
                if data & DOOR_FLAG == DOOR_FLAG {
                    for d in Facing::HORIZONTAL {
                        let (dx, dz) = step(d);
                        if grid.get(x + dx, y + dz) == CORRIDOR {
                            doors.push(d);
                        }
                    }
                }
                let door_dir = if !doors.is_empty() {
                    Some(doors[self.random.next_int_bounded(doors.len() as i32) as usize])
                } else if data & ORIGIN_FLAG == ORIGIN_FLAG {
                    Some(Facing::Up)
                } else {
                    None
                };
                let room = relative(self.cell(origin, rotation, x, y), east, -1);
                let pick = |dir: Facing| if door_dir == Some(dir) { door } else { wall };
                if is_house(grid, x - 1, y) && !layout.is_room_id(x - 1, y, index, id) {
                    self.add(pick(Facing::West), room, rotation);
                }
                if grid.get(x + 1, y) == CORRIDOR && !third_start {
                    self.add(pick(Facing::East), relative(room, east, 8), rotation);
                }
                if is_house(grid, x, y + 1) && !layout.is_room_id(x, y + 1, index, id) {
                    let p = relative(relative(room, south, 7), east, 7);
                    self.add(pick(Facing::South), p, rotation.then(Rotation::Clockwise90));
                }
                if grid.get(x, y - 1) == CORRIDOR && !third_start {
                    let p = relative(relative(room, north, 1), east, 7);
                    self.add(pick(Facing::North), p, rotation.then(Rotation::Clockwise90));
                }
                if kind == ROOM_1X1 {
                    self.room_1x1(room, rotation, door_dir, floor);
                } else if kind == ROOM_1X2
                    && let Some(door_dir) = door_dir
                {
                    let room_dir = layout.room_direction(x, y, index, id);
                    let stairs = data & STAIRS_FLAG == STAIRS_FLAG;
                    self.room_1x2(room, rotation, room_dir, door_dir, floor, stairs);
                } else if kind == ROOM_2X2
                    && let Some(door_dir) = door_dir
                    && door_dir != Facing::Up
                {
                    let mut room_dir = door_dir.clockwise();
                    let (dx, dz) = step(room_dir);
                    if !layout.is_room_id(x + dx, y + dz, index, id) {
                        room_dir = room_dir.opposite();
                    }
                    self.room_2x2(room, rotation, room_dir, door_dir, floor);
                } else if kind == ROOM_2X2 && door_dir == Some(Facing::Up) {
                    let name = floor.two_by_two_secret();
                    self.add(name, relative(room, east, 1), rotation);
                }
            }
        }
    }

    fn room_1x1(&mut self, room: BlockPos, rotation: Rotation, door: Option<Facing>, floor: Floor) {
        let mut piece_rotation = Rotation::None;
        let mut name = floor.one_by_one(self.random);
        match door {
            Some(Facing::East) => {}
            Some(Facing::North) => piece_rotation = Rotation::CounterClockwise90,
            Some(Facing::West) => piece_rotation = Rotation::Clockwise180,
            Some(Facing::South) => piece_rotation = Rotation::Clockwise90,
            _ => name = floor.one_by_one_secret(self.random),
        }
        let zero = zero_position(BlockPos::new(1, 0, 0), piece_rotation, 7, 7);
        let turned = rotate_pos(zero, rotation);
        let at = BlockPos::new(room.x + turned.x, room.y, room.z + turned.z);
        self.add(&name, at, piece_rotation.then(rotation));
    }

    #[allow(clippy::too_many_arguments)]
    fn room_1x2(
        &mut self,
        room: BlockPos,
        rotation: Rotation,
        room_dir: Option<Facing>,
        door: Facing,
        floor: Floor,
        stairs: bool,
    ) {
        use Facing::*;
        let (east, south) = (self.east(rotation), self.south(rotation));
        let (west, north) = (east.opposite(), south.opposite());
        let r = rotation;
        let side = |p: &mut Self| floor.side_entrance(p.random, stairs);
        let front = |p: &mut Self| floor.front_entrance(p.random, stairs);
        match (door, room_dir) {
            (East, Some(South)) => {
                let name = side(self);
                self.add(&name, relative(room, east, 1), r);
            }
            (East, Some(North)) => {
                let name = side(self);
                let at = relative(relative(room, east, 1), south, 6);
                self.add_mirrored(&name, at, r, Mirror::LeftRight);
            }
            (West, Some(North)) => {
                let name = side(self);
                let at = relative(relative(room, east, 7), south, 6);
                self.add(&name, at, r.then(Rotation::Clockwise180));
            }
            (West, Some(South)) => {
                let name = side(self);
                self.add_mirrored(&name, relative(room, east, 7), r, Mirror::FrontBack);
            }
            (South, Some(East)) => {
                let name = side(self);
                let at = relative(room, east, 1);
                self.add_mirrored(&name, at, r.then(Rotation::Clockwise90), Mirror::LeftRight);
            }
            (South, Some(West)) => {
                let name = side(self);
                self.add(
                    &name,
                    relative(room, east, 7),
                    r.then(Rotation::Clockwise90),
                );
            }
            (North, Some(West)) => {
                let name = side(self);
                let at = relative(relative(room, east, 7), south, 6);
                self.add_mirrored(&name, at, r.then(Rotation::Clockwise90), Mirror::FrontBack);
            }
            (North, Some(East)) => {
                let name = side(self);
                let at = relative(relative(room, east, 1), south, 6);
                self.add(&name, at, r.then(Rotation::CounterClockwise90));
            }
            (South, Some(North)) => {
                let name = front(self);
                let at = relative(relative(room, east, 1), north, 8);
                self.add(&name, at, r);
            }
            (North, Some(South)) => {
                let name = front(self);
                let at = relative(relative(room, east, 7), south, 14);
                self.add(&name, at, r.then(Rotation::Clockwise180));
            }
            (West, Some(East)) => {
                let name = front(self);
                self.add(
                    &name,
                    relative(room, east, 15),
                    r.then(Rotation::Clockwise90),
                );
            }
            (East, Some(West)) => {
                let name = front(self);
                let at = relative(relative(room, west, 7), south, 6);
                self.add(&name, at, r.then(Rotation::CounterClockwise90));
            }
            (Up, Some(East)) => {
                let name = floor.one_by_two_secret(self.random);
                self.add(
                    &name,
                    relative(room, east, 15),
                    r.then(Rotation::Clockwise90),
                );
            }
            (Up, Some(South)) => {
                let name = floor.one_by_two_secret(self.random);
                self.add(&name, relative(room, east, 1), r);
            }
            _ => {}
        }
    }

    fn room_2x2(
        &mut self,
        room: BlockPos,
        rotation: Rotation,
        room_dir: Facing,
        door: Facing,
        floor: Floor,
    ) {
        use Facing::*;
        let (mut east, mut south) = (0, 0);
        let mut turned = rotation;
        let mut mirror = Mirror::None;
        match (door, room_dir) {
            (East, South) => east = -7,
            (East, North) => {
                east = -7;
                south = 6;
                mirror = Mirror::LeftRight;
            }
            (North, East) => {
                east = 1;
                south = 14;
                turned = rotation.then(Rotation::CounterClockwise90);
            }
            (North, West) => {
                east = 7;
                south = 14;
                turned = rotation.then(Rotation::CounterClockwise90);
                mirror = Mirror::LeftRight;
            }
            (South, West) => {
                east = 7;
                south = -8;
                turned = rotation.then(Rotation::Clockwise90);
            }
            (South, East) => {
                east = 1;
                south = -8;
                turned = rotation.then(Rotation::Clockwise90);
                mirror = Mirror::LeftRight;
            }
            (West, North) => {
                east = 15;
                south = 6;
                turned = rotation.then(Rotation::Clockwise180);
            }
            (West, South) => {
                east = 15;
                mirror = Mirror::FrontBack;
            }
            _ => {}
        }
        let at = relative(room, self.east(rotation), east);
        let at = relative(at, self.south(rotation), south);
        let name = floor.two_by_two(self.random);
        self.add_mirrored(&name, at, turned, mirror);
    }

    fn roof(
        &mut self,
        origin: BlockPos,
        rotation: Rotation,
        grid: &Grid,
        above_grid: Option<&Grid>,
    ) {
        let (east, south) = (self.east(rotation), self.south(rotation));
        let (west, north) = (east.opposite(), south.opposite());
        let covered = |x: i32, y: i32| above_grid.is_some_and(|g| is_house(g, x, y));
        for y in 0..grid.height {
            for x in 0..grid.width {
                let at = self.cell(origin, rotation, x, y);
                if !is_house(grid, x, y) || covered(x, y) {
                    continue;
                }
                self.add("roof", above(at, 3), rotation);
                if !is_house(grid, x + 1, y) {
                    self.add("roof_front", relative(at, east, 6), rotation);
                }
                if !is_house(grid, x - 1, y) {
                    let p = relative(relative(at, east, 0), south, 7);
                    self.add("roof_front", p, rotation.then(Rotation::Clockwise180));
                }
                if !is_house(grid, x, y - 1) {
                    self.add(
                        "roof_front",
                        relative(at, west, 1),
                        rotation.then(Rotation::CounterClockwise90),
                    );
                }
                if !is_house(grid, x, y + 1) {
                    let p = relative(relative(at, east, 6), south, 6);
                    self.add("roof_front", p, rotation.then(Rotation::Clockwise90));
                }
            }
        }
        if above_grid.is_some() {
            for y in 0..grid.height {
                for x in 0..grid.width {
                    let at = self.cell(origin, rotation, x, y);
                    if !is_house(grid, x, y) || !covered(x, y) {
                        continue;
                    }
                    let (ex, wx) = (!is_house(grid, x + 1, y), !is_house(grid, x - 1, y));
                    let (ny, sy) = (!is_house(grid, x, y - 1), !is_house(grid, x, y + 1));
                    if ex {
                        self.add("small_wall", relative(at, east, 7), rotation);
                    }
                    if wx {
                        let p = relative(relative(at, west, 1), south, 6);
                        self.add("small_wall", p, rotation.then(Rotation::Clockwise180));
                    }
                    if ny {
                        let p = relative(relative(at, west, 0), north, 1);
                        self.add("small_wall", p, rotation.then(Rotation::CounterClockwise90));
                    }
                    if sy {
                        let p = relative(relative(at, east, 6), south, 7);
                        self.add("small_wall", p, rotation.then(Rotation::Clockwise90));
                    }
                    if ex {
                        if ny {
                            let p = relative(relative(at, east, 7), north, 2);
                            self.add("small_wall_corner", p, rotation);
                        }
                        if sy {
                            let p = relative(relative(at, east, 8), south, 7);
                            self.add("small_wall_corner", p, rotation.then(Rotation::Clockwise90));
                        }
                    }
                    if wx {
                        if ny {
                            let p = relative(relative(at, west, 2), north, 1);
                            self.add(
                                "small_wall_corner",
                                p,
                                rotation.then(Rotation::CounterClockwise90),
                            );
                        }
                        if sy {
                            let p = relative(relative(at, west, 1), south, 8);
                            self.add(
                                "small_wall_corner",
                                p,
                                rotation.then(Rotation::Clockwise180),
                            );
                        }
                    }
                }
            }
        }
        for y in 0..grid.height {
            for x in 0..grid.width {
                let at = self.cell(origin, rotation, x, y);
                if !is_house(grid, x, y) || covered(x, y) {
                    continue;
                }
                if !is_house(grid, x + 1, y) {
                    let p2 = relative(at, east, 6);
                    if !is_house(grid, x, y + 1) {
                        self.add("roof_corner", relative(p2, south, 6), rotation);
                    } else if is_house(grid, x + 1, y + 1) {
                        self.add("roof_inner_corner", relative(p2, south, 5), rotation);
                    }
                    if !is_house(grid, x, y - 1) {
                        self.add(
                            "roof_corner",
                            p2,
                            rotation.then(Rotation::CounterClockwise90),
                        );
                    } else if is_house(grid, x + 1, y - 1) {
                        let p5 = relative(at, east, 9);
                        self.add(
                            "roof_inner_corner",
                            relative(p5, north, 2),
                            rotation.then(Rotation::Clockwise90),
                        );
                    }
                }
                if !is_house(grid, x - 1, y) {
                    let p6 = relative(relative(at, east, 0), south, 0);
                    if !is_house(grid, x, y + 1) {
                        self.add(
                            "roof_corner",
                            relative(p6, south, 6),
                            rotation.then(Rotation::Clockwise90),
                        );
                    } else if is_house(grid, x - 1, y + 1) {
                        let p8 = relative(p6, south, 8);
                        self.add(
                            "roof_inner_corner",
                            relative(p8, west, 3),
                            rotation.then(Rotation::CounterClockwise90),
                        );
                    }
                    if !is_house(grid, x, y - 1) {
                        self.add("roof_corner", p6, rotation.then(Rotation::Clockwise180));
                    } else if is_house(grid, x - 1, y - 1) {
                        self.add(
                            "roof_inner_corner",
                            relative(p6, south, 1),
                            rotation.then(Rotation::Clockwise180),
                        );
                    }
                }
            }
        }
    }
}

impl Floor {
    fn two_by_two_secret(self) -> &'static str {
        "2x2_s1"
    }
}

/// Where a template's origin lands so that its footprint of `size_x` by
/// `size_z` keeps its corner at `zero` after `rotation`.
fn zero_position(zero: BlockPos, rotation: Rotation, size_x: i32, size_z: i32) -> BlockPos {
    let (sx, sz) = (size_x - 1, size_z - 1);
    let (dx, dz) = match rotation {
        Rotation::CounterClockwise90 => (0, sx),
        Rotation::Clockwise90 => (sz, 0),
        Rotation::Clockwise180 => (sx, sz),
        Rotation::None => (0, 0),
    };
    BlockPos::new(zero.x + dx, zero.y, zero.z + dz)
}

fn rotate_pos(pos: BlockPos, rotation: Rotation) -> BlockPos {
    match rotation {
        Rotation::Clockwise90 => BlockPos::new(-pos.z, pos.y, pos.x),
        Rotation::Clockwise180 => BlockPos::new(-pos.x, pos.y, -pos.z),
        Rotation::CounterClockwise90 => BlockPos::new(pos.z, pos.y, -pos.x),
        Rotation::None => pos,
    }
}

struct MansionPiece {
    name: String,
    template: Arc<Template>,
    settings: PlaceSettings,
    position: BlockPos,
    bounding_box: BoundingBox,
}

impl MansionPiece {
    fn new(name: &str, position: BlockPos, rotation: Rotation, mirror: Mirror) -> Self {
        let template = template::get(&format!("minecraft:woodland_mansion/{name}"));
        let mut processors = ProcessorList::default();
        processors.push(BlockIgnore::structure_block());
        let settings = PlaceSettings {
            rotation,
            mirror,
            processors,
            ignore_entities: true,
            ..PlaceSettings::default()
        };
        let bounding_box = template.settings_box(&settings, position);
        Self {
            name: name.to_string(),
            template,
            settings,
            position,
            bounding_box,
        }
    }
}

impl Piece for MansionPiece {
    fn kind(&self) -> PieceType {
        PieceType::Wmp
    }

    fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        Some(Facing::North)
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("TPX", self.position.x);
        tag.insert("TPY", self.position.y);
        tag.insert("TPZ", self.position.z);
        tag.insert("Template", self.name.as_str());
        tag.insert("Rot", self.settings.rotation.name());
        tag.insert("Mi", self.settings.mirror.name());
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        reference: BlockPos,
    ) {
        let chunk = *chunk_box;
        let rotation = self.settings.rotation;
        place_template_with_markers(
            ctx,
            &self.template,
            self.position,
            reference,
            &self.settings,
            chunk,
            |ctx, marker, pos| {
                if marker.starts_with("Chest") {
                    let facing = match marker {
                        "ChestWest" => Some(Facing::West),
                        "ChestEast" => Some(Facing::East),
                        "ChestSouth" => Some(Facing::South),
                        "ChestNorth" => Some(Facing::North),
                        _ => None,
                    };
                    let chest = Block::Chest.default_state();
                    let chest = match facing {
                        Some(f) => {
                            let name =
                                crate::structure::processor::facing_name(rotation.rotate_facing(f));
                            chest.with("facing", name).unwrap_or(chest)
                        }
                        None => chest,
                    };
                    if chunk.contains(pos) && ctx.level.block(pos).block() != Block::Chest {
                        ctx.level.set_block(pos, chest);
                        ctx.level.set_block_entity(
                            pos,
                            Compound::new()
                                .with("id", "minecraft:chest")
                                .with("LootTable", structure_loot("woodland_mansion"))
                                .with("LootTableSeed", ctx.random.next_long()),
                        );
                    }
                    return;
                }
                let (id, count) = match marker {
                    "Mage" => ("minecraft:evoker", 1),
                    "Warrior" => ("minecraft:vindicator", 1),
                    "Group of Allays" => (
                        "minecraft:allay",
                        ctx.level.random().next_int_bounded(3) + 1,
                    ),
                    _ => return,
                };
                for _ in 0..count {
                    ctx.level
                        .add_entity(pos, crate::structure::structure_mob(id, pos, true));
                }
                ctx.level.set_block(pos, Block::Air.default_state());
            },
        );
    }
}

impl StructureKind for Mansion {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let chunk = ctx.chunk;
        let (x, z) = ((chunk.x << 4) + 7, (chunk.z << 4) + 7);
        if !ctx.could_exist_in_terrain_column(x, z) {
            return None;
        }
        let rotation = Rotation::ALL[ctx.random.next_int_bounded(4) as usize];
        let (size_x, size_z) = match rotation {
            Rotation::Clockwise90 => (-5, 5),
            Rotation::Clockwise180 => (-5, -5),
            Rotation::CounterClockwise90 => (5, -5),
            Rotation::None => (5, 5),
        };
        let y = ctx.lowest_y(x, z, size_x, size_z);
        if y < 60 {
            return None;
        }
        let start = BlockPos::new(x, y, z);
        Some(GenerationStub {
            position: start,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let random = &mut ctx.random;
                let mut layout = Layout::new(random);
                let mut placer = Placer {
                    random,
                    pieces: Vec::new(),
                    start_x: 0,
                    start_y: 0,
                };
                placer.create(start, rotation, &mut layout);
                placer
                    .pieces
                    .into_iter()
                    .map(|p| Box::new(p) as Box<dyn Piece>)
                    .collect()
            }),
        })
    }

    fn after_place(&self, start: &StructureStart, ctx: &mut PieceContext, chunk_box: &BoundingBox) {
        let boxes: Vec<BoundingBox> = start.pieces.iter().map(|p| p.bounding_box()).collect();
        let Some(bounds) = crate::structure::geometry::encapsulating_boxes(&boxes) else {
            return;
        };
        let min_y = ctx.level.min_y();
        let y_start = bounds.min_y;
        for x in chunk_box.min_x..=chunk_box.max_x {
            for z in chunk_box.min_z..=chunk_box.max_z {
                let pos = BlockPos::new(x, y_start, z);
                if is_air(ctx.level.block(pos))
                    || !bounds.contains(pos)
                    || !boxes.iter().any(|b| b.contains(pos))
                {
                    continue;
                }
                let mut y = y_start - 1;
                while y > min_y {
                    let below = BlockPos::new(x, y, z);
                    let state = ctx.level.block(below);
                    if !is_air(state) && !state.is_liquid() {
                        break;
                    }
                    ctx.level
                        .set_block(below, Block::Cobblestone.default_state());
                    y -= 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use tuff_item::random::LegacyRandom;

    fn fingerprint(seeds: std::ops::Range<i64>) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for seed in seeds {
            let layout = Layout::new(&mut LegacyRandom::new(seed));
            let grids = [
                &layout.base,
                &layout.third,
                &layout.rooms[0],
                &layout.rooms[1],
                &layout.rooms[2],
            ];
            for grid in grids {
                for &cell in &grid.cells {
                    hash = (hash ^ cell as u32 as u64).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn layouts_for_fixed_seeds_are_pinned() {
        assert_eq!(fingerprint(0..256), 9_875_044_382_747_095_999);
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore]
    fn layout_timing() {
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for round in 0..20 {
            sink ^= fingerprint(round * 1000..round * 1000 + 1000);
        }
        eprintln!("layout_timing {:?} {sink}", start.elapsed());
    }
}
