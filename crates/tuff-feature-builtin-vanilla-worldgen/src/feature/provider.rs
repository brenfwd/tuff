//! Block state providers: what block a feature places at a position.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::worldgen::IntProvider;
use tuff_vanilla_data::{Block, BlockState};

use crate::feature::Registry;
use crate::feature::level::Region;
use crate::feature::predicate::BlockPredicate;
use crate::json::{self, Result, obj};
use crate::noise::NoiseStack;
use crate::normal_noise::{NoiseParameters, NormalNoise, Normalization};
use tuff_item::random::{LegacyRandom, Random, RandomSource};
use tuff_world_model::BlockPos;

pub enum StateProvider {
    Simple(BlockState),
    Weighted(Vec<(BlockState, i32)>),
    Rotated(Arc<StateProvider>, Option<tuff_vanilla_data::block::Facing>),
    RandomizedInt {
        source: Arc<StateProvider>,
        property: String,
        values: Box<crate::feature::OwnedIntProvider>,
    },
    RandomBlock(Vec<Block>),
    RuleBased {
        fallback: Option<Arc<StateProvider>>,
        rules: Vec<(BlockPredicate, Arc<StateProvider>)>,
    },
    CopyProperties(Arc<StateProvider>),
    Noise {
        noise: NoiseStack,
        scale: f32,
        states: Vec<BlockState>,
    },
    DualNoise {
        noise: NoiseStack,
        scale: f32,
        states: Vec<BlockState>,
        variety: (i32, i32),
        slow: NoiseStack,
        slow_scale: f32,
    },
    NoiseThreshold {
        noise: NoiseStack,
        scale: f32,
        threshold: f32,
        high_chance: f32,
        default: BlockState,
        low: Vec<BlockState>,
        high: Vec<BlockState>,
    },
}

pub fn noise_parameters(value: &Value) -> Result<NormalNoise> {
    let o = obj(
        value,
        &[
            "base_amplitude",
            "base_octave",
            "octave_count",
            "normalize",
            "amplitude_modifiers",
        ],
        "noise parameters",
    )?;
    let normalize = match o.get("normalize") {
        None | Some(Value::Bool(true)) => Normalization::Enabled,
        Some(Value::Bool(false)) => Normalization::Disabled,
        Some(Value::String(s)) if s == "legacy" => Normalization::Legacy,
        Some(other) => return Err(format!("bad normalization {other}")),
    };
    let modifiers = match o.get("amplitude_modifiers") {
        None => Vec::new(),
        Some(list) => list
            .as_array()
            .ok_or("amplitude modifiers are a list")?
            .iter()
            .map(|v| v.as_f64().ok_or("amplitudes are numbers"))
            .collect::<std::result::Result<_, _>>()?,
    };
    Ok(NormalNoise::new(NoiseParameters {
        base_amplitude: o.f64_or("base_amplitude", 1.0)?,
        base_octave: o.i32("base_octave")?,
        octave_count: o.i32_or("octave_count", 1)?,
        normalize,
        amplitude_modifiers: modifiers,
    }))
}

fn seeded_noise(parameters: &NormalNoise, seed: i64) -> NoiseStack {
    parameters.create(&mut Random::Legacy(LegacyRandom::new(seed)))
}

fn states(value: &Value) -> Result<Vec<BlockState>> {
    value
        .as_array()
        .ok_or("states are a list")?
        .iter()
        .map(json::block_state)
        .collect()
}

