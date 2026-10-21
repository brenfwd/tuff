//! Chunk and region coordinates, and vanilla's view-distance rules.

use crate::block_pos::BlockPos;

/// Chunks per side of a region, the area one region actor owns. Any power
/// of two; storage keeps its own file layout ([`FILE_REGION_CHUNKS`]).
pub const REGION_CHUNKS: i32 = 32;
const REGION_SHIFT: u32 = REGION_CHUNKS.trailing_zeros();
const _: () = assert!(REGION_CHUNKS > 0 && REGION_CHUNKS.count_ones() == 1);

/// Chunks per side of a storage file region: one vanilla `.mca` file.
pub const FILE_REGION_CHUNKS: i32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

/// A map keyed by chunk position with a seeded multiply hash instead of
/// SipHash: regions look a chunk up on every block read.
pub type ChunkMap<V> = std::collections::HashMap<ChunkPos, V, ChunkHash>;

#[derive(Clone)]
pub struct ChunkHash {
    seed: u64,
}

impl Default for ChunkHash {
    fn default() -> Self {
        use std::hash::BuildHasher;
        Self {
            seed: std::hash::RandomState::new().hash_one(0u64) | 1,
        }
    }
}

impl std::hash::BuildHasher for ChunkHash {
    type Hasher = ChunkHasher;

    fn build_hasher(&self) -> ChunkHasher {
        ChunkHasher { state: self.seed }
    }
}

pub struct ChunkHasher {
    state: u64,
}

impl ChunkHasher {
    fn mix(&mut self, value: u64) {
        const K: u64 = 0x9E37_79B9_7F4A_7C15;
        let product = u128::from(self.state ^ value) * u128::from(K);
        self.state = (product as u64) ^ ((product >> 64) as u64);
    }
}

impl std::hash::Hasher for ChunkHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.mix(u64::from_le_bytes(word));
        }
    }

    fn write_i32(&mut self, value: i32) {
        self.mix(value as u32 as u64);
    }

    fn finish(&self) -> u64 {
        self.state
    }
}

impl ChunkPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    pub fn of_block(pos: BlockPos) -> Self {
        Self::new(pos.x >> 4, pos.z >> 4)
    }

    pub fn of_position(position: [f64; 3]) -> Self {
        Self::new(
            (position[0].floor() as i64 >> 4) as i32,
            (position[2].floor() as i64 >> 4) as i32,
        )
    }

    pub fn region(self) -> RegionPos {
        RegionPos::new(self.x >> REGION_SHIFT, self.z >> REGION_SHIFT)
    }

    pub fn file_region(self) -> FileRegionPos {
        FileRegionPos::new(self.x >> 5, self.z >> 5)
    }

    /// Chebyshev distance in chunks.
    pub fn distance(self, other: ChunkPos) -> i32 {
        (self.x - other.x).abs().max((self.z - other.z).abs())
    }

    /// Vanilla's `ChunkTrackingView.isWithinDistance`: a rounded-square
    /// area whose radius is `view_distance` plus a one- or two-chunk buffer.
    fn within(self, center: ChunkPos, view_distance: i32, buffer: i64) -> bool {
        let dx = (0i64).max((self.x as i64 - center.x as i64).abs() - buffer);
        let dz = (0i64).max((self.z as i64 - center.z as i64).abs() - buffer);
        dx * dx + dz * dz < (view_distance as i64) * (view_distance as i64)
    }

    /// Whether a player centred on `center` is sent this chunk.
    pub fn is_sent_to(self, center: ChunkPos, view_distance: i32) -> bool {
        self.within(center, view_distance, 2)
    }

    /// Whether entities in this chunk are visible to a player on `center`.
    pub fn is_tracked_from(self, center: ChunkPos, view_distance: i32) -> bool {
        self.within(center, view_distance, 1)
    }

    /// Every chunk sent to a player centred on `self`, nearest first.
    pub fn view(self, view_distance: i32) -> Vec<ChunkPos> {
        let radius = view_distance + 2;
        let mut chunks: Vec<ChunkPos> = (-radius..=radius)
            .flat_map(|dx| {
                (-radius..=radius).map(move |dz| ChunkPos::new(self.x + dx, self.z + dz))
            })
            .filter(|pos| pos.is_sent_to(self, view_distance))
            .collect();
        chunks.sort_by_key(|pos| {
            let (dx, dz) = ((pos.x - self.x) as i64, (pos.z - self.z) as i64);
            dx * dx + dz * dz
        });
        chunks
    }
}

/// A storage file's region: [`FILE_REGION_CHUNKS`] chunks per side,
/// independent of the region actors' size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileRegionPos {
    pub x: i32,
    pub z: i32,
}

