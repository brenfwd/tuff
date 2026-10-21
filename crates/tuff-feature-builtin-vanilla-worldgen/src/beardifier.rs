//! Terrain adaptation around structures ("beards" and burial), bound into
//! the density context per chunk: pieces of structures with a terrain
//! adaptation raise terrain under them or carve it around them.

use std::sync::OnceLock;

use tuff_vanilla_data::structure::TerrainAdaptation;

use crate::density::{BoundSampler, DensityVolume, Interval};
use crate::structure::References;
use crate::structure::geometry::BoundingBox;
use tuff_world_model::pos::ChunkPos;

pub const RANGE: Interval = Interval::INFINITE;

const KERNEL_RADIUS: i32 = 12;
const KERNEL_SIZE: i32 = 24;

/// Where one jigsaw piece connects to another, which the terrain blends
/// toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Junction {
    pub source_x: i32,
    pub source_ground_y: i32,
    pub source_z: i32,
    pub delta_y: i32,
    pub rigid_target: bool,
}

#[derive(Debug, Clone, Copy)]
struct Rigid {
    bounding_box: BoundingBox,
    adaptation: TerrainAdaptation,
    ground_delta: i32,
}

#[derive(Debug, Default)]
pub struct Beardifier {
    pieces: Vec<Rigid>,
    junctions: Vec<Junction>,
    affected: Option<BoundingBox>,
}

fn kernel() -> &'static [f32] {
    static KERNEL: OnceLock<Vec<f32>> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let size = KERNEL_SIZE as usize;
        let mut kernel = vec![0.0f32; size * size * size];
        for zi in 0..KERNEL_SIZE {
            for xi in 0..KERNEL_SIZE {
                for yi in 0..KERNEL_SIZE {
                    let (dx, dy, dz) = (
                        (xi - KERNEL_RADIUS) as f64,
                        (yi - KERNEL_RADIUS) as f64 + 0.5,
                        (zi - KERNEL_RADIUS) as f64,
                    );
                    let distance = dx * dx + dy * dy + dz * dz;
                    let value = std::f64::consts::E.powf(-distance / 16.0);
                    kernel[((zi * KERNEL_SIZE + xi) * KERNEL_SIZE + yi) as usize] = value as f32;
                }
            }
        }
        kernel
    })
}

fn fast_inv_sqrt(x: f64) -> f64 {
    let half = 0.5 * x;
    let i = x.to_bits() as i64;
    let y = f64::from_bits((0x5FE6_EB50_C7B5_37AA_i64 - (i >> 1)) as u64);
    y * (1.5 - half * y * y)
}

fn length_squared(x: f32, y: f32, z: f32) -> f32 {
    x * x + y * y + z * z
}

fn bury_contribution(dx: f32, dy: f32, dz: f32) -> f32 {
    let distance = length_squared(dx, dy, dz);
    if distance >= 36.0 {
        return 0.0;
    }
    1.0 - (distance as f64).sqrt() as f32 / 6.0
}

fn beard_contribution(dx: i32, dy: i32, dz: i32, to_ground: i32) -> f32 {
    let (xi, yi, zi) = (dx + KERNEL_RADIUS, dy + KERNEL_RADIUS, dz + KERNEL_RADIUS);
    let in_range = |i: i32| (0..KERNEL_SIZE).contains(&i);
    if !in_range(xi) || !in_range(yi) || !in_range(zi) {
        return 0.0;
    }
    let dy_offset = to_ground as f32 + 0.5;
    let distance = length_squared(dx as f32, dy_offset, dz as f32);
    let value = -dy_offset * fast_inv_sqrt((distance / 2.0) as f64) as f32 / 2.0;
    value * kernel()[((zi * KERNEL_SIZE + xi) * KERNEL_SIZE + yi) as usize]
}

