//! Drawing terrain onto a map. Sampling reads blocks (one pixel's
//! averaging area at a time, wholly inside one chunk) and is separate from
//! shading, which needs the pixel above and so runs over a column in order.

use tuff_vanilla_data::block::{Facing, Support};
use tuff_vanilla_data::map::{Brightness, NONE, WATER, packed};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

use crate::data::SIZE;

/// Where a render reads blocks from.
pub trait Terrain {
    fn state(&self, pos: BlockPos) -> Option<BlockState>;
    /// The y above the highest non-air block of a column, `None` where the
    /// column is not loaded.
    fn surface(&self, x: i32, z: i32) -> Option<i32>;
    fn min_y(&self) -> i32;
    fn has_ceiling(&self) -> bool;
}

/// What one pixel's area looks like from above.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub color: u8,
    pub height: f64,
    pub water_depth: i32,
}

/// The block size of one pixel at a map scale.
pub const fn block_scale(scale: u8) -> i32 {
    1 << scale
}

/// The block at the corner of the area pixel (`img_x`, `img_y`) covers.
pub fn area_origin(center: i32, scale: u8, img: i32) -> i32 {
    let block = block_scale(scale);
    ((center / block) + img - 64) * block
}

fn corrected(state: BlockState) -> BlockState {
    let fluid = state.fluid_state();
    if !fluid.is_empty() && !state.is_face_sturdy(Facing::Up, Support::Full) {
        fluid.block()
    } else {
        state
    }
}

/// Counts colours in the order they first appear; the commonest wins and
/// the earliest wins a tie.
struct Tally(Vec<(u8, i32)>);

impl Tally {
    fn add(&mut self, color: u8, count: i32) {
        match self.0.iter_mut().find(|(c, _)| *c == color) {
            Some((_, n)) => *n += count,
            None => self.0.push((color, count)),
        }
    }

    fn top(&self) -> u8 {
        let mut best: Option<(u8, i32)> = None;
        for &(color, count) in &self.0 {
            if best.is_none_or(|(_, n)| count > n) {
                best = Some((color, count));
            }
        }
        best.map_or(NONE, |(color, _)| color)
    }
}

/// Reads the area from (`x0`, `z0`) of side `block_scale(scale)`. `None`
/// where its chunk is not loaded.
pub fn sample(terrain: &dyn Terrain, x0: i32, z0: i32, scale: u8) -> Option<Sample> {
    let side = block_scale(scale);
    let min_y = terrain.min_y();
    let mut tally = Tally(Vec::new());
    let mut water_depth = 0;
    let mut height = 0.0f64;
    if terrain.has_ceiling() {
        terrain.state(BlockPos::new(x0, min_y, z0))?;
        let noise = x0.wrapping_add(z0.wrapping_mul(231_871));
        let mixed = noise
            .wrapping_mul(noise)
            .wrapping_mul(31_287_121)
            .wrapping_add(noise.wrapping_mul(11));
        if (mixed >> 20) & 1 == 0 {
            tally.add(Block::Dirt.default_state().map_color(), 10);
        } else {
            tally.add(Block::Stone.default_state().map_color(), 100);
        }
        height = 100.0;
    } else {
        terrain.surface(x0, z0)?;
        for dx in 0..side {
            for dz in 0..side {
                let (x, z) = (x0 + dx, z0 + dz);
                let top = terrain.surface(x, z)?;
                let mut y = top;
                let state = if y > min_y {
                    let mut found;
                    loop {
                        y -= 1;
                        found = terrain.state(BlockPos::new(x, y, z))?;
                        if found.map_color() != NONE || y <= min_y {
                            break;
                        }
                    }
                    if y > min_y && !found.fluid_state().is_empty() {
                        let mut below = y - 1;
                        loop {
                            let under = terrain.state(BlockPos::new(x, below, z))?;
                            below -= 1;
                            water_depth += 1;
                            if below <= min_y || under.fluid_state().is_empty() {
                                break;
                            }
                        }
                        found = corrected(found);
                    }
                    found
                } else {
                    Block::Bedrock.default_state()
                };
                height += f64::from(y) / f64::from(side * side);
                tally.add(state.map_color(), 1);
            }
        }
    }
    Some(Sample {
        color: tally.top(),
        height,
        water_depth: water_depth / (side * side),
    })
}