impl FileRegionPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    pub fn contains(self, chunk: ChunkPos) -> bool {
        chunk.file_region() == self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionPos {
    pub x: i32,
    pub z: i32,
}

impl RegionPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    pub fn contains(self, chunk: ChunkPos) -> bool {
        chunk.region() == self
    }

    /// The eight surrounding regions.
    pub fn neighbors(self) -> impl Iterator<Item = RegionPos> {
        (-1..=1)
            .flat_map(move |dx| (-1..=1).map(move |dz| RegionPos::new(self.x + dx, self.z + dz)))
            .filter(move |pos| *pos != self)
    }

    /// Whether `chunk` (in this region) touches another region, so that
    /// neighbours need a copy of it for reads across the boundary.
    pub fn is_border(self, chunk: ChunkPos) -> bool {
        let last = REGION_CHUNKS - 1;
        let (lx, lz) = (chunk.x & last, chunk.z & last);
        self.contains(chunk) && (lx == 0 || lz == 0 || lx == last || lz == last)
    }

    /// Whether `chunk`, in some other region, borders this one.
    pub fn is_adjacent(self, chunk: ChunkPos) -> bool {
        let (min_x, min_z) = (self.x * REGION_CHUNKS, self.z * REGION_CHUNKS);
        !self.contains(chunk)
            && (min_x - 1..=min_x + REGION_CHUNKS).contains(&chunk.x)
            && (min_z - 1..=min_z + REGION_CHUNKS).contains(&chunk.z)
    }
}

/// Regions whose area intersects the horizontal square of `margin` blocks
/// around `position`.
pub fn regions_near(position: [f64; 3], margin: f64) -> Vec<RegionPos> {
    let low = ChunkPos::of_position([position[0] - margin, 0.0, position[2] - margin]).region();
    let high = ChunkPos::of_position([position[0] + margin, 0.0, position[2] + margin]).region();
    (low.x..=high.x)
        .flat_map(|x| (low.z..=high.z).map(move |z| RegionPos::new(x, z)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn regions_follow_floor_division() {
        let size = REGION_CHUNKS;
        assert_eq!(ChunkPos::new(-1, size - 1).region(), RegionPos::new(-1, 0));
        assert_eq!(
            ChunkPos::new(size, -size - 1).region(),
            RegionPos::new(1, -2)
        );
        assert_eq!(
            ChunkPos::new(-1, 31).file_region(),
            FileRegionPos::new(-1, 0)
        );
        assert_eq!(
            ChunkPos::new(32, -33).file_region(),
            FileRegionPos::new(1, -2)
        );
        assert_eq!(
            ChunkPos::of_position([-0.5, 0.0, 15.9]),
            ChunkPos::new(-1, 0)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn view_matches_vanilla_shape() {
        let center = ChunkPos::new(0, 0);
        let view = center.view(2);
        assert_eq!(view[0], center);
        assert!(ChunkPos::new(3, 0).is_sent_to(center, 2));
        assert!(!ChunkPos::new(4, 0).is_sent_to(center, 2));
        assert!(ChunkPos::new(2, 0).is_tracked_from(center, 2));
        assert!(!ChunkPos::new(3, 0).is_tracked_from(center, 2));
        assert!(ChunkPos::new(3, 3).is_sent_to(center, 2));
        assert!(!ChunkPos::new(4, 4).is_sent_to(center, 2));
        assert_eq!(view.len(), 49);
        assert_eq!(
            view.len(),
            view.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn border_and_adjacency() {
        let size = REGION_CHUNKS;
        let region = RegionPos::new(0, 0);
        assert!(region.is_border(ChunkPos::new(0, 1)));
        assert!(region.is_border(ChunkPos::new(size - 1, 1)));
        assert!(!region.is_border(ChunkPos::new(1, 1)) || size <= 2);
        assert!(region.is_adjacent(ChunkPos::new(-1, 1)));
        assert!(region.is_adjacent(ChunkPos::new(size, size)));
        assert!(!region.is_adjacent(ChunkPos::new(size + 1, 1)));
        assert_eq!(region.neighbors().count(), 8);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nearby_regions_cover_corners() {
        assert_eq!(
            regions_near([8.0, 0.0, 8.0], 2.0),
            vec![RegionPos::new(0, 0)]
        );
        assert_eq!(regions_near([0.5, 0.0, 0.5], 2.0).len(), 4);
        let size = f64::from(REGION_CHUNKS * 16);
        let wide = regions_near([size / 2.0, 0.0, size / 2.0], size * 1.5);
        assert_eq!(wide.len(), 16, "regions between the corners count too");
    }
}
