use tuff_wire::{BufferError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// The block a point is in.
    pub fn containing(position: [f64; 3]) -> Self {
        Self::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        )
    }

    pub fn offset(self, face: Face) -> Self {
        let (dx, dy, dz) = face.normal();
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }

    pub fn center(self) -> [f64; 3] {
        [
            self.x as f64 + 0.5,
            self.y as f64 + 0.5,
            self.z as f64 + 0.5,
        ]
    }

    pub fn pack(self) -> i64 {
        ((self.x as i64 & 0x3FF_FFFF) << 38)
            | ((self.z as i64 & 0x3FF_FFFF) << 12)
            | (self.y as i64 & 0xFFF)
    }

    pub fn unpack(value: i64) -> Self {
        Self {
            x: (value >> 38) as i32,
            y: (value << 52 >> 52) as i32,
            z: (value << 26 >> 38) as i32,
        }
    }

    pub fn dist_manhattan(self, other: Self) -> i32 {
        (self.x - other.x).abs() + (self.y - other.y).abs() + (self.z - other.z).abs()
    }

    pub fn dist_chessboard(self, other: Self) -> i32 {
        (self.x - other.x)
            .abs()
            .max((self.y - other.y).abs())
            .max((self.z - other.z).abs())
    }

    pub fn dist_sqr(self, other: Self) -> f64 {
        let (dx, dy, dz) = (
            f64::from(self.x) - f64::from(other.x),
            f64::from(self.y) - f64::from(other.y),
            f64::from(self.z) - f64::from(other.z),
        );
        dx * dx + dy * dy + dz * dz
    }

    pub fn closer_than(self, other: Self, distance: f64) -> bool {
        self.dist_sqr(other) < distance * distance
    }

    /// Every block of the box spanned by the two corners (either order),
    /// x fastest, then y, then z.
    pub fn between_closed(a: Self, b: Self) -> impl Iterator<Item = Self> {
        let (min_x, min_y, min_z) = (a.x.min(b.x), a.y.min(b.y), a.z.min(b.z));
        let width = i64::from(a.x.max(b.x)) - i64::from(min_x) + 1;
        let height = i64::from(a.y.max(b.y)) - i64::from(min_y) + 1;
        let depth = i64::from(a.z.max(b.z)) - i64::from(min_z) + 1;
        (0..width * height * depth).map(move |index| {
            let slice = index / width;
            Self::new(
                min_x + (index % width) as i32,
                min_y + (slice % height) as i32,
                min_z + (slice / height) as i32,
            )
        })
    }

    /// `limit` blocks picked at random from the cube of half-size `size`
    /// around `center`; `next_int(bound)` is a uniform draw below `bound`.
    pub fn random_in_cube(
        mut next_int: impl FnMut(i32) -> i32,
        limit: usize,
        center: Self,
        size: i32,
    ) -> impl Iterator<Item = Self> {
        let width = size * 2 + 1;
        (0..limit).map(move |_| {
            Self::new(
                center.x - size + next_int(width),
                center.y - size + next_int(width),
                center.z - size + next_int(width),
            )
        })
    }

    /// Blocks within `reach` Manhattan distance of `origin`, nearest first.
    pub fn within_manhattan(origin: Self, reach: i32) -> impl Iterator<Item = Self> {
        Self::manhattan_ordered(origin, [reach; 3], reach)
    }

    /// [`Self::within_manhattan`] with a reach per axis, clipped to the
    /// largest of them.
    pub fn within_clipped_manhattan(origin: Self, reach: [i32; 3]) -> impl Iterator<Item = Self> {
        Self::manhattan_ordered(origin, reach, reach[0].max(reach[1]).max(reach[2]))
    }

    /// The box of the given per-axis reach, ordered by Manhattan distance.
    pub fn within_box_by_manhattan(origin: Self, reach: [i32; 3]) -> impl Iterator<Item = Self> {
        Self::manhattan_ordered(origin, reach, reach[0] + reach[1] + reach[2])
    }

    fn manhattan_ordered(
        origin: Self,
        reach: [i32; 3],
        max_depth: i32,
    ) -> impl Iterator<Item = Self> {
        let [reach_x, reach_y, reach_z] = reach;
        let mut depth = 0;
        let (mut max_x, mut max_y, mut x, mut y) = (0, 0, 0, 0);
        let mut mirror: Option<Self> = None;
        std::iter::from_fn(move || {
            if let Some(pos) = mirror.take() {
                return Some(pos);
            }
            loop {
                if y > max_y {
                    x += 1;
                    if x > max_x {
                        depth += 1;
                        if depth > max_depth {
                            return None;
                        }
                        max_x = reach_x.min(depth);
                        x = -max_x;
                    }
                    max_y = reach_y.min(depth - x.abs());
                    y = -max_y;
                }
                let z = depth - x.abs() - y.abs();
                let found =
                    (z <= reach_z).then(|| Self::new(origin.x + x, origin.y + y, origin.z + z));
                y += 1;
                if let Some(pos) = found {
                    if z != 0 {
                        mirror = Some(Self::new(pos.x, pos.y, origin.z - z));
                    }
                    return Some(pos);
                }
            }
        })
    }

    /// A square spiral of blocks around `center` out to `radius`, starting
    /// at the center, moving first toward `first` and turning toward
    /// `second`. The two must lie on different axes.
    pub fn spiral_around(
        center: Self,
        radius: i32,
        first: Face,
        second: Face,
    ) -> impl Iterator<Item = Self> {
        assert!(
            first.axis() != second.axis(),
            "the two directions cannot be on the same axis"
        );
        let directions = [first, second, first.opposite(), second.opposite()];
        let legs = 4 * radius;
        let mut cursor = center.offset(second);
        let (mut leg, mut leg_size, mut leg_index) = (-1_i32, 0, 0);
        std::iter::from_fn(move || {
            cursor = cursor.offset(directions[((leg + 4) % 4) as usize]);
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

    /// Visits blocks breadth first from `start`. `neighbours` feeds the
    /// next blocks of one into the callback; `visit` decides whether a block
    /// counts, is skipped, or stops the walk. Returns how many counted;
    /// stops at `max_count` and does not expand past `max_depth`.
    pub fn breadth_first_traversal(
        start: Self,
        max_depth: i32,
        max_count: usize,
        mut neighbours: impl FnMut(Self, &mut dyn FnMut(Self)),
        mut visit: impl FnMut(Self) -> Traversal,
    ) -> usize {
        let mut queue = std::collections::VecDeque::from([(start, 0)]);
        let mut seen = std::collections::HashSet::new();
        let mut count = 0;
        while let Some((pos, depth)) = queue.pop_front() {
            if !seen.insert(pos) {
                continue;
            }
            match visit(pos) {
                Traversal::Skip => continue,
                Traversal::Stop => break,
                Traversal::Accept => {}
            }
            count += 1;
            if count >= max_count {
                return count;
            }
            if depth < max_depth {
                neighbours(pos, &mut |next| queue.push_back((next, depth + 1)));
            }
        }
        count
    }
}

/// What [`BlockPos::breadth_first_traversal`] does with a visited block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Traversal {
    Accept,
    Skip,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Face {
    pub fn from_id(id: i32) -> Result<Self> {
        Ok(match id {
            0 => Self::Down,
            1 => Self::Up,
            2 => Self::North,
            3 => Self::South,
            4 => Self::West,
            5 => Self::East,
            other => return Err(BufferError::InvalidEnum(other)),
        })
    }

    pub fn id(self) -> i32 {
        self as i32
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }

    /// 0 for x, 1 for y, 2 for z.
    pub fn axis(self) -> u8 {
        match self {
            Self::West | Self::East => 0,
            Self::Down | Self::Up => 1,
            Self::North | Self::South => 2,
        }
    }

    pub fn normal(self) -> (i32, i32, i32) {
        match self {
            Self::Down => (0, -1, 0),
            Self::Up => (0, 1, 0),
            Self::North => (0, 0, -1),
            Self::South => (0, 0, 1),
            Self::West => (-1, 0, 0),
            Self::East => (1, 0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[tuff_core::watchdog]
    #[test]
    fn distances_match_the_game() {
        let (a, b) = (BlockPos::new(1, 2, 3), BlockPos::new(-2, 6, 3));
        assert_eq!(a.dist_manhattan(b), 7);
        assert_eq!(a.dist_chessboard(b), 4);
        assert_eq!(a.dist_sqr(b), 25.0);
        assert!(a.closer_than(b, 5.1));
        assert!(!a.closer_than(b, 5.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn between_closed_runs_x_then_y_then_z_from_either_corner() {
        let cells: Vec<_> =
            BlockPos::between_closed(BlockPos::new(1, 1, 1), BlockPos::new(0, 0, 0)).collect();
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[0], BlockPos::new(0, 0, 0));
        assert_eq!(cells[1], BlockPos::new(1, 0, 0));
        assert_eq!(cells[2], BlockPos::new(0, 1, 0));
        assert_eq!(cells[4], BlockPos::new(0, 0, 1));
        assert_eq!(cells[7], BlockPos::new(1, 1, 1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn within_manhattan_is_each_block_once_nearest_first() {
        let origin = BlockPos::new(10, 20, 30);
        let found: Vec<_> = BlockPos::within_manhattan(origin, 3).collect();
        let unique: HashSet<_> = found.iter().copied().collect();
        assert_eq!(unique.len(), found.len());
        assert_eq!(found[0], origin);
        assert_eq!(found.len(), 63);
        assert!(found.iter().all(|pos| pos.dist_manhattan(origin) <= 3));
        let distances: Vec<_> = found.iter().map(|pos| pos.dist_manhattan(origin)).collect();
        assert!(distances.windows(2).all(|w| w[0] <= w[1]));
        let flat: Vec<_> = BlockPos::within_clipped_manhattan(origin, [1, 0, 1]).collect();
        assert!(flat.iter().all(|pos| pos.y == origin.y));
        assert_eq!(flat.len(), 5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spiral_around_visits_the_square_once_starting_at_the_center() {
        let center = BlockPos::new(5, 0, -5);
        let visited: Vec<_> = BlockPos::spiral_around(center, 2, Face::East, Face::South).collect();
        assert_eq!(visited[0], center);
        assert_eq!(visited[1], BlockPos::new(6, 0, -5));
        assert_eq!(visited[2], BlockPos::new(6, 0, -4));
        let unique: HashSet<_> = visited.iter().copied().collect();
        assert_eq!(unique.len(), visited.len());
        assert!(unique.contains(&BlockPos::new(3, 0, -7)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn random_in_cube_draws_within_the_cube() {
        let mut seed = 7_i32;
        let draws: Vec<_> = BlockPos::random_in_cube(
            |bound| {
                seed = seed.wrapping_mul(31).wrapping_add(11);
                seed.rem_euclid(bound)
            },
            50,
            BlockPos::new(0, 0, 0),
            2,
        )
        .collect();
        assert_eq!(draws.len(), 50);
        assert!(
            draws
                .iter()
                .all(|pos| pos.dist_chessboard(BlockPos::default()) <= 2)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn breadth_first_traversal_counts_by_depth_and_honours_skip_and_stop() {
        let around = |pos: BlockPos, add: &mut dyn FnMut(BlockPos)| {
            for face in [Face::East, Face::West, Face::North, Face::South] {
                add(pos.offset(face));
            }
        };
        let start = BlockPos::new(0, 0, 0);
        let all =
            BlockPos::breadth_first_traversal(start, 2, usize::MAX, around, |_| Traversal::Accept);
        assert_eq!(all, 13);
        let capped = BlockPos::breadth_first_traversal(start, 9, 5, around, |_| Traversal::Accept);
        assert_eq!(capped, 5);
        let walled = BlockPos::breadth_first_traversal(start, 3, usize::MAX, around, |pos| {
            if pos.x > 0 {
                Traversal::Skip
            } else {
                Traversal::Accept
            }
        });
        assert!(walled < 25);
        let stopped = BlockPos::breadth_first_traversal(start, 3, usize::MAX, around, |pos| {
            if pos == start {
                Traversal::Accept
            } else {
                Traversal::Stop
            }
        });
        assert_eq!(stopped, 1);
    }
}
