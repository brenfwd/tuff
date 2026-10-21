//! Sampling of the data-driven int, float and height providers.

use tuff_vanilla_data::worldgen::{FloatProvider, HeightProvider, IntProvider};

use crate::surface::GenContext;
use tuff_core::mth::{clamp_f32, next_int, normal, random_between, random_between_inclusive};
use tuff_item::random::RandomSource;

fn weighted<T>(
    entries: &'static [(&'static T, i32)],
    random: &mut impl RandomSource,
) -> &'static T {
    let total: i32 = entries.iter().map(|(_, w)| *w).sum();
    let mut selection = random.next_int_bounded(total);
    for (value, weight) in entries {
        selection -= weight;
        if selection < 0 {
            return value;
        }
    }
    unreachable!("selection within the total weight")
}

pub fn int(provider: &'static IntProvider, random: &mut impl RandomSource) -> i32 {
    match *provider {
        IntProvider::Constant(v) => v,
        IntProvider::Uniform { min, max } => random_between_inclusive(random, min, max),
        IntProvider::BiasedToBottom { min, max } => {
            let inner = random.next_int_bounded(max - min + 1) + 1;
            min + random.next_int_bounded(inner)
        }
        IntProvider::VeryBiasedToBottom { min, max } => {
            let a = random.next_int_bounded(max - min + 1) + 1;
            let b = random.next_int_bounded(a) + 1;
            min + random.next_int_bounded(b)
        }
        IntProvider::Clamped { source, min, max } => int(source, random).clamp(min, max),
        IntProvider::ClampedNormal {
            mean,
            deviation,
            min,
            max,
        } => clamp_f32(normal(random, mean, deviation), min as f32, max as f32) as i32,
        IntProvider::Trapezoid { min, max, plateau } => {
            if plateau == 0 && max == -min {
                return random.next_int_bounded(max + 1) - random.next_int_bounded(max + 1);
            }
            let range = max - min;
            if plateau == range {
                return random_between_inclusive(random, min, max);
            }
            let start = (range - plateau) / 2;
            let end = range - start;
            min + random_between_inclusive(random, 0, end)
                + random_between_inclusive(random, 0, start)
        }
        IntProvider::WeightedList(entries) => {
            let chosen = weighted(entries, random);
            int(chosen, random)
        }
    }
}

pub fn int_bounds(provider: &'static IntProvider) -> (i32, i32) {
    match *provider {
        IntProvider::Constant(v) => (v, v),
        IntProvider::Uniform { min, max }
        | IntProvider::BiasedToBottom { min, max }
        | IntProvider::VeryBiasedToBottom { min, max }
        | IntProvider::ClampedNormal { min, max, .. }
        | IntProvider::Trapezoid { min, max, .. } => (min, max),
        IntProvider::Clamped { source, min, max } => {
            let (a, b) = int_bounds(source);
            (a.max(min), b.min(max))
        }
        IntProvider::WeightedList(entries) => {
            entries
                .iter()
                .fold((i32::MAX, i32::MIN), |(lo, hi), (p, _)| {
                    let (a, b) = int_bounds(p);
                    (lo.min(a), hi.max(b))
                })
        }
    }
}

pub fn float(provider: &'static FloatProvider, random: &mut impl RandomSource) -> f32 {
    match *provider {
        FloatProvider::Constant(v) => v,
        FloatProvider::Uniform { min, max } => random_between(random, min, max),
        FloatProvider::ClampedNormal {
            mean,
            deviation,
            min,
            max,
        } => clamp_f32(normal(random, mean, deviation), min, max),
        FloatProvider::Trapezoid { min, max, plateau } => {
            let range = max - min;
            let start = (range - plateau) / 2.0;
            let end = range - start;
            min + random.next_float() * end + random.next_float() * start
        }
    }
}

pub fn height(
    provider: &'static HeightProvider,
    random: &mut impl RandomSource,
    span: GenContext,
) -> i32 {
    match *provider {
        HeightProvider::Constant(anchor) => span.resolve(anchor),
        HeightProvider::Uniform { min, max } => {
            let (min, max) = (span.resolve(min), span.resolve(max));
            if min > max {
                return min;
            }
            random_between_inclusive(random, min, max)
        }
        HeightProvider::BiasedToBottom { min, max, inner } => {
            let (min, max) = (span.resolve(min), span.resolve(max));
            if max - min - inner < 0 {
                return min;
            }
            let limit = random.next_int_bounded(max - min - inner + 1);
            random.next_int_bounded(limit + inner) + min
        }
        HeightProvider::VeryBiasedToBottom { min, max, inner } => {
            let (min, max) = (span.resolve(min), span.resolve(max));
            if max - min - inner < 0 {
                return min;
            }
            let upper = next_int(random, min + inner, max);
            let biased = next_int(random, min, upper - 1);
            next_int(random, min, biased - 1 + inner)
        }
        HeightProvider::Trapezoid { min, max, plateau } => {
            let (min, max) = (span.resolve(min), span.resolve(max));
            if min > max {
                return min;
            }
            let range = max - min;
            if plateau >= range {
                return random_between_inclusive(random, min, max);
            }
            let start = (range - plateau) / 2;
            let end = range - start;
            min + random_between_inclusive(random, 0, end)
                + random_between_inclusive(random, 0, start)
        }
        HeightProvider::WeightedList(entries) => {
            let chosen = weighted(entries, random);
            height(chosen, random, span)
        }
    }
}
