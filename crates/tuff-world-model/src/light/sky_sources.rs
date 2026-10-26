use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::light::Direction;

use crate::chunk::{Chunk, ChunkShape};

use super::AIR;

/// Returned for columns open to the sky all the way down.
pub const NEGATIVE_INFINITY: i32 = i32::MIN;

/// For each column of a chunk, the lowest y that still receives full sky
/// light straight from above (the lowest "sky source"). `floor` (one below
/// the world) marks a column open all the way down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkyLightSources {
    lowest: Box<[i32; 256]>,
    floor: i32,
}

impl Default for SkyLightSources {
    fn default() -> Self {
        Self::open(NEGATIVE_INFINITY)
    }
}

fn index(x: i32, z: i32) -> usize {
    (x + z * 16) as usize
}

/// Light cannot pass down from `above` into `below`.
fn is_edge_occluded(above: BlockState, below: BlockState) -> bool {
    below.light_dampening() != 0
        || above
            .light_face(Direction::Down)
            .occludes(below.light_face(Direction::Up))
}

impl SkyLightSources {
    /// Every column open to the sky, for a world of `shape`.
    pub fn open_in(shape: ChunkShape) -> Self {
        Self::open(shape.min_y - 1)
    }

    fn open(floor: i32) -> Self {
        Self {
            lowest: Box::new([floor; 256]),
            floor,
        }
    }

    pub fn from_chunk(chunk: &Chunk) -> Self {
        let mut sources = Self::open_in(chunk.shape());
        sources.fill_from(chunk);
        sources
    }

    pub fn fill_from(&mut self, chunk: &Chunk) {
        self.floor = chunk.shape().min_y - 1;
        let floor = self.floor;
        let only_air: Vec<bool> = chunk.sections().map(|s| s.has_only_air()).collect();
        let Some(highest) = only_air.iter().rposition(|air| !air) else {
            self.lowest.fill(floor);
            return;
        };
        for z in 0..16 {
            for x in 0..16 {
                self.lowest[index(x, z)] =
                    Self::find_lowest_source_y(chunk, &only_air, highest, x, z).max(floor);
            }
        }
    }

    fn find_lowest_source_y(
        chunk: &Chunk,
        only_air: &[bool],
        highest: usize,
        x: i32,
        z: i32,
    ) -> i32 {
        let floor = chunk.shape().min_y - 1;
        let section_y = |index: usize| chunk.shape().min_section() + index as i32;
        let mut y = (section_y(highest) + 1) << 4;
        let mut below = y - 1;
        let mut above = AIR;
        for index in (0..=highest).rev() {
            if only_air[index] {
                above = AIR;
                y = section_y(index) << 4;
                below = y - 1;
                continue;
            }
            let base = section_y(index) << 4;
            for local_y in (0..16).rev() {
                let state = chunk.get(x, base + local_y, z);
                if is_edge_occluded(above, state) {
                    return y;
                }
                above = state;
                y = below;
                below -= 1;
            }
        }
        floor
    }

    /// Updates the column at chunk-local `x`/`z` after the block at `y`
    /// changed in `chunk`. Returns whether the column's source moved.
    pub fn update(&mut self, chunk: &Chunk, x: i32, y: i32, z: i32) -> bool {
        let index = index(x & 15, z & 15);
        let lowest = self.lowest[index];
        if y + 1 < lowest {
            return false;
        }
        let above = chunk.get(x, y + 1, z);
        let state = chunk.get(x, y, z);
        if self.update_edge(chunk, index, lowest, y + 1, above, y, state) {
            return true;
        }
        let below = chunk.get(x, y - 1, z);
        self.update_edge(chunk, index, lowest, y, state, y - 1, below)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn update_edge(
        &mut self,
        chunk: &Chunk,
        index: usize,
        lowest: i32,
        upper_y: i32,
        upper: BlockState,
        lower_y: i32,
        lower: BlockState,
    ) -> bool {
        if is_edge_occluded(upper, lower) {
            if upper_y <= lowest {
                return false;
            }
            self.lowest[index] = upper_y;
            return true;
        }
        if upper_y != lowest {
            return false;
        }
        let (x, z) = (index as i32 & 15, index as i32 >> 4);
        self.lowest[index] = self.find_lowest_source_below(chunk, x, z, lower_y, lower);
        true
    }

    fn find_lowest_source_below(
        &self,
        chunk: &Chunk,
        x: i32,
        z: i32,
        y: i32,
        state: BlockState,
    ) -> i32 {
        let mut y = y;
        let mut above = state;
        while y > self.floor {
            let below = chunk.get(x, y - 1, z);
            if is_edge_occluded(above, below) {
                return y;
            }
            above = below;
            y -= 1;
        }
        self.floor
    }

    pub fn lowest_source_y(&self, x: i32, z: i32) -> i32 {
        self.extend_below_world(self.lowest[index(x, z)])
    }

    pub fn highest_lowest_source_y(&self) -> i32 {
        self.extend_below_world(*self.lowest.iter().max().unwrap_or(&self.floor))
    }

    fn extend_below_world(&self, y: i32) -> i32 {
        if y == self.floor {
            NEGATIVE_INFINITY
        } else {
            y
        }
    }
}
