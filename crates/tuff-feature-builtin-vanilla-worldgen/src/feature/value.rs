//! Int, float and height providers parsed from feature JSON.

use serde_json::Value;
use tuff_vanilla_data::worldgen::VerticalAnchor;

use crate::json::{self, Result, obj};
use crate::surface::GenContext;
use tuff_core::mth::{clamp_f32, next_int, normal, random_between, random_between_inclusive};
use tuff_item::random::RandomSource;

#[derive(Debug, Clone)]
pub enum OwnedIntProvider {
    Constant(i32),
    Uniform(i32, i32),
    BiasedToBottom(i32, i32),
    VeryBiasedToBottom(i32, i32),
    Clamped(Box<OwnedIntProvider>, i32, i32),
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: i32,
        max: i32,
    },
    Trapezoid {
        min: i32,
        max: i32,
        plateau: i32,
    },
    WeightedList(Vec<(OwnedIntProvider, i32)>),
}

fn weighted<'a, T>(entries: &'a [(T, i32)], random: &mut impl RandomSource) -> &'a T {
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

impl OwnedIntProvider {
    pub fn parse(value: &Value) -> Result<Self> {
        if let Some(v) = value.as_i64() {
            return Ok(Self::Constant(v as i32));
        }
        let what = "int provider";
        let range = |o: &json::Obj| -> Result<(i32, i32)> {
            let (min, max) = (o.i32("min_inclusive")?, o.i32("max_inclusive")?);
            if max < min {
                return Err(format!(
                    "Max must be at least min, min_inclusive: {min}, max_inclusive: {max}"
                ));
            }
            Ok((min, max))
        };
        Ok(match json::qualify(json::kind(value)?).as_str() {
            "minecraft:constant" => Self::Constant(obj(value, &["value"], what)?.i32("value")?),
            "minecraft:uniform" => {
                let (a, b) = range(&obj(value, &["min_inclusive", "max_inclusive"], what)?)?;
                Self::Uniform(a, b)
            }
            "minecraft:biased_to_bottom" => {
                let (a, b) = range(&obj(value, &["min_inclusive", "max_inclusive"], what)?)?;
                Self::BiasedToBottom(a, b)
            }
            "minecraft:very_biased_to_bottom" => {
                let (a, b) = range(&obj(value, &["min_inclusive", "max_inclusive"], what)?)?;
                Self::VeryBiasedToBottom(a, b)
            }
            "minecraft:clamped" => {
                let o = obj(value, &["source", "min_inclusive", "max_inclusive"], what)?;
                let (a, b) = range(&o)?;
                Self::Clamped(Box::new(Self::parse(o.req("source")?)?), a, b)
            }
            "minecraft:clamped_normal" => {
                let o = obj(
                    value,
                    &["mean", "deviation", "min_inclusive", "max_inclusive"],
                    what,
                )?;
                let (min, max) = range(&o)?;
                Self::ClampedNormal {
                    mean: o.f32("mean")?,
                    deviation: o.f32("deviation")?,
                    min,
                    max,
                }
            }
            "minecraft:trapezoid" => {
                let o = obj(value, &["min", "max", "plateau"], what)?;
                let (min, max, plateau) = (o.i32("min")?, o.i32("max")?, o.i32("plateau")?);
                if max < min {
                    return Err(format!("Max must be larger than min: [{min}, {max}]"));
                }
                if i64::from(plateau) > i64::from(max) - i64::from(min) {
                    return Err(format!(
                        "Plateau can at most be the full span: [{min}, {max}]"
                    ));
                }
                Self::Trapezoid { min, max, plateau }
            }
            "minecraft:weighted_list" => {
                let o = obj(value, &["distribution"], what)?;
                let mut entries = Vec::new();
                for entry in o.list("distribution")? {
                    let e = obj(entry, &["data", "weight"], what)?;
                    entries.push((Self::parse(e.req("data")?)?, e.i32("weight")?));
                }
                Self::WeightedList(entries)
            }
            other => return Err(format!("unmodeled int provider {other}")),
        })
    }

