//! Data-driven noise definitions (`worldgen/noise`) and how the game turns
//! them into octave stacks: two stacks of Perlin octaves, the second
//! sampled at a slightly stretched frequency, normalized so the sum has a
//! target deviation.

use crate::density::Interval;
use crate::noise::{NoiseStack, Octave};
use tuff_item::random::{Random, RandomSource};

const INPUT_FACTOR: f64 = 1.0181268882175227;
const TARGET_DEVIATION: f64 = 0.3333333333333333;
const OCTAVE_DEVIATION: f64 = 0.2702247831245211;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalization {
    Disabled,
    Enabled,
    Legacy,
}

/// The parameters of a `worldgen/noise` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseParameters {
    pub base_amplitude: f64,
    pub base_octave: i32,
    pub octave_count: i32,
    pub normalize: Normalization,
    /// Empty means every octave has weight one.
    pub amplitude_modifiers: Vec<f64>,
}

#[derive(Debug, Clone, Copy)]
struct OctaveInfo {
    index: i32,
    frequency: f64,
    amplitude: f64,
}

/// A noise definition with its octaves and normalization worked out.
#[derive(Debug, Clone)]
pub struct NormalNoise {
    parameters: NoiseParameters,
    octaves: Vec<OctaveInfo>,
    normalization: f64,
    range: Interval,
}

fn weight_at(weights: &[f64], octave: i32) -> f64 {
    if weights.is_empty() {
        1.0
    } else {
        weights[octave as usize]
    }
}

/// `DoubleStream.sum`, which uses compensated summation.
pub fn java_double_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let (mut sum, mut compensation, mut simple) = (0.0f64, 0.0f64, 0.0f64);
    for value in values {
        let tmp = value - compensation;
        let velvel = sum + tmp;
        compensation = (velvel - sum) - tmp;
        sum = velvel;
        simple += value;
    }
    let total = sum - compensation;
    if total.is_nan() && simple.is_infinite() {
        simple
    } else {
        total
    }
}

fn octave_layout(
    first_octave: i32,
    amplitude: f64,
    count: i32,
    normalize: bool,
    weights: &[f64],
) -> Vec<OctaveInfo> {
    let leading = if normalize {
        let geometric = 0.5f64.powf(-(count - 1) as f64) / (0.5f64.powf(-count as f64) - 1.0);
        amplitude * geometric
    } else {
        amplitude
    };
    (0..count)
        .scan((2f64.powi(first_octave), leading), |(freq, amp), octave| {
            let level = (octave, *freq, *amp);
            *freq *= 2.0;
            *amp *= 0.5;
            Some(level)
        })
        .filter_map(|(octave, frequency, amp)| {
            let weight = weight_at(weights, octave);
            (weight != 0.0).then_some(OctaveInfo {
                index: first_octave + octave,
                frequency,
                amplitude: amp * weight,
            })
        })
        .collect()
}

fn normalization_factor(target_amplitude: f64, octaves: &[OctaveInfo]) -> f64 {
    let spread = octaves
        .iter()
        .fold(0.0f64, |sum, octave| {
            let layer = OCTAVE_DEVIATION * octave.amplitude.abs();
            sum + layer * layer
        })
        .sqrt();
    if spread == 0.0 {
        0.0
    } else {
        target_amplitude * TARGET_DEVIATION / (spread * 2f64.sqrt())
    }
}

fn parity_normalization(base_amplitude: f64, count: i32, weights: &[f64]) -> f64 {
    let mut active = (0..count).filter(|&octave| weight_at(weights, octave) != 0.0);
    let lowest = active.next().unwrap_or(0);
    let highest = active.next_back().unwrap_or(lowest);
    let width = (highest - lowest + 1) as f64;
    base_amplitude * 0.5 * TARGET_DEVIATION / (0.1 * (1.0 + 1.0 / width))
}

impl NormalNoise {
    pub fn new(parameters: NoiseParameters) -> Self {
        let octaves = octave_layout(
            parameters.base_octave,
            parameters.base_amplitude,
            parameters.octave_count,
            parameters.normalize != Normalization::Disabled,
            &parameters.amplitude_modifiers,
        );
        let mut target = java_double_sum(octaves.iter().map(|o| o.amplitude.abs()));
        let mut factor = normalization_factor(target, &octaves);
        if parameters.normalize == Normalization::Legacy && factor != 0.0 {
            let parity = parity_normalization(
                parameters.base_amplitude,
                parameters.octave_count,
                &parameters.amplitude_modifiers,
            );
            target *= parity / factor;
            factor = parity;
        }
        let range = Interval::symmetric((target * TARGET_DEVIATION * 6.0) as f32);
        Self {
            parameters,
            octaves,
            normalization: factor,
            range,
        }
    }