impl StateProvider {
    /// Parses a provider: a registry name, a plain block state, or a typed
    /// provider.
    pub fn parse(registry: &mut Registry, value: &Value) -> Result<Arc<Self>> {
        if let Some(name) = value.as_str() {
            return registry.state_provider(&json::qualify(name));
        }
        if value.get("type").is_none() {
            return Ok(Arc::new(Self::Simple(json::block_state(value)?)));
        }
        let kind = json::qualify(json::kind(value)?);
        let what = "state provider";
        let provider = match kind.as_str() {
            "minecraft:simple" => Self::Simple(json::block_state(
                obj(value, &["state"], what)?.req("state")?,
            )?),
            "minecraft:weighted" => {
                let o = obj(value, &["entries"], what)?;
                let mut entries = Vec::new();
                for entry in o.list("entries")? {
                    let e = obj(entry, &["data", "weight"], what)?;
                    entries.push((json::block_state(e.req("data")?)?, e.i32("weight")?));
                }
                if entries.is_empty() {
                    return Err("weighted providers need entries".into());
                }
                Self::Weighted(entries)
            }
            "minecraft:rotated" => {
                let o = obj(value, &["state", "direction"], what)?;
                let direction = match o.get("direction") {
                    Some(v) => Some(super::predicate::parse_direction(
                        v.as_str().ok_or("direction")?,
                    )?),
                    None => None,
                };
                Self::Rotated(Self::parse(registry, o.req("state")?)?, direction)
            }
            "minecraft:randomized_int" => {
                let o = obj(value, &["source", "property", "values"], what)?;
                Self::RandomizedInt {
                    source: Self::parse(registry, o.req("source")?)?,
                    property: o.str("property")?.to_string(),
                    values: Box::new(super::OwnedIntProvider::parse(o.req("values")?)?),
                }
            }
            "minecraft:random_block" => {
                Self::RandomBlock(json::blocks(obj(value, &["blocks"], what)?.req("blocks")?)?)
            }
            "minecraft:rule_based" => {
                let o = obj(value, &["fallback", "rules"], what)?;
                let fallback = match o.get("fallback") {
                    Some(v) => Some(Self::parse(registry, v)?),
                    None => None,
                };
                let mut rules = Vec::new();
                for rule in o.list("rules")? {
                    let r = obj(rule, &["if_true", "then"], what)?;
                    rules.push((
                        BlockPredicate::parse(r.req("if_true")?)?,
                        Self::parse(registry, r.req("then")?)?,
                    ));
                }
                Self::RuleBased { fallback, rules }
            }
            "minecraft:copy_properties" => Self::CopyProperties(Self::parse(
                registry,
                obj(value, &["source"], what)?.req("source")?,
            )?),
            "minecraft:noise_provider" | "minecraft:noise" => {
                let o = obj(value, &["seed", "noise", "scale", "states"], what)?;
                let parameters = noise_parameters(o.req("noise")?)?;
                Self::Noise {
                    noise: seeded_noise(&parameters, o.req("seed")?.as_i64().ok_or("seed")?),
                    scale: o.f32("scale")?,
                    states: states(o.req("states")?)?,
                }
            }
            "minecraft:dual_noise" => {
                let o = obj(
                    value,
                    &[
                        "seed",
                        "noise",
                        "scale",
                        "states",
                        "variety",
                        "slow_noise",
                        "slow_scale",
                    ],
                    what,
                )?;
                let seed = o.req("seed")?.as_i64().ok_or("seed")?;
                let variety = o.list("variety")?;
                Self::DualNoise {
                    noise: seeded_noise(&noise_parameters(o.req("noise")?)?, seed),
                    scale: o.f32("scale")?,
                    states: states(o.req("states")?)?,
                    variety: (
                        variety.first().and_then(Value::as_i64).ok_or("variety")? as i32,
                        variety.get(1).and_then(Value::as_i64).ok_or("variety")? as i32,
                    ),
                    slow: seeded_noise(&noise_parameters(o.req("slow_noise")?)?, seed),
                    slow_scale: o.f32("slow_scale")?,
                }
            }
            "minecraft:noise_threshold" => {
                let o = obj(
                    value,
                    &[
                        "seed",
                        "noise",
                        "scale",
                        "threshold",
                        "high_chance",
                        "default_state",
                        "low_states",
                        "high_states",
                    ],
                    what,
                )?;
                Self::NoiseThreshold {
                    noise: seeded_noise(
                        &noise_parameters(o.req("noise")?)?,
                        o.req("seed")?.as_i64().ok_or("seed")?,
                    ),
                    scale: o.f32("scale")?,
                    threshold: o.f32("threshold")?,
                    high_chance: o.f32("high_chance")?,
                    default: json::block_state(o.req("default_state")?)?,
                    low: states(o.req("low_states")?)?,
                    high: states(o.req("high_states")?)?,
                }
            }
            other => return Err(format!("unmodeled state provider {other}")),
        };
        Ok(Arc::new(provider))
    }

    pub fn state(
        &self,
        level: &mut Region,
        random: &mut impl RandomSource,
        pos: BlockPos,
    ) -> BlockState {
        match self.optional_state(level, random, pos) {
            Some(state) => state,
            None => level.block(pos),
        }
    }

