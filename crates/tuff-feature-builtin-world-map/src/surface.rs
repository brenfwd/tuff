//! What a chunk looks like from above: one [`Sample`] per column, and how
//! a sample is shaded against the one north of it. Sampling and shading
//! follow in-game maps at their closest scale, except that a dimension
//! with a ceiling is read below its roof.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::{Facing, Support};
use tuff_vanilla_data::map::{Brightness, NONE, WATER, packed};
use tuff_world_model::chunk::{Chunk, is_air};

/// One column: its base map colour, the height of the block it shows and
/// how deep the water there is, packed into 32 bits. Zero is a column
/// with nothing to show (missing chunk or void).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample(u32);

const MAX_DEPTH: i32 = 15;
const SECTION_HEIGHT: i32 = 16;

impl Sample {
    pub const EMPTY: Sample = Sample(0);

    pub fn new(color: u8, height: i32, water_depth: i32) -> Self {
        if color == NONE {
            return Self::EMPTY;
        }
        let depth = water_depth.clamp(0, MAX_DEPTH) as u32;
        let height = (height.clamp(i16::MIN.into(), i16::MAX.into()) as i16 as u16) as u32;
        Self(u32::from(color & 63) | (depth << 8) | (height << 16))
    }

    pub fn color(self) -> u8 {
        (self.0 & 63) as u8
    }

    pub fn height(self) -> i32 {
        i32::from((self.0 >> 16) as u16 as i16)
    }

    pub fn water_depth(self) -> i32 {
        ((self.0 >> 8) & 15) as i32
    }

