//! Tile geometry and pixel buffers. A level-0 tile is one storage file
//! region (32 x 32 chunks) at one block per pixel; a level-L tile covers
//! the four level L-1 tiles below it at half their resolution.

use tuff_world_model::pos::{ChunkPos, FileRegionPos};

use crate::surface::{ChunkSamples, Sample};

pub const SIDE: usize = 512;
pub const PIXELS: usize = SIDE * SIDE;
pub const CHUNKS: i32 = 32;
pub const MAX_LEVEL: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub dim: u16,
    pub level: u8,
    pub x: i32,
    pub z: i32,
}

impl TileKey {
    pub fn new(dim: u16, level: u8, x: i32, z: i32) -> Self {
        Self { dim, level, x, z }
    }

    /// Blocks along one side.
    pub fn span(self) -> i64 {
        (SIDE as i64) << self.level
    }

    /// The north-west block it covers.
    pub fn origin(self) -> (i64, i64) {
        (
            i64::from(self.x) * self.span(),
            i64::from(self.z) * self.span(),
        )
    }

    pub fn parent(self) -> Option<TileKey> {
        (self.level < MAX_LEVEL).then(|| {
            TileKey::new(
                self.dim,
                self.level + 1,
                self.x.div_euclid(2),
                self.z.div_euclid(2),
            )
        })
    }

    /// Which quarter of its parent it is: (0 or 1, 0 or 1) as (x, z).
    pub fn quadrant(self) -> (usize, usize) {
        (self.x.rem_euclid(2) as usize, self.z.rem_euclid(2) as usize)
    }

    /// The four tiles one level down, in row order.
    pub fn children(self) -> Option<[TileKey; 4]> {
        (self.level > 0).then(|| {
            let (x, z, l) = (self.x * 2, self.z * 2, self.level - 1);
            [
                TileKey::new(self.dim, l, x, z),
                TileKey::new(self.dim, l, x + 1, z),
                TileKey::new(self.dim, l, x, z + 1),
                TileKey::new(self.dim, l, x + 1, z + 1),
            ]
        })
    }

    /// The file regions it covers, as an inclusive range of x and z.
    pub fn file_regions(self) -> ((i32, i32), (i32, i32)) {
        let n = 1i32 << self.level;
        (
            (self.x * n, self.x * n + n - 1),
            (self.z * n, self.z * n + n - 1),
        )
    }

    /// The level-0 tile holding `chunk`.
    pub fn of_chunk(dim: u16, chunk: ChunkPos) -> TileKey {
        let file = chunk.file_region();
        TileKey::new(dim, 0, file.x, file.z)
    }

    /// For a level-0 tile: its file region and first chunk.
    pub fn first_chunk(self) -> ChunkPos {
        debug_assert_eq!(self.level, 0);
        ChunkPos::new(self.x * CHUNKS, self.z * CHUNKS)
    }

    pub fn file_region(self) -> FileRegionPos {
        FileRegionPos::new(self.x, self.z)
    }

    pub fn contains_chunk(self, chunk: ChunkPos) -> bool {
        self.level == 0 && chunk.file_region() == self.file_region()
    }

    pub fn chunks(self) -> impl Iterator<Item = ChunkPos> {
        let first = self.first_chunk();
        (0..CHUNKS).flat_map(move |dz| {
            (0..CHUNKS).map(move |dx| ChunkPos::new(first.x + dx, first.z + dz))
        })
    }

    /// The chunks just north of a level-0 tile, whose heights shade its
    /// first row.
    pub fn north_chunks(self) -> impl Iterator<Item = ChunkPos> {
        let first = self.first_chunk();
        (0..CHUNKS).map(move |dx| ChunkPos::new(first.x + dx, first.z - 1))
    }
}

/// A pixel rectangle `[x0, x1) x [z0, z1)` in tile pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub z0: usize,
    pub x1: usize,
    pub z1: usize,
}

impl Rect {
    pub const FULL: Rect = Rect {
        x0: 0,
        z0: 0,
        x1: SIDE,
        z1: SIDE,
    };

    pub fn union(self, other: Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            z0: self.z0.min(other.z0),
            x1: self.x1.max(other.x1),
            z1: self.z1.max(other.z1),
        }
    }

    /// Where this rectangle of a child lands in its parent.
    pub fn in_parent(self, quadrant: (usize, usize)) -> Rect {
        let half = SIDE / 2;
        Rect {
            x0: quadrant.0 * half + self.x0 / 2,
            z0: quadrant.1 * half + self.z0 / 2,
            x1: quadrant.0 * half + self.x1.div_ceil(2),
            z1: quadrant.1 * half + self.z1.div_ceil(2),
        }
    }
}

/// A level-0 tile's samples, the heights just north of it, and its
/// shaded pixels.
pub struct BaseTile {
    pub samples: Box<[Sample]>,
    pub north: Box<[Sample]>,
    pub pixels: Box<[u8]>,
}

impl BaseTile {
    pub fn empty() -> Self {
        Self {
            samples: vec![Sample::EMPTY; PIXELS].into_boxed_slice(),
            north: vec![Sample::EMPTY; SIDE].into_boxed_slice(),
            pixels: vec![0; PIXELS].into_boxed_slice(),
        }
    }

    pub const BYTES: usize = PIXELS * 4 + SIDE * 4 + PIXELS;

    /// Writes a chunk's samples at chunk offset (`cx`, `cz`) in the tile.
    pub fn place(&mut self, cx: usize, cz: usize, samples: &ChunkSamples) {
        for z in 0..16 {
            let row = (cz * 16 + z) * SIDE + cx * 16;
            self.samples[row..row + 16].copy_from_slice(&samples[z * 16..z * 16 + 16]);
        }
    }

