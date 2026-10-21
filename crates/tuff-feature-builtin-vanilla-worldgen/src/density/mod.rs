//! Density functions: the data-driven expression trees the noise router is
//! made of ([`function`]), compiled once per world into samplers
//! ([`sampler`]) that evaluate either single blocks or whole volumes
//! against a per-task [`Context`] holding the caches.

mod compile;
pub mod function;
mod interval;
mod program;
mod sampler;

pub use compile::Compiler;
pub use function::{Axis, DensityFunction, Df, DistanceMetric, NamedFunction, Tiling};
pub use interval::{Interval, java_max, java_min};
pub use program::{Compiled, Program, ProgramMemo};
pub use sampler::{BoundSampler, Buf, Context, ContextBuilder, Sampler};

/// A box of sample points: `size` points per axis starting at `min`, `step`
/// blocks apart. Buffers are laid out y fastest, then x, then z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DensityVolume {
    pub size_x: i32,
    pub size_y: i32,
    pub size_z: i32,
    pub min_x: i32,
    pub min_y: i32,
    pub min_z: i32,
    pub step_x: i32,
    pub step_y: i32,
    pub step_z: i32,
}

impl DensityVolume {
    #[allow(clippy::too_many_arguments)]
    pub fn stepped(size: [i32; 3], min: [i32; 3], step: [i32; 3]) -> Self {
        assert!(size.iter().all(|&s| s > 0), "volume size must be positive");
        assert!(step.iter().all(|&s| s > 0), "volume step must be positive");
        Self {
            size_x: size[0],
            size_y: size[1],
            size_z: size[2],
            min_x: min[0],
            min_y: min[1],
            min_z: min[2],
            step_x: step[0],
            step_y: step[1],
            step_z: step[2],
        }
    }

    pub fn blocks(size: [i32; 3], min: [i32; 3]) -> Self {
        Self::stepped(size, min, [1, 1, 1])
    }

    pub fn size(&self) -> usize {
        (self.size_x * self.size_y * self.size_z) as usize
    }

    pub fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (y + (x + z * self.size_x) * self.size_y) as usize
    }

    pub fn block_x(&self, x: i32) -> i32 {
        self.min_x + x * self.step_x
    }

    pub fn block_y(&self, y: i32) -> i32 {
        self.min_y + y * self.step_y
    }

    pub fn block_z(&self, z: i32) -> i32 {
        self.min_z + z * self.step_z
    }

    pub fn max_x(&self) -> i32 {
        self.min_x + self.size_x * self.step_x - 1
    }

    pub fn max_y(&self) -> i32 {
        self.min_y + self.size_y * self.step_y - 1
    }

    pub fn max_z(&self) -> i32 {
        self.min_z + self.size_z * self.step_z - 1
    }

    pub fn index_of_block(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (rx, ry, rz) = (x - self.min_x, y - self.min_y, z - self.min_z);
        if self.step_x == 1 && self.step_y == 1 && self.step_z == 1 {
            if rx >= 0
                && ry >= 0
                && rz >= 0
                && rx < self.size_x
                && ry < self.size_y
                && rz < self.size_z
            {
                return Some(self.index(rx, ry, rz));
            }
            return None;
        }
        let inside = rx >= 0
            && ry >= 0
            && rz >= 0
            && rx < self.size_x * self.step_x
            && ry < self.size_y * self.step_y
            && rz < self.size_z * self.step_z
            && rx.rem_euclid(self.step_x) == 0
            && ry.rem_euclid(self.step_y) == 0
            && rz.rem_euclid(self.step_z) == 0;
        inside.then(|| {
            self.index(
                rx.div_euclid(self.step_x),
                ry.div_euclid(self.step_y),
                rz.div_euclid(self.step_z),
            )
        })
    }
}