impl Beardifier {
    /// The pieces near `pos` of every structure referencing it whose
    /// terrain adapts.
    pub fn for_chunk(references: &[References], pos: ChunkPos) -> Self {
        let (min_x, min_z) = (pos.x << 4, pos.z << 4);
        let mut out = Self::default();
        let mut any: Option<BoundingBox> = None;
        let include = |any: &mut Option<BoundingBox>, b: BoundingBox| {
            *any = Some(match any {
                Some(a) => BoundingBox::encapsulating(a, &b),
                None => b,
            });
        };
        for refs in references {
            for start in &refs.starts {
                let adaptation = start.structure.terrain_adaptation;
                if adaptation == TerrainAdaptation::None {
                    continue;
                }
                for piece in &start.pieces {
                    let bounding_box = piece.bounding_box();
                    if !bounding_box.intersects_xz(
                        min_x - KERNEL_RADIUS,
                        min_z - KERNEL_RADIUS,
                        min_x + 15 + KERNEL_RADIUS,
                        min_z + 15 + KERNEL_RADIUS,
                    ) {
                        continue;
                    }
                    if let Some(ground_delta) = piece.rigid_ground_delta() {
                        out.pieces.push(Rigid {
                            bounding_box,
                            adaptation,
                            ground_delta,
                        });
                        include(&mut any, bounding_box);
                    }
                    for junction in piece.junctions() {
                        let (x, z) = (junction.source_x, junction.source_z);
                        if x > min_x - KERNEL_RADIUS
                            && z > min_z - KERNEL_RADIUS
                            && x < min_x + 15 + KERNEL_RADIUS
                            && z < min_z + 15 + KERNEL_RADIUS
                        {
                            out.junctions.push(*junction);
                            include(
                                &mut any,
                                BoundingBox::new(
                                    x,
                                    junction.source_ground_y,
                                    z,
                                    x,
                                    junction.source_ground_y,
                                    z,
                                ),
                            );
                        }
                    }
                }
            }
        }
        out.affected = any.map(|b| b.inflated(24));
        out
    }

    pub fn is_empty(&self) -> bool {
        self.affected.is_none()
    }

    fn sample(&self, x: i32, y: i32, z: i32) -> f32 {
        let mut value = 0.0f32;
        for rigid in &self.pieces {
            let b = rigid.bounding_box;
            let dx = 0.max((b.min_x - x).max(x - b.max_x));
            let dz = 0.max((b.min_z - z).max(z - b.max_z));
            let ground = b.min_y + rigid.ground_delta;
            let to_ground = y - ground;
            let dy = match rigid.adaptation {
                TerrainAdaptation::None => 0,
                TerrainAdaptation::Bury | TerrainAdaptation::BeardThin => to_ground,
                TerrainAdaptation::BeardBox => 0.max((ground - y).max(y - b.max_y)),
                TerrainAdaptation::Encapsulate => 0.max((b.min_y - y).max(y - b.max_y)),
            };
            value += match rigid.adaptation {
                TerrainAdaptation::None => 0.0,
                TerrainAdaptation::Bury => bury_contribution(dx as f32, dy as f32 / 2.0, dz as f32),
                TerrainAdaptation::BeardThin | TerrainAdaptation::BeardBox => {
                    beard_contribution(dx, dy, dz, to_ground) * 0.8
                }
                TerrainAdaptation::Encapsulate => {
                    bury_contribution(dx as f32 / 2.0, dy as f32 / 2.0, dz as f32 / 2.0) * 0.8
                }
            };
        }
        for junction in &self.junctions {
            let dx = x - junction.source_x;
            let dy = y - junction.source_ground_y;
            value += beard_contribution(dx, dy, z - junction.source_z, dy) * 0.4;
        }
        value
    }
}

impl BoundSampler for Beardifier {
    fn sample_value(&self, x: i32, y: i32, z: i32) -> f32 {
        match self.affected {
            Some(b) if b.contains_xyz(x, y, z) => self.sample(x, y, z),
            _ => 0.0,
        }
    }

    fn sample_volume(&self, out: &mut [f32], volume: &DensityVolume) {
        out.fill(0.0);
        let Some(b) = self.affected else {
            return;
        };
        let volume_box = BoundingBox::new(
            volume.min_x,
            volume.min_y,
            volume.min_z,
            volume.max_x(),
            volume.max_y(),
            volume.max_z(),
        );
        if !b.intersects(&volume_box) {
            return;
        }
        let (sx, sy, sz) = (volume.step_x, volume.step_y, volume.step_z);
        let (bx, by, bz) = (volume.min_x, volume.min_y, volume.min_z);
        let min_x = 0.max(b.min_x - bx).div_euclid(sx);
        let min_y = 0.max(b.min_y - by).div_euclid(sy);
        let min_z = 0.max(b.min_z - bz).div_euclid(sz);
        let max_x = (volume.size_x - 1).min((b.max_x - bx).div_euclid(sx));
        let max_y = (volume.size_y - 1).min((b.max_y - by).div_euclid(sy));
        let max_z = (volume.size_z - 1).min((b.max_z - bz).div_euclid(sz));
        for z in min_z..=max_z {
            let block_z = volume.block_z(z);
            for x in min_x..=max_x {
                let block_x = volume.block_x(x);
                for y in min_y..=max_y {
                    out[volume.index(x, y, z)] = self.sample(block_x, volume.block_y(y), block_z);
                }
            }
        }
    }
}