    /// A noise whose base amplitude is chosen so its octaves are scaled as
    /// the game's older parity noises were, from explicit per-octave
    /// amplitudes.
    pub fn parity(first_octave: i32, amplitudes: &[f64]) -> Self {
        assert!(!amplitudes.is_empty(), "parity noise needs an amplitude");
        let count = amplitudes.len() as i32;
        let octaves = octave_layout(first_octave, 1.0, count, true, amplitudes);
        let target = java_double_sum(octaves.iter().map(|o| o.amplitude.abs()));
        let factor = normalization_factor(target, &octaves);
        let base_amplitude = if factor == 0.0 {
            1.0
        } else {
            parity_normalization(1.0, count, amplitudes) / factor
        };
        let amplitude_modifiers = if amplitudes.iter().all(|a| *a == 1.0) {
            Vec::new()
        } else {
            amplitudes.to_vec()
        };
        Self::new(NoiseParameters {
            base_amplitude,
            base_octave: first_octave,
            octave_count: count,
            normalize: Normalization::Enabled,
            amplitude_modifiers,
        })
    }

    pub fn range(&self) -> Interval {
        self.range
    }

    /// Instantiates the octaves from `random`, one fork per stack and one
    /// named stream per octave.
    pub fn create(&self, random: &mut Random) -> NoiseStack {
        let first = random.fork_positional();
        let second = random.fork_positional();
        let mut stack = NoiseStack::builder();
        for octave in &self.octaves {
            let seed = format!("octave_{}", octave.index);
            let a = Octave::perlin(&mut first.from_hash_of(&seed));
            let b = Octave::perlin(&mut second.from_hash_of(&seed));
            let amplitude = (self.normalization * octave.amplitude) as f32;
            stack = stack.add(a, octave.frequency, amplitude).add(
                b,
                octave.frequency * INPUT_FACTOR,
                amplitude,
            );
        }
        stack.build()
    }

    /// The nether's biome noises keep their pre-1.18 octave layout, seeded
    /// from a legacy generator.
    pub fn create_legacy_nether(&self, random: &mut Random) -> NoiseStack {
        let amplitudes = if self.parameters.amplitude_modifiers.is_empty() {
            vec![1.0; self.parameters.octave_count as usize]
        } else {
            self.parameters.amplitude_modifiers.clone()
        };
        let first = legacy_fbm(random, self.parameters.base_octave, &amplitudes);
        let second = legacy_fbm(random, self.parameters.base_octave, &amplitudes);
        let amplitude = (self.normalization * self.parameters.base_amplitude) as f32;
        NoiseStack::builder()
            .add_stack(first, 1.0, amplitude)
            .add_stack(second, INPUT_FACTOR, amplitude)
            .build()
    }
}

fn legacy_fbm(random: &mut Random, first_octave: i32, amplitudes: &[f64]) -> NoiseStack {
    let octaves = amplitudes.len() as i32;
    let zero_index = -first_octave;
    let mut levels: Vec<Option<Octave>> = vec![None; octaves as usize];
    let zero = Octave::perlin(random);
    if zero_index >= 0 && zero_index < octaves && amplitudes[zero_index as usize] != 0.0 {
        levels[zero_index as usize] = Some(zero);
    }
    for i in (0..zero_index).rev() {
        if i < octaves {
            if amplitudes[i as usize] != 0.0 {
                levels[i as usize] = Some(Octave::perlin(random));
            } else {
                random.consume_count(262);
            }
        } else {
            random.consume_count(262);
        }
    }
    assert!(
        zero_index >= octaves - 1,
        "positive octaves are not supported"
    );
    let mut factor = 2f64.powi(-zero_index);
    let mut value_factor = 2f64.powi(octaves - 1) / (2f64.powi(octaves) - 1.0);
    let mut stack = NoiseStack::builder();
    for (i, level) in levels.into_iter().enumerate() {
        if let Some(octave) = level {
            stack = stack.add(octave, factor, (value_factor * amplitudes[i]) as f32);
        }
        factor *= 2.0;
        value_factor /= 2.0;
    }
    stack.build()
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    fn digest(noise: &NormalNoise) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |v: u64| hash = (hash ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        for o in &noise.octaves {
            mix(o.index as u64);
            mix(o.frequency.to_bits());
            mix(o.amplitude.to_bits());
        }
        mix(noise.normalization.to_bits());
        mix(noise.range.max.to_bits() as u64);
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn octave_layout_and_normalization_are_pinned() {
        let mut combined = 0u64;
        for (i, normalize) in [
            Normalization::Disabled,
            Normalization::Enabled,
            Normalization::Legacy,
        ]
        .into_iter()
        .enumerate()
        {
            for (base_octave, count, modifiers) in [
                (-7, 2, vec![]),
                (-9, 4, vec![1.0, 0.0, 1.5, 0.0]),
                (-3, 6, vec![0.0, 0.0, 1.0, 1.0, 0.5, 0.25]),
            ] {
                let noise = NormalNoise::new(NoiseParameters {
                    base_amplitude: 1.25 + i as f64,
                    base_octave,
                    octave_count: count,
                    normalize,
                    amplitude_modifiers: modifiers,
                });
                combined = combined.rotate_left(7) ^ digest(&noise);
            }
        }
        combined =
            combined.rotate_left(7) ^ digest(&NormalNoise::parity(-4, &[1.0, 2.0, 0.0, 1.0]));
        combined = combined.rotate_left(7) ^ digest(&NormalNoise::parity(-8, &[1.0, 1.0]));
        assert_eq!(combined, 739852349928103428);
    }
}
