//! Which chunks a pre-generation job covers and in which order: a circle
//! or square of blocks around a centre block, walked as a square spiral
//! from the centre's chunk outward.

use tuff_world_model::pos::ChunkPos;

/// The largest radius a job may have, in blocks.
pub const MAX_RADIUS: i32 = 100_000;
/// Chunk coordinates past this lie outside the world's 30 million block
/// horizontal limit.
const MAX_CHUNK: i32 = 1_875_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Circle,
    Square,
}

impl Shape {
    pub fn name(self) -> &'static str {
        match self {
            Shape::Circle => "circle",
            Shape::Square => "square",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "circle" => Some(Shape::Circle),
            "square" => Some(Shape::Square),
            _ => None,
        }
    }
}

/// A shape of blocks: every block within `radius` of the centre block,
/// by Euclidean distance for a circle and by the larger axis distance
/// for a square. A chunk belongs to the area when any of its blocks does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub z: i32,
    pub radius: i32,
    pub shape: Shape,
}

/// The distance from `v` to the closest of the 16 block coordinates of
/// chunk coordinate `c` along one axis.
fn gap(v: i32, c: i32) -> i64 {
    let lo = i64::from(c) * 16;
    let hi = lo + 15;
    let v = i64::from(v);
    if v < lo {
        lo - v
    } else if v > hi {
        v - hi
    } else {
        0
    }
}

impl Area {
    pub fn new(x: i32, z: i32, radius: i32, shape: Shape) -> Self {
        Self {
            x,
            z,
            radius: radius.clamp(0, MAX_RADIUS),
            shape,
        }
    }

    pub fn centre(&self) -> ChunkPos {
        ChunkPos::new(self.x >> 4, self.z >> 4)
    }

    /// How many chunks out from the centre chunk the area can reach.
    pub fn reach(&self) -> i32 {
        let x = (self.x - self.radius) >> 4;
        let x2 = (self.x + self.radius) >> 4;
        let z = (self.z - self.radius) >> 4;
        let z2 = (self.z + self.radius) >> 4;
        let c = self.centre();
        (c.x - x).max(x2 - c.x).max(c.z - z).max(z2 - c.z)
    }

    pub fn contains(&self, chunk: ChunkPos) -> bool {
        if chunk.x.abs() > MAX_CHUNK || chunk.z.abs() > MAX_CHUNK {
            return false;
        }
        let dx = gap(self.x, chunk.x);
        let dz = gap(self.z, chunk.z);
        let r = i64::from(self.radius);
        match self.shape {
            Shape::Square => dx <= r && dz <= r,
            Shape::Circle => dx * dx + dz * dz <= r * r,
        }
    }

    /// The chunks of the area in spiral order.
    pub fn chunks(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        spiral(self.centre(), self.reach()).filter(|c| self.contains(*c))
    }

    /// How many chunks the area covers, counted a row at a time.
    pub fn count(&self) -> u64 {
        let c = self.centre();
        let reach = self.reach();
        let mut total = 0;
        for z in c.z - reach..=c.z + reach {
            let row = (c.x - reach..=c.x + reach)
                .filter(|x| self.contains(ChunkPos::new(*x, z)))
                .count();
            total += row as u64;
        }
        total
    }
}