/// The packed pixel for a sample, given the height of the one above it in
/// the column and its position.
pub fn shade(sample: &Sample, previous_height: f64, scale: u8, img_x: i32, img_y: i32) -> u8 {
    let checker = f64::from((img_x + img_y) & 1);
    let brightness = if sample.color == WATER {
        let diff = f64::from(sample.water_depth) * 0.1 + checker * 0.2;
        if diff < 0.5 {
            Brightness::High
        } else if diff > 0.9 {
            Brightness::Low
        } else {
            Brightness::Normal
        }
    } else {
        let diff = (sample.height - previous_height) * 4.0 / f64::from(block_scale(scale) + 4)
            + (checker - 0.5) * 0.4;
        if diff > 0.6 {
            Brightness::High
        } else if diff < -0.6 {
            Brightness::Low
        } else {
            Brightness::Normal
        }
    };
    packed(sample.color, brightness)
}

/// What a render of one player's surroundings covers: the columns to
/// draw on a step and, per column, the rows around the player.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub scale: u8,
    pub center: [i32; 2],
    /// The pixel the player stands over.
    pub player: [i32; 2],
    pub radius: i32,
    /// Columns in the order they are drawn, with whether the step reaches
    /// them on its own (the rest only follow a changed neighbour).
    pub columns: Vec<(i32, bool)>,
}

impl Plan {
    pub fn new(scale: u8, center: [i32; 2], position: [f64; 3], ceiling: bool, step: u32) -> Self {
        let block = block_scale(scale);
        let px = ((position[0] - f64::from(center[0])).floor() as i32) / block + 64;
        let py = ((position[2] - f64::from(center[1])).floor() as i32) / block + 64;
        let mut radius = 128 / block;
        if ceiling {
            radius /= 2;
        }
        let mut columns: Vec<(i32, bool)> = Vec::new();
        for x in (px - radius + 1)..(px + radius) {
            if x < 0 || x >= SIZE as i32 {
                continue;
            }
            if (x & 15) as u32 == step & 15 {
                columns.push((x, true));
                if x + 1 < SIZE as i32 && x + 1 < px + radius {
                    columns.push((x + 1, false));
                }
            }
        }
        Self {
            scale,
            center,
            player: [px, py],
            radius,
            columns,
        }
    }

    /// The rows drawn in a column, top to bottom, as the walk visits them
    /// (rows above the map are visited too, only to carry the height).
    pub fn rows(&self) -> std::ops::Range<i32> {
        (self.player[1] - self.radius - 1)..(self.player[1] + self.radius)
    }

    pub fn in_map(x: i32, y: i32) -> bool {
        x >= 0 && y >= -1 && x < SIZE as i32 && y < SIZE as i32
    }

    /// The pixels whose areas the plan needs sampled.
    pub fn pixels(&self) -> Vec<(i32, i32)> {
        let mut pixels = Vec::new();
        for &(x, _) in &self.columns {
            for y in self.rows() {
                if Self::in_map(x, y) {
                    pixels.push((x, y));
                }
            }
        }
        pixels
    }