    /// Sets the samples of the row north of the tile for chunk column `cx`
    /// from that chunk's last row.
    pub fn place_north(&mut self, cx: usize, samples: &ChunkSamples) {
        self.north[cx * 16..cx * 16 + 16].copy_from_slice(&samples[15 * 16..]);
    }

    /// Reshades `rect`, the tile's north-west block being `origin`.
    pub fn shade(&mut self, rect: Rect, origin: (i64, i64)) {
        for z in rect.z0..rect.z1 {
            for x in rect.x0..rect.x1 {
                let north = if z == 0 {
                    self.north[x]
                } else {
                    self.samples[(z - 1) * SIDE + x]
                };
                let north = (!north.is_empty()).then(|| north.height());
                let wx = (origin.0 + x as i64) as i32;
                let wz = (origin.1 + z as i64) as i32;
                self.pixels[z * SIDE + x] = self.samples[z * SIDE + x].shade(north, wx, wz);
            }
        }
    }

    /// The pixels a changed chunk at (`cx`, `cz`) reshades: itself and the
    /// row below it, which shades against its last row.
    pub fn chunk_rect(cx: usize, cz: usize) -> Rect {
        Rect {
            x0: cx * 16,
            z0: cz * 16,
            x1: cx * 16 + 16,
            z1: (cz * 16 + 17).min(SIDE),
        }
    }
}

/// The most common non-transparent of four pixels, the first in row order
/// winning a tie; transparent only when all four are.
pub fn pick(four: [u8; 4]) -> u8 {
    let mut best = 0u8;
    let mut best_count = 0;
    for &p in &four {
        if p >> 2 == 0 {
            continue;
        }
        let count = four.iter().filter(|&&q| q == p).count();
        if count > best_count {
            best = p;
            best_count = count;
        }
    }
    best
}

/// Writes the downsampled `child` (a full tile of pixels, or `None` for an
/// empty one) into its quadrant of `parent`, only where `rect` (child
/// pixels) reaches.
pub fn downsample(child: Option<&[u8]>, quadrant: (usize, usize), rect: Rect, parent: &mut [u8]) {
    let target = rect.in_parent(quadrant);
    let half = SIDE / 2;
    for pz in target.z0..target.z1 {
        for px in target.x0..target.x1 {
            let cx = (px - quadrant.0 * half) * 2;
            let cz = (pz - quadrant.1 * half) * 2;
            parent[pz * SIDE + px] = match child {
                None => 0,
                Some(c) => pick([
                    c[cz * SIDE + cx],
                    c[cz * SIDE + cx + 1],
                    c[(cz + 1) * SIDE + cx],
                    c[(cz + 1) * SIDE + cx + 1],
                ]),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn keys_nest_into_parents_and_children() {
        let key = TileKey::new(0, 0, -1, 3);
        let parent = key.parent().unwrap();
        assert_eq!(parent, TileKey::new(0, 1, -1, 1));
        assert_eq!(key.quadrant(), (1, 1));
        assert!(parent.children().unwrap().contains(&key));
        assert_eq!(TileKey::new(0, 6, 0, 0).parent(), None);
        assert_eq!(key.origin(), (-512, 1536));
        assert_eq!(parent.span(), 1024);
        assert_eq!(parent.file_regions(), ((-2, -1), (2, 3)));
        assert_eq!(
            TileKey::of_chunk(0, ChunkPos::new(-1, 33)),
            TileKey::new(0, 0, -1, 1)
        );
        assert_eq!(key.chunks().count(), 1024);
        assert!(key.chunks().all(|c| key.contains_chunk(c)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_commonest_colour_of_four_wins_and_ties_go_first() {
        assert_eq!(pick([5, 9, 9, 5]), 5);
        assert_eq!(pick([5, 9, 9, 9]), 9);
        assert_eq!(pick([0, 0, 0, 13]), 13);
        assert_eq!(
            pick([0, 1, 2, 3]),
            0,
            "brightness bits alone are transparent"
        );
        assert_eq!(pick([0, 0, 0, 0]), 0);
        assert_eq!(pick([6, 7, 8, 9]), 6);
    }

    #[tuff_core::watchdog]
    #[test]
    fn downsampling_fills_only_the_dirty_quarter() {
        let child = vec![5u8; PIXELS];
        let mut parent = vec![0u8; PIXELS];
        downsample(
            Some(&child),
            (1, 0),
            Rect {
                x0: 0,
                z0: 0,
                x1: 16,
                z1: 16,
            },
            &mut parent,
        );
        assert_eq!(parent[256], 5);
        assert_eq!(parent[7 * SIDE + 263], 5);
        assert_eq!(parent[8 * SIDE + 256], 0);
        assert_eq!(parent[0], 0);
        downsample(Some(&child), (0, 1), Rect::FULL, &mut parent);
        assert_eq!(parent[(SIDE - 1) * SIDE], 5);
        assert_eq!(parent[(SIDE / 2 - 1) * SIDE], 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_chunk_change_reshades_the_row_below_it() {
        let mut tile = BaseTile::empty();
        let mut samples = [Sample::new(1, 70, 0); 256];
        tile.place(0, 0, &samples);
        samples.fill(Sample::new(1, 60, 0));
        tile.place(0, 1, &samples);
        tile.shade(Rect::FULL, (0, 0));
        assert_ne!(tile.pixels[16 * SIDE], tile.pixels[17 * SIDE]);
        assert_eq!(
            BaseTile::chunk_rect(0, 0),
            Rect {
                x0: 0,
                z0: 0,
                x1: 16,
                z1: 17
            }
        );
        assert_eq!(BaseTile::chunk_rect(31, 31).z1, SIDE);
    }
}