    /// The largest value [`Self::sample`] can return.
    pub fn max_inclusive(&self) -> i32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(_, max)
            | Self::BiasedToBottom(_, max)
            | Self::VeryBiasedToBottom(_, max)
            | Self::ClampedNormal { max, .. }
            | Self::Trapezoid { max, .. } => *max,
            Self::Clamped(source, _, max) => (*max).min(source.max_inclusive()),
            Self::WeightedList(entries) => entries
                .iter()
                .map(|(p, _)| p.max_inclusive())
                .max()
                .unwrap_or(0),
        }
    }

    pub fn sample(&self, random: &mut impl RandomSource) -> i32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(min, max) => random_between_inclusive(random, *min, *max),
            Self::BiasedToBottom(min, max) => {
                let inner = random.next_int_bounded(max - min + 1) + 1;
                min + random.next_int_bounded(inner)
            }
            Self::VeryBiasedToBottom(min, max) => {
                let a = random.next_int_bounded(max - min + 1) + 1;
                let b = random.next_int_bounded(a) + 1;
                min + random.next_int_bounded(b)
            }
            Self::Clamped(source, min, max) => source.sample(random).clamp(*min, *max),
            Self::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => clamp_f32(normal(random, *mean, *deviation), *min as f32, *max as f32) as i32,
            Self::Trapezoid { min, max, plateau } => {
                let (min, max, plateau) = (*min, *max, *plateau);
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
            Self::WeightedList(entries) => weighted(entries, random).sample(random),
        }
    }

    pub fn min(&self) -> i32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(a, _) | Self::BiasedToBottom(a, _) | Self::VeryBiasedToBottom(a, _) => *a,
            Self::Clamped(source, a, _) => (*a).max(source.min()),
            Self::ClampedNormal { min, .. } | Self::Trapezoid { min, .. } => *min,
            Self::WeightedList(entries) => entries.iter().map(|(p, _)| p.min()).min().unwrap_or(0),
        }
    }

    pub fn max(&self) -> i32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(_, b) | Self::BiasedToBottom(_, b) | Self::VeryBiasedToBottom(_, b) => *b,
            Self::Clamped(source, _, b) => (*b).min(source.max()),
            Self::ClampedNormal { max, .. } | Self::Trapezoid { max, .. } => *max,
            Self::WeightedList(entries) => entries.iter().map(|(p, _)| p.max()).max().unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone)]
pub enum OwnedFloatProvider {
    Constant(f32),
    Uniform(f32, f32),
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: f32,
        max: f32,
    },
    Trapezoid {
        min: f32,
        max: f32,
        plateau: f32,
    },
}

impl OwnedFloatProvider {
    pub fn parse(value: &Value) -> Result<Self> {
        if let Some(v) = value.as_f64() {
            return Ok(Self::Constant(v as f32));
        }
        let what = "float provider";
        Ok(match json::qualify(json::kind(value)?).as_str() {
            "minecraft:constant" => Self::Constant(obj(value, &["value"], what)?.f32("value")?),
            "minecraft:uniform" => {
                let o = obj(value, &["min_inclusive", "max_exclusive"], what)?;
                let (min, max) = (o.f32("min_inclusive")?, o.f32("max_exclusive")?);
                if max <= min {
                    return Err(format!(
                        "Max must be larger than min, min: {min}, max: {max}"
                    ));
                }
                Self::Uniform(min, max)
            }
            "minecraft:clamped_normal" => {
                let o = obj(value, &["mean", "deviation", "min", "max"], what)?;
                let (min, max) = (o.f32("min")?, o.f32("max")?);
                if max < min {
                    return Err(format!("Max must be larger than min: [{min}, {max}]"));
                }
                Self::ClampedNormal {
                    mean: o.f32("mean")?,
                    deviation: o.f32("deviation")?,
                    min,
                    max,
                }
            }
            "minecraft:trapezoid" => {
                let o = obj(value, &["min", "max", "plateau"], what)?;
                let (min, max, plateau) = (o.f32("min")?, o.f32("max")?, o.f32("plateau")?);
                if max < min {
                    return Err(format!("Max must be larger than min: [{min}, {max}]"));
                }
                if plateau > max - min {
                    return Err(format!(
                        "Plateau can at most be the full span: [{min}, {max}]"
                    ));
                }
                Self::Trapezoid { min, max, plateau }
            }
            other => return Err(format!("unmodeled float provider {other}")),
        })
    }