    pub fn optional_state(
        &self,
        level: &mut Region,
        random: &mut impl RandomSource,
        pos: BlockPos,
    ) -> Option<BlockState> {
        Some(match self {
            Self::Simple(state) => *state,
            Self::Weighted(entries) => {
                let total: i32 = entries.iter().map(|(_, w)| w).sum();
                let mut selection = random.next_int_bounded(total);
                let mut chosen = entries[0].0;
                for (state, weight) in entries {
                    selection -= weight;
                    if selection < 0 {
                        chosen = *state;
                        break;
                    }
                }
                chosen
            }
            Self::Rotated(inner, direction) => {
                let facing = match direction {
                    Some(d) => *d,
                    None => random_direction(random),
                };
                let state = inner.state(level, random, pos);
                let axis = match facing.axis() {
                    tuff_vanilla_data::block::Axis::X => "x",
                    tuff_vanilla_data::block::Axis::Y => "y",
                    tuff_vanilla_data::block::Axis::Z => "z",
                };
                let state = state.with("axis", axis).unwrap_or(state);

                state.with("facing", facing.name()).unwrap_or(state)
            }
            Self::RandomizedInt {
                source,
                property,
                values,
            } => {
                let state = source.state(level, random, pos);
                match state.get(property) {
                    Some(_) => {
                        let value = values.sample(random);
                        state.with(property, &value.to_string()).unwrap_or(state)
                    }
                    None => state,
                }
            }
            Self::RandomBlock(blocks) => {
                if blocks.is_empty() {
                    return None;
                }
                blocks[random.next_int_bounded(blocks.len() as i32) as usize].default_state()
            }
            Self::RuleBased { fallback, rules } => {
                for (predicate, then) in rules {
                    if predicate.test(level, pos)
                        && let Some(state) = then.optional_state(level, random, pos)
                    {
                        return Some(state);
                    }
                }
                return fallback.as_ref()?.optional_state(level, random, pos);
            }
            Self::CopyProperties(source) => {
                let state = source.state(level, random, pos);
                copy_properties(state, level.block(pos))
            }
            Self::Noise {
                noise,
                scale,
                states,
            } => {
                let value = noise_value(noise, pos, *scale as f64);
                pick_by_noise(states, value)
            }
            Self::DualNoise {
                noise,
                scale,
                states,
                variety,
                slow,
                slow_scale,
            } => {
                let slow_value = |p: BlockPos| {
                    slow.get(
                        (p.x as f32 * slow_scale) as f64,
                        (p.y as f32 * slow_scale) as f64,
                        (p.z as f32 * slow_scale) as f64,
                    )
                };
                let variety_noise = slow_value(pos) as f64;
                let local = clamped_map(
                    variety_noise,
                    -1.0,
                    1.0,
                    variety.0 as f64,
                    (variety.1 + 1) as f64,
                ) as i32;
                let mut possible = Vec::with_capacity(local.max(0) as usize);
                for i in 0..local {
                    let p = BlockPos::new(pos.x + i * 54545, pos.y, pos.z + i * 34234);
                    possible.push(pick_by_noise(states, slow_value(p)));
                }
                pick_by_noise(&possible, noise_value(noise, pos, *scale as f64))
            }
            Self::NoiseThreshold {
                noise,
                scale,
                threshold,
                high_chance,
                default,
                low,
                high,
            } => {
                let value = noise_value(noise, pos, *scale as f64) as f64;
                if value < *threshold as f64 {
                    low[random.next_int_bounded(low.len() as i32) as usize]
                } else if random.next_float() < *high_chance {
                    high[random.next_int_bounded(high.len() as i32) as usize]
                } else {
                    *default
                }
            }
        })
    }
}

pub fn clamped_map(v: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let t = (v - from_min) / (from_max - from_min);
    if t < 0.0 {
        to_min
    } else if t > 1.0 {
        to_max
    } else {
        to_min + t * (to_max - to_min)
    }
}

fn noise_value(noise: &NoiseStack, pos: BlockPos, scale: f64) -> f32 {
    noise.get(
        pos.x as f64 * scale,
        pos.y as f64 * scale,
        pos.z as f64 * scale,
    )
}

fn pick_by_noise(states: &[BlockState], value: f32) -> BlockState {
    let placement = tuff_core::mth::clamp_f32((1.0 + value) / 2.0, 0.0, 0.9999);
    states[(placement * states.len() as f32) as usize]
}

/// The game's `Direction.getRandom`: down, up, north, south, west, east.
pub fn random_direction(random: &mut impl RandomSource) -> tuff_vanilla_data::block::Facing {
    use tuff_vanilla_data::block::Facing;
    [
        Facing::Down,
        Facing::Up,
        Facing::North,
        Facing::South,
        Facing::West,
        Facing::East,
    ][random.next_int_bounded(6) as usize]
}

/// `target` with each property it shares with `source` copied over.
pub fn copy_properties(target: BlockState, source: BlockState) -> BlockState {
    let mut state = target;
    for (name, value) in source.properties() {
        if let Some(next) = state.with(name, value) {
            state = next;
        }
    }
    state
}

pub type IntProviderData = &'static IntProvider;