    pub fn is_empty(self) -> bool {
        self.color() == NONE
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    /// The sample [`Self::bits`] came from, normalised as [`Self::new`]
    /// would, so any 32 bits make a valid sample.
    pub fn from_bits(bits: u32) -> Self {
        Self::new(
            (bits & 63) as u8,
            i32::from((bits >> 16) as u16 as i16),
            ((bits >> 8) & 15) as i32,
        )
    }

    /// The packed map pixel for this sample, given the height of the
    /// column north of it (`None` when that column shows nothing) and the
    /// world position, whose parity drives the checkerboard.
    pub fn shade(self, north: Option<i32>, x: i32, z: i32) -> u8 {
        if self.is_empty() {
            return 0;
        }
        let checker = f64::from((x.wrapping_add(z)) & 1);
        let brightness = if self.color() == WATER {
            let diff = f64::from(self.water_depth()) * 0.1 + checker * 0.2;
            if diff < 0.5 {
                Brightness::High
            } else if diff > 0.9 {
                Brightness::Low
            } else {
                Brightness::Normal
            }
        } else {
            let previous = north.unwrap_or(self.height());
            let diff = f64::from(self.height() - previous) * 4.0 / 5.0 + (checker - 0.5) * 0.4;
            if diff > 0.6 {
                Brightness::High
            } else if diff < -0.6 {
                Brightness::Low
            } else {
                Brightness::Normal
            }
        };
        packed(self.color(), brightness)
    }
}

/// A chunk's 256 samples, indexed `z << 4 | x`.
pub type ChunkSamples = [Sample; 256];

fn has_fluid(state: BlockState) -> bool {
    !state.fluid_state().is_empty()
}

fn shown(state: BlockState) -> BlockState {
    let fluid = state.fluid_state();
    if !fluid.is_empty() && !state.is_face_sturdy(Facing::Up, Support::Full) {
        fluid.block()
    } else {
        state
    }
}

/// Samples every column of `chunk`. With `ceiling`, each column is read
/// from the first air below the topmost blocks (the roof) downwards.
pub fn sample_chunk(chunk: &Chunk, ceiling: bool) -> ChunkSamples {
    let mut out = [Sample::EMPTY; 256];
    let min_y = chunk.shape().min_y;
    let sections = chunk.sections().len();
    for z in 0..16 {
        for x in 0..16 {
            out[(z << 4 | x) as usize] = sample_column(chunk, x, z, min_y, sections, ceiling);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    AboveRoof,
    InRoof,
    Open,
}

fn sample_column(
    chunk: &Chunk,
    x: i32,
    z: i32,
    min_y: i32,
    sections: usize,
    ceiling: bool,
) -> Sample {
    let mut phase = if ceiling {
        Phase::AboveRoof
    } else {
        Phase::Open
    };
    let column = ((z & 15) << 4 | (x & 15)) as usize;
    for index in (0..sections).rev() {
        let section = chunk.section(index);
        let base = min_y + index as i32 * SECTION_HEIGHT;
        if section.has_only_air() {
            if phase == Phase::InRoof {
                phase = Phase::Open;
            }
            continue;
        }
        for local in (0..SECTION_HEIGHT).rev() {
            let state = section.get((local as usize) << 8 | column);
            let air = is_air(state);
            match phase {
                Phase::AboveRoof => {
                    if !air {
                        phase = Phase::InRoof;
                    }
                    continue;
                }
                Phase::InRoof => {
                    if air {
                        phase = Phase::Open;
                    }
                    continue;
                }
                Phase::Open => {}
            }
            if state.map_color() == NONE {
                continue;
            }
            let y = base + local;
            let mut depth = 0;
            let mut color = state.map_color();
            if has_fluid(state) {
                let mut below = y;
                loop {
                    below -= 1;
                    depth += 1;
                    if below < min_y || depth > MAX_DEPTH || !has_fluid(chunk.get(x, below, z)) {
                        break;
                    }
                }
                color = shown(state).map_color();
            }
            return Sample::new(color, y, depth);
        }
    }
    Sample::EMPTY
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;
    use tuff_world_model::chunk::ChunkShape;

    fn flat(top: Block, height: i32) -> Chunk {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        for x in 0..16 {
            for z in 0..16 {
                for y in -64..height {
                    chunk.set(x, y, z, Block::Stone.default_state());
                }
                chunk.set(x, height, z, top.default_state());
            }
        }
        chunk
    }

    #[tuff_core::watchdog]
    #[test]
    fn samples_pack_and_unpack() {
        let sample = Sample::new(WATER, -60, 40);
        assert_eq!(sample.color(), WATER);
        assert_eq!(sample.height(), -60);
        assert_eq!(sample.water_depth(), 15);
        assert!(Sample::new(NONE, 70, 0).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_grass_field_shows_grass_at_its_height() {
        let samples = sample_chunk(&flat(Block::GrassBlock, 63), false);
        let grass = Block::GrassBlock.default_state().map_color();
        assert!(
            samples
                .iter()
                .all(|s| s.color() == grass && s.height() == 63)
        );
        let pixel = samples[0].shade(Some(63), 0, 0);
        assert_eq!(pixel, packed(grass, Brightness::Normal));
    }

    #[tuff_core::watchdog]
    #[test]
    fn slopes_shade_against_the_column_to_the_north() {
        let sample = Sample::new(1, 70, 0);
        assert_eq!(sample.shade(Some(65), 0, 0), packed(1, Brightness::High));
        assert_eq!(sample.shade(Some(75), 0, 0), packed(1, Brightness::Low));
        assert_eq!(sample.shade(Some(70), 0, 0), packed(1, Brightness::Normal));
        assert_eq!(sample.shade(None, 0, 0), packed(1, Brightness::Normal));
        assert_eq!(Sample::EMPTY.shade(Some(0), 0, 0), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_is_shaded_by_its_depth() {
        let mut chunk = flat(Block::Sand, 50);
        for y in 51..=64 {
            chunk.set(0, y, 0, Block::Water.default_state());
        }
        chunk.set(1, 51, 0, Block::Water.default_state());
        let samples = sample_chunk(&chunk, false);
        let deep = samples[0];
        assert_eq!(
            (deep.color(), deep.height(), deep.water_depth()),
            (WATER, 64, 14)
        );
        assert_eq!(deep.shade(None, 0, 0), packed(WATER, Brightness::Low));
        let shallow = samples[1];
        assert_eq!(shallow.water_depth(), 1);
        assert_eq!(shallow.shade(None, 1, 0), packed(WATER, Brightness::High));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_waterlogged_slab_shows_as_water() {
        let mut chunk = flat(Block::Sand, 50);
        let slab = Block::OakSlab
            .default_state()
            .with("waterlogged", "true")
            .unwrap();
        chunk.set(0, 51, 0, slab);
        let samples = sample_chunk(&chunk, false);
        assert_eq!(samples[0].color(), WATER);
        assert_eq!(samples[0].height(), 51);
    }

    #[tuff_core::watchdog]
    #[test]
    fn ceiling_dimensions_are_read_below_the_roof() {
        let shape = ChunkShape::new(0, 256);
        let mut chunk = Chunk::empty(shape);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..40 {
                    chunk.set(x, y, z, Block::Netherrack.default_state());
                }
                for y in 120..128 {
                    chunk.set(x, y, z, Block::Bedrock.default_state());
                }
            }
        }
        chunk.set(3, 40, 3, Block::Glowstone.default_state());
        let samples = sample_chunk(&chunk, true);
        let floor = Block::Netherrack.default_state().map_color();
        assert_eq!(samples[0].color(), floor);
        assert_eq!(samples[0].height(), 39);
        let glow = samples[3 << 4 | 3];
        assert_eq!(glow.color(), Block::Glowstone.default_state().map_color());
        assert_eq!(glow.height(), 40);
        let open = sample_chunk(&chunk, false);
        assert_eq!(open[0].color(), Block::Bedrock.default_state().map_color());
    }

    #[tuff_core::watchdog]
    #[test]
    fn void_columns_are_empty() {
        let samples = sample_chunk(&Chunk::empty(ChunkShape::new(0, 256)), false);
        assert!(samples.iter().all(|s| s.is_empty()));
    }
}
