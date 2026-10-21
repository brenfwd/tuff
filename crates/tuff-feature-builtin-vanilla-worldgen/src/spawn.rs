//! Where a new world's spawn goes: the chunk whose climate best fits the
//! noise settings' spawn targets, found with the game's radial search.

use std::sync::Arc;

use crate::climate::quantize;
use crate::density::{Context, Sampler};
use crate::state::RandomState;

const MAX_RADIUS: i64 = 2048;

pub struct SpawnTarget {
    pub parameters: Vec<(Arc<Sampler>, i64, i64)>,
}

impl SpawnTarget {
    fn fitness(&self, ctx: &mut Context, x: i32, z: i32) -> i64 {
        let mut fitness = 0i64;
        for (sampler, min, max) in &self.parameters {
            let value = quantize(sampler.sample_value(ctx, x, 0, z));
            let above = value - max;
            let below = min - value;
            let distance = if above > 0 { above } else { below.max(0) };
            fitness = fitness.wrapping_add(distance.wrapping_mul(distance));
        }
        fitness
    }
}

pub fn targets(
    state: &mut RandomState,
    settings: &'static tuff_vanilla_data::worldgen::NoiseSettings,
) -> Vec<SpawnTarget> {
    settings
        .spawn_target
        .iter()
        .map(|target| SpawnTarget {
            parameters: target
                .parameters
                .iter()
                .map(|(named, range)| {
                    let function = state.loader().named(named);
                    let reference = Arc::new(crate::density::DensityFunction::Reference(function));
                    (state.compile_runtime(&reference), range.min, range.max)
                })
                .collect(),
        })
        .collect()
}

fn position_fitness(
    ctx: &mut Context,
    targets: &[SpawnTarget],
    x: i32,
    z: i32,
) -> ((i32, i32), i64) {
    let (qx, qz) = ((x >> 2) << 2, (z >> 2) << 2);
    let mut min = i64::MAX;
    for target in targets {
        min = min.min(target.fitness(ctx, qx, qz));
    }
    let bias = (x as i64) * (x as i64) + (z as i64) * (z as i64);
    (
        (x, z),
        min.wrapping_mul(MAX_RADIUS * MAX_RADIUS).wrapping_add(bias),
    )
}

/// The block position (x, z) the search settles on.
pub fn find(targets: &[SpawnTarget]) -> (i32, i32) {
    let mut ctx = Context::builder().caches().build();
    let mut result = position_fitness(&mut ctx, targets, 0, 0);
    for (max_radius, increment) in [(2048.0f32, 512.0f32), (512.0, 32.0)] {
        let mut angle = 0.0f32;
        let mut radius = increment;
        let origin = result.0;
        while radius <= max_radius {
            let x = origin.0 + ((angle as f64).sin() * radius as f64) as i32;
            let z = origin.1 + ((angle as f64).cos() * radius as f64) as i32;
            let candidate = position_fitness(&mut ctx, targets, x, z);
            if candidate.1 < result.1 {
                result = candidate;
            }
            angle += increment / radius;
            if angle as f64 > std::f64::consts::TAU {
                angle = 0.0;
                radius += increment;
            }
        }
    }
    result.0
}