    /// Shades the sampled columns into `colors`, returning every pixel
    /// that changed.
    pub fn draw(
        &self,
        samples: &dyn Fn(i32, i32) -> Option<Sample>,
        colors: &mut [u8],
    ) -> Vec<(usize, u8)> {
        let mut changed = Vec::new();
        let mut consecutive = false;
        for &(x, direct) in &self.columns {
            if !(direct || consecutive) {
                continue;
            }
            consecutive = false;
            let mut previous = 0.0f64;
            for y in self.rows() {
                if !Self::in_map(x, y) {
                    continue;
                }
                let Some(sample) = samples(x, y) else {
                    continue;
                };
                let dx = x - self.player[0];
                let dy = y - self.player[1];
                let distance = dx * dx + dy * dy;
                let dither = distance > (self.radius - 2) * (self.radius - 2);
                let pixel = shade(&sample, previous, self.scale, x, y);
                previous = sample.height;
                if y >= 0 && distance < self.radius * self.radius && (!dither || (x + y) & 1 != 0) {
                    let index = x as usize + y as usize * SIZE;
                    if colors[index] != pixel {
                        colors[index] = pixel;
                        changed.push((index, pixel));
                        consecutive = true;
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Layers {
        ground: BlockState,
        top: i32,
        ceiling: bool,
        overrides: HashMap<(i32, i32, i32), BlockState>,
        surface_overrides: HashMap<(i32, i32), i32>,
    }

    impl Layers {
        fn new(ground: Block, top: i32) -> Self {
            Self {
                ground: ground.default_state(),
                top,
                ceiling: false,
                overrides: HashMap::new(),
                surface_overrides: HashMap::new(),
            }
        }
    }

    impl Terrain for Layers {
        fn state(&self, pos: BlockPos) -> Option<BlockState> {
            if let Some(state) = self.overrides.get(&(pos.x, pos.y, pos.z)) {
                return Some(*state);
            }
            Some(if pos.y <= self.top {
                self.ground
            } else {
                Block::Air.default_state()
            })
        }

        fn surface(&self, x: i32, z: i32) -> Option<i32> {
            Some(
                self.surface_overrides
                    .get(&(x, z))
                    .copied()
                    .unwrap_or(self.top + 1),
            )
        }

        fn min_y(&self) -> i32 {
            -64
        }

        fn has_ceiling(&self) -> bool {
            self.ceiling
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_flat_grass_field_is_one_colour_in_normal_brightness() {
        let terrain = Layers::new(Block::GrassBlock, 63);
        let first = sample(&terrain, 0, 0, 0).unwrap();
        assert_eq!(first.color, 1);
        assert_eq!(first.height, 63.0);
        let pixel = shade(&first, first.height, 0, 10, 10);
        assert_eq!(pixel, packed(1, Brightness::Normal));
        let odd = shade(&first, first.height, 0, 10, 11);
        assert_eq!(odd, packed(1, Brightness::Normal));
    }

    #[tuff_core::watchdog]
    #[test]
    fn slopes_shade_by_the_height_difference() {
        let rising = Sample {
            color: 1,
            height: 70.0,
            water_depth: 0,
        };
        assert_eq!(shade(&rising, 65.0, 0, 0, 0), packed(1, Brightness::High));
        assert_eq!(shade(&rising, 75.0, 0, 0, 0), packed(1, Brightness::Low));
        assert_eq!(shade(&rising, 70.0, 0, 0, 0), packed(1, Brightness::Normal));
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_shades_by_depth() {
        let mut terrain = Layers::new(Block::Stone, 60);
        let water = Block::Water.default_state();
        for y in 51..=64 {
            terrain.overrides.insert((0, y, 0), water);
        }
        terrain.surface_overrides.insert((0, 0), 65);
        let deep = sample(&terrain, 0, 0, 0).unwrap();
        assert_eq!(deep.color, WATER);
        assert_eq!(deep.water_depth, 14);
        assert_eq!(shade(&deep, 0.0, 0, 0, 0), packed(WATER, Brightness::Low));

        let mut shallow = Layers::new(Block::Sand, 60);
        shallow.overrides.insert((0, 61, 0), water);
        shallow.surface_overrides.insert((0, 0), 62);
        let sample = sample(&shallow, 0, 0, 0).unwrap();
        assert_eq!(sample.color, WATER);
        assert_eq!(sample.water_depth, 1);
        assert_eq!(
            shade(&sample, 0.0, 0, 0, 0),
            packed(WATER, Brightness::High)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_commonest_colour_in_an_area_wins() {
        let mut terrain = Layers::new(Block::GrassBlock, 63);
        terrain
            .overrides
            .insert((0, 63, 0), Block::Sand.default_state());
        let result = sample(&terrain, 0, 0, 1).unwrap();
        assert_eq!(result.color, 1);
        let mut tie = Layers::new(Block::GrassBlock, 63);
        tie.overrides
            .insert((1, 63, 0), Block::Sand.default_state());
        tie.overrides
            .insert((1, 63, 1), Block::Sand.default_state());
        tie.overrides
            .insert((0, 63, 1), Block::GrassBlock.default_state());
        let result = sample(&tie, 0, 0, 1).unwrap();
        assert_eq!(result.color, 1, "grass appears first and ties 2-2");
    }

    #[tuff_core::watchdog]
    #[test]
    fn ceilings_use_the_hashed_filler() {
        let mut terrain = Layers::new(Block::Netherrack, 63);
        terrain.ceiling = true;
        let result = sample(&terrain, 5, 9, 0).unwrap();
        assert_eq!(result.height, 100.0);
        let stone = Block::Stone.default_state().map_color();
        let dirt = Block::Dirt.default_state().map_color();
        assert!(result.color == stone || result.color == dirt);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unloaded_areas_are_skipped() {
        struct Nothing;
        impl Terrain for Nothing {
            fn state(&self, _: BlockPos) -> Option<BlockState> {
                None
            }
            fn surface(&self, _: i32, _: i32) -> Option<i32> {
                None
            }
            fn min_y(&self) -> i32 {
                0
            }
            fn has_ceiling(&self) -> bool {
                false
            }
        }
        assert_eq!(sample(&Nothing, 0, 0, 0), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_step_draws_every_sixteenth_column_and_follows_changes() {
        let terrain = Layers::new(Block::GrassBlock, 63);
        let plan = Plan::new(0, [0, 0], [0.5, 64.0, 0.5], false, 3);
        let direct: Vec<i32> = plan
            .columns
            .iter()
            .filter(|(_, direct)| *direct)
            .map(|(x, _)| *x)
            .collect();
        assert_eq!(direct, vec![3, 19, 35, 51, 67, 83, 99, 115]);
        assert!(plan.columns.contains(&(4, false)));
        let mut colors = vec![0u8; SIZE * SIZE];
        let samples =
            |x: i32, y: i32| sample(&terrain, area_origin(0, 0, x), area_origin(0, 0, y), 0);
        let changed = plan.draw(&samples, &mut colors);
        assert!(!changed.is_empty());
        let normal = packed(1, Brightness::Normal);
        let high = packed(1, Brightness::High);
        assert!(
            changed
                .iter()
                .all(|(_, pixel)| *pixel == normal || *pixel == high)
        );
        assert_eq!(colors[3 + 64 * SIZE], normal);
        assert!(
            colors[4 + 64 * SIZE] != 0,
            "the column after a change is drawn"
        );
        assert_eq!(colors[5 + 64 * SIZE], 0);
        let again = plan.draw(&samples, &mut colors);
        assert!(again.is_empty(), "an unchanged world changes nothing");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_edge_of_the_drawn_disc_is_dithered() {
        let terrain = Layers::new(Block::GrassBlock, 63);
        assert_eq!(Plan::new(2, [0, 0], [0.5, 64.0, 0.5], false, 0).radius, 32);
        let mut colors = vec![0u8; SIZE * SIZE];
        let samples =
            |x: i32, y: i32| sample(&terrain, area_origin(0, 2, x), area_origin(0, 2, y), 2);
        for step in 0..16 {
            let plan = Plan::new(2, [0, 0], [0.5, 64.0, 0.5], false, step);
            plan.draw(&samples, &mut colors);
        }
        let drawn = |x: usize, y: usize| colors[x + y * SIZE] != 0;
        assert!(drawn(64, 64));
        assert!(!drawn(64 + 33, 64), "outside the radius");
        assert!(drawn(95, 64));
        assert!(!drawn(95, 65), "dithered edge skips alternate pixels");
    }
}
