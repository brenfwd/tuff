//! The pre-1.18 terrain noise (`old_blended_noise`): two smeared Perlin
//! "limit" stacks blended by a third "main" stack.

use std::sync::Arc;

use crate::density::{Interval, Sampler};
use crate::noise::{NoiseStack, Octave};
use tuff_item::random::Random;

const BASE_SCALE: f64 = 684.412;
const LIMIT_FACTOR: f64 = 0.9999847412109375;
const MAIN_FACTOR: f64 = 12.75;
const LIMIT_FIRST_OCTAVE: i32 = -15;
const MAIN_FIRST_OCTAVE: i32 = -7;

fn fbm(random: &mut Random, first_octave: i32, smear_y: f64, value_factor: f64) -> NoiseStack {
    let octaves = -first_octave + 1;
    let mut factor = 1.0;
    let mut value = value_factor / (2f64.powi(octaves) - 1.0);
    let mut stack = NoiseStack::builder();
    for _ in 0..octaves {
        stack = stack.add(
            Octave::smeared(random, smear_y * factor),
            factor,
            value as f32,
        );
        factor /= 2.0;
        value *= 2.0;
    }
    stack.build()
}

pub fn range(y_scale: f64, smear_scale_multiplier: f64) -> Interval {
    let smear_y = BASE_SCALE * y_scale * smear_scale_multiplier;
    let octaves = -LIMIT_FIRST_OCTAVE + 1;
    let mut factor = 1.0;
    let mut value = LIMIT_FACTOR / (2f64.powi(octaves) - 1.0);
    let mut range = Interval::exact(0.0);
    for _ in 0..octaves {
        let layer = Interval::product(
            Interval::symmetric(((smear_y * factor).abs() + 2.0) as f32),
            Interval::exact(value as f32),
        );
        range = Interval::sum(range, layer);
        factor /= 2.0;
        value *= 2.0;
    }
    range
}

pub fn compile(
    random: &mut Random,
    xz_scale: f64,
    y_scale: f64,
    xz_factor: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
) -> Sampler {
    let xz_multiplier = BASE_SCALE * xz_scale;
    let y_multiplier = BASE_SCALE * y_scale;
    let limit_smear = y_multiplier * smear_scale_multiplier;
    let main_smear = limit_smear / y_factor;
    let min_limit = fbm(random, LIMIT_FIRST_OCTAVE, limit_smear, LIMIT_FACTOR);
    let max_limit = fbm(random, LIMIT_FIRST_OCTAVE, limit_smear, LIMIT_FACTOR);
    let main = fbm(random, MAIN_FIRST_OCTAVE, main_smear, MAIN_FACTOR);
    let noise = |stack: NoiseStack, xz: f64, y: f64| Sampler::Noise {
        noise: Arc::new(stack),
        xz_scale: xz,
        y_scale: y,
    };
    let choice = Sampler::Clamp(
        Box::new(Sampler::ConstAdd(
            Box::new(noise(
                main,
                xz_multiplier / xz_factor,
                y_multiplier / y_factor,
            )),
            0.5,
        )),
        0.0,
        1.0,
    );
    Sampler::Lerp(
        Box::new(choice),
        Box::new(noise(min_limit, xz_multiplier, y_multiplier)),
        Box::new(noise(max_limit, xz_multiplier, y_multiplier)),
    )
}