/// Every chunk within `reach` chunks of `centre` (by the larger axis
/// distance), ring by ring: the centre, then each ring walked from just
/// below its east corner south, west, north and back east, so each step
/// moves to a neighbouring chunk.
pub fn spiral(centre: ChunkPos, reach: i32) -> impl Iterator<Item = ChunkPos> {
    std::iter::once(centre).chain((1..=reach.max(0)).flat_map(move |k| {
        let east = (-k + 1..=k).map(move |dz| (k, dz));
        let south = (-k..k).rev().map(move |dx| (dx, k));
        let west = (-k..k).rev().map(move |dz| (-k, dz));
        let north = (-k + 1..=k).map(move |dx| (dx, -k));
        east.chain(south)
            .chain(west)
            .chain(north)
            .map(move |(dx, dz)| ChunkPos::new(centre.x + dx, centre.z + dz))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[tuff_core::watchdog]
    #[test]
    fn the_spiral_starts_at_the_centre_and_steps_to_neighbours() {
        let centre = ChunkPos::new(3, -2);
        let order: Vec<ChunkPos> = spiral(centre, 3).collect();
        assert_eq!(order.len(), 49);
        assert_eq!(order[0], centre);
        assert_eq!(order[1], ChunkPos::new(4, -2));
        let unique: HashSet<ChunkPos> = order.iter().copied().collect();
        assert_eq!(unique.len(), 49);
        for pair in order.windows(2) {
            assert_eq!(pair[0].distance(pair[1]), 1, "{pair:?}");
            let manhattan = (pair[0].x - pair[1].x).abs() + (pair[0].z - pair[1].z).abs();
            assert!(manhattan <= 2, "{pair:?}");
        }
        for (i, pos) in order.iter().enumerate() {
            let ring = pos.distance(centre);
            let start = if ring == 0 {
                0
            } else {
                (2 * ring - 1).pow(2) as usize
            };
            assert!(
                i >= start && i < ((2 * ring + 1).pow(2) as usize),
                "{pos:?}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_first_ring_goes_around_clockwise_from_the_east() {
        let order: Vec<(i32, i32)> = spiral(ChunkPos::new(0, 0), 1).map(|c| (c.x, c.z)).collect();
        assert_eq!(
            order,
            [
                (0, 0),
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0),
                (-1, -1),
                (0, -1),
                (1, -1)
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_zero_radius_covers_the_centre_chunk_only() {
        for shape in [Shape::Circle, Shape::Square] {
            let area = Area::new(-7, 40, 0, shape);
            assert_eq!(area.chunks().collect::<Vec<_>>(), [ChunkPos::new(-1, 2)]);
            assert_eq!(area.count(), 1);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_square_covers_every_chunk_touching_its_blocks() {
        let area = Area::new(0, 0, 16, Shape::Square);
        let chunks: HashSet<ChunkPos> = area.chunks().collect();
        let expected: HashSet<ChunkPos> = (-1..=1)
            .flat_map(|x| (-1..=1).map(move |z| ChunkPos::new(x, z)))
            .collect();
        assert_eq!(chunks, expected);
        assert_eq!(area.count(), 9);
        assert_eq!(Area::new(8, 8, 7, Shape::Square).count(), 1);
        assert_eq!(Area::new(8, 8, 8, Shape::Square).count(), 4);
        assert_eq!(Area::new(7, 7, 8, Shape::Square).count(), 4);
        assert_eq!(Area::new(8, 8, 9, Shape::Square).count(), 9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_circle_leaves_out_the_far_corners() {
        let area = Area::new(0, 0, 100, Shape::Circle);
        assert!(area.contains(ChunkPos::new(6, 0)));
        assert!(!area.contains(ChunkPos::new(7, 0)));
        assert!(area.contains(ChunkPos::new(-7, 0)));
        assert!(!area.contains(ChunkPos::new(-8, 0)));
        assert!(area.contains(ChunkPos::new(4, 4)));
        assert!(!area.contains(ChunkPos::new(5, 5)));
        assert!(!area.contains(ChunkPos::new(-6, -6)));
        let square = Area::new(0, 0, 100, Shape::Square);
        assert!(area.count() < square.count());
        assert_eq!(square.count(), 14 * 14);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_count_matches_the_spiral() {
        for (x, z, r) in [(0, 0, 300), (-1234, 567, 777), (15, -16, 31), (1, 1, 1)] {
            for shape in [Shape::Circle, Shape::Square] {
                let area = Area::new(x, z, r, shape);
                let chunks: Vec<ChunkPos> = area.chunks().collect();
                let unique: HashSet<ChunkPos> = chunks.iter().copied().collect();
                assert_eq!(unique.len(), chunks.len());
                assert_eq!(chunks.len() as u64, area.count(), "{area:?}");
                let c = area.centre();
                let reach = area.reach() + 1;
                for dz in -reach..=reach {
                    for dx in -reach..=reach {
                        let pos = ChunkPos::new(c.x + dx, c.z + dz);
                        assert_eq!(area.contains(pos), unique.contains(&pos), "{pos:?}");
                    }
                }
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn counts_the_panel_shares() {
        let cases = [
            (0, 0, 100, 148, 196),
            (-1234, 567, 777, 7590, 9702),
            (5, -3, 1, 1, 1),
            (0, 0, 1000, 12522, 15876),
            (8, 8, 8, 3, 4),
            (8, 8, 7, 1, 1),
        ];
        for (x, z, r, circle, square) in cases {
            assert_eq!(Area::new(x, z, r, Shape::Circle).count(), circle);
            assert_eq!(Area::new(x, z, r, Shape::Square).count(), square);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_radius_is_capped_and_the_world_edge_respected() {
        assert_eq!(Area::new(0, 0, i32::MAX, Shape::Square).radius, MAX_RADIUS);
        let edge = Area::new(29_999_999, 0, 64, Shape::Square);
        assert!(edge.chunks().all(|c| c.x <= MAX_CHUNK));
    }
}