    pub fn sample(&self, random: &mut impl RandomSource) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(min, max) => random_between(random, *min, *max),
            Self::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => clamp_f32(normal(random, *mean, *deviation), *min, *max),
            Self::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let start = (range - plateau) / 2.0;
                let end = range - start;
                min + random.next_float() * end + random.next_float() * start
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum OwnedHeightProvider {
    Constant(VerticalAnchor),
    Uniform(VerticalAnchor, VerticalAnchor),
    BiasedToBottom(VerticalAnchor, VerticalAnchor, i32),
    VeryBiasedToBottom(VerticalAnchor, VerticalAnchor, i32),
    Trapezoid(VerticalAnchor, VerticalAnchor, i32),
    WeightedList(Vec<(OwnedHeightProvider, i32)>),
}

impl OwnedHeightProvider {
    pub fn parse(value: &Value) -> Result<Self> {
        if value.get("type").is_none() {
            return Ok(Self::Constant(json::anchor(value)?));
        }
        let what = "height provider";
        let range = |o: &json::Obj| -> Result<(VerticalAnchor, VerticalAnchor)> {
            Ok((
                json::anchor(o.req("min_inclusive")?)?,
                json::anchor(o.req("max_inclusive")?)?,
            ))
        };
        Ok(match json::qualify(json::kind(value)?).as_str() {
            "minecraft:constant" => {
                Self::Constant(json::anchor(obj(value, &["value"], what)?.req("value")?)?)
            }
            "minecraft:uniform" => {
                let (a, b) = range(&obj(value, &["min_inclusive", "max_inclusive"], what)?)?;
                Self::Uniform(a, b)
            }
            "minecraft:biased_to_bottom" => {
                let o = obj(value, &["min_inclusive", "max_inclusive", "inner"], what)?;
                let (a, b) = range(&o)?;
                Self::BiasedToBottom(a, b, o.i32_or("inner", 1)?)
            }
            "minecraft:very_biased_to_bottom" => {
                let o = obj(value, &["min_inclusive", "max_inclusive", "inner"], what)?;
                let (a, b) = range(&o)?;
                Self::VeryBiasedToBottom(a, b, o.i32_or("inner", 1)?)
            }
            "minecraft:trapezoid" => {
                let o = obj(value, &["min_inclusive", "max_inclusive", "plateau"], what)?;
                let (a, b) = range(&o)?;
                Self::Trapezoid(a, b, o.i32_or("plateau", 0)?)
            }
            "minecraft:weighted_list" => {
                let o = obj(value, &["distribution"], what)?;
                let mut entries = Vec::new();
                for entry in o.list("distribution")? {
                    let e = obj(entry, &["data", "weight"], what)?;
                    entries.push((Self::parse(e.req("data")?)?, e.i32("weight")?));
                }
                Self::WeightedList(entries)
            }
            other => return Err(format!("unmodeled height provider {other}")),
        })
    }

    pub fn sample(&self, random: &mut impl RandomSource, span: GenContext) -> i32 {
        match self {
            Self::Constant(a) => span.resolve(*a),
            Self::Uniform(a, b) => {
                let (min, max) = (span.resolve(*a), span.resolve(*b));
                if min > max {
                    return min;
                }
                random_between_inclusive(random, min, max)
            }
            Self::BiasedToBottom(a, b, inner) => {
                let (min, max) = (span.resolve(*a), span.resolve(*b));
                if max - min - inner < 0 {
                    return min;
                }
                let limit = random.next_int_bounded(max - min - inner + 1);
                random.next_int_bounded(limit + inner) + min
            }
            Self::VeryBiasedToBottom(a, b, inner) => {
                let (min, max) = (span.resolve(*a), span.resolve(*b));
                if max - min - inner < 0 {
                    return min;
                }
                let upper = next_int(random, min + inner, max);
                let biased = next_int(random, min, upper - 1);
                next_int(random, min, biased - 1 + inner)
            }
            Self::Trapezoid(a, b, plateau) => {
                let (min, max) = (span.resolve(*a), span.resolve(*b));
                if min > max {
                    return min;
                }
                let range = max - min;
                if *plateau >= range {
                    return random_between_inclusive(random, min, max);
                }
                let start = (range - plateau) / 2;
                let end = range - start;
                min + random_between_inclusive(random, 0, end)
                    + random_between_inclusive(random, 0, start)
            }
            Self::WeightedList(entries) => weighted(entries, random).sample(random, span),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tuff_core::watchdog]
    #[test]
    fn int_providers_reject_invalid_ranges() {
        let bad = [
            json!({"type": "uniform", "min_inclusive": 5, "max_inclusive": 4}),
            json!({"type": "biased_to_bottom", "min_inclusive": 5, "max_inclusive": 4}),
            json!({"type": "clamped", "source": 1, "min_inclusive": 5, "max_inclusive": 4}),
            json!({"type": "clamped_normal", "mean": 0.0, "deviation": 1.0, "min_inclusive": 5, "max_inclusive": 4}),
            json!({"type": "trapezoid", "min": 5, "max": 4, "plateau": 0}),
            json!({"type": "trapezoid", "min": 0, "max": 4, "plateau": 5}),
        ];
        for value in bad {
            assert!(OwnedIntProvider::parse(&value).is_err(), "{value}");
        }
        let ok = json!({"type": "trapezoid", "min": 0, "max": 4, "plateau": 4});
        assert!(OwnedIntProvider::parse(&ok).is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn float_providers_reject_invalid_ranges() {
        let bad = [
            json!({"type": "uniform", "min_inclusive": 1.0, "max_exclusive": 1.0}),
            json!({"type": "clamped_normal", "mean": 0.0, "deviation": 1.0, "min": 2.0, "max": 1.0}),
            json!({"type": "trapezoid", "min": 0.0, "max": 1.0, "plateau": 2.0}),
        ];
        for value in bad {
            assert!(OwnedFloatProvider::parse(&value).is_err(), "{value}");
        }
        let ok = json!({"type": "uniform", "min_inclusive": 1.0, "max_exclusive": 2.0});
        assert!(OwnedFloatProvider::parse(&ok).is_ok());
    }
}
