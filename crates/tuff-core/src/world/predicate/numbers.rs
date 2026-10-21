//! Number providers (int and float) and the ranges built from them, as
//! loot conditions evaluate them. Integer arithmetic is checked: an
//! overflow or a division by zero makes the whole provider read 0, and a
//! float that is not finite reads 0, as the game does. A provider this
//! server cannot evaluate (an attribute it does not model, a missing
//! registry entry) or that needs a world it lacks reads `None`. Registry
//! references resolve against the built-in providers only.

use serde_json::Value;

use super::{Context, PredicateWorld, condition_in};
use crate::command::nbt_path::NbtPath;
use tuff_nbt::Tag;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// Not evaluable here.
    Unsupported,
    /// The game's arithmetic exception: the provider reads 0.
    Arithmetic,
}

type Eval<T> = Result<T, Fail>;

const SIN_SCALE: f64 = 10430.378350470453;

fn table_sin(index: i64) -> f32 {
    ((index & 65535) as f64 / SIN_SCALE).sin() as f32
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

fn long_to_int(value: i64) -> Eval<i32> {
    i32::try_from(value).map_err(|_| Fail::Arithmetic)
}

/// A storage number as the game reads it: the one numeric tag the path
/// selects.
fn stored(tag: &Tag) -> Option<(i64, f64)> {
    Some(match tag {
        Tag::Byte(v) => (i64::from(*v), f64::from(*v)),
        Tag::Short(v) => (i64::from(*v), f64::from(*v)),
        Tag::Int(v) => (i64::from(*v), f64::from(*v)),
        Tag::Long(v) => (*v, *v as f64),
        Tag::Float(v) => (*v as i64, f64::from(*v)),
        Tag::Double(v) => (*v as i64, *v),
        _ => return None,
    })
}

/// An enchantment level based value at `level`.
fn level_based(value: &Value, level: i32) -> Eval<f32> {
    if let Some(constant) = value.as_f64() {
        return Ok(constant as f32);
    }
    let float = |key: &str| {
        value[key]
            .as_f64()
            .map(|v| v as f32)
            .ok_or(Fail::Unsupported)
    };
    let nested = |key: &str| level_based(&value[key], level);
    Ok(match Numbers::kind(value)?.as_str() {
        "minecraft:constant" => float("value")?,
        "minecraft:linear" => float("base")? + float("per_level_above_first")? * (level - 1) as f32,
        "minecraft:levels_squared" => (level as f32) * (level as f32) + float("added")?,
        "minecraft:clamped" => nested("value")?.clamp(float("min")?, float("max")?),
        "minecraft:fraction" => {
            let denominator = nested("denominator")?;
            if denominator == 0.0 {
                0.0
            } else {
                nested("numerator")? / denominator
            }
        }
        "minecraft:exponent" => f64::from(nested("base")?).powf(f64::from(nested("power")?)) as f32,
        "minecraft:lookup" => {
            let values = value["values"].as_array().ok_or(Fail::Unsupported)?;
            if level >= 1 && (level as usize) <= values.len() {
                values[level as usize - 1]
                    .as_f64()
                    .ok_or(Fail::Unsupported)? as f32
            } else if level < 1 {
                return Err(Fail::Arithmetic);
            } else {
                nested("fallback")?
            }
        }
        _ => return Err(Fail::Unsupported),
    })
}

#[derive(Clone, Copy)]
pub(super) struct Numbers<'a> {
    pub context: &'a Context<'a>,
    pub world: Option<&'a dyn PredicateWorld>,
}

impl Numbers<'_> {
    fn kind(provider: &Value) -> Eval<String> {
        provider["type"]
            .as_str()
            .map(qualified)
            .ok_or(Fail::Unsupported)
    }

    fn world(&self) -> Eval<&dyn PredicateWorld> {
        self.world.ok_or(Fail::Unsupported)
    }

    /// The inputs of an aggregate: a list of providers or a single one.
    fn inputs(provider: &Value) -> Eval<Vec<&Value>> {
        match &provider["inputs"] {
            Value::Array(list) if !list.is_empty() => Ok(list.iter().collect()),
            Value::Null | Value::Array(_) | Value::String(_) => Err(Fail::Unsupported),
            single => Ok(vec![single]),
        }
    }

    fn condition(&self, condition: &Value) -> bool {
        condition_in(condition, self.context, self.world)
    }

    /// `on_true`/`on_false` of a conditional, or the matching case of a
    /// dispatcher; `None` picks the default, 0.
    fn select<'v>(&self, provider: &'v Value, kind: &str) -> Option<&'v Value> {
        match kind {
            "minecraft:conditional" => {
                if self.condition(&provider["condition"]) {
                    provider.get("on_true")
                } else {
                    provider.get("on_false")
                }
            }
            _ => provider["cases"]
                .as_array()
                .and_then(|cases| {
                    cases
                        .iter()
                        .find(|case| self.condition(&case["condition"]))
                        .and_then(|case| case.get("value"))
                })
                .or_else(|| provider.get("default")),
        }
    }

    fn weighted<'v>(&self, provider: &'v Value) -> Eval<&'v Value> {
        let entries = provider["distribution"]
            .as_array()
            .filter(|list| !list.is_empty())
            .ok_or(Fail::Unsupported)?;
        let weights: Vec<i64> = entries
            .iter()
            .map(|entry| entry["weight"].as_i64().filter(|w| *w > 0))
            .collect::<Option<_>>()
            .ok_or(Fail::Unsupported)?;
        let total: i64 = weights.iter().sum();
        let mut selection = self.world()?.random_int(0, total - 1);
        for (entry, weight) in entries.iter().zip(weights) {
            if selection < weight {
                return Ok(&entry["data"]);
            }
            selection -= weight;
        }
        Err(Fail::Unsupported)
    }

    fn storage(&self, provider: &Value) -> Eval<Option<(i64, f64)>> {
        let id = provider["storage"].as_str().ok_or(Fail::Unsupported)?;
        let path = provider["path"].as_str().ok_or(Fail::Unsupported)?;
        let path = NbtPath::from_text(path).map_err(|_| Fail::Unsupported)?;
        let root = self
            .world()?
            .storage(&qualified(id))
            .ok_or(Fail::Unsupported)?;
        let root = Tag::Compound(root);
        Ok(match path.get(&root) {
            Ok(found) if found.len() == 1 => stored(&found[0]),
            _ => None,
        })
    }

    fn int_unsafe(&self, provider: &Value) -> Eval<i32> {
        if let Some(value) = provider.as_i64() {
            return long_to_int(value);
        }
        if let Some(value) = provider.as_f64() {
            return if value.fract() == 0.0 {
                long_to_int(value as i64)
            } else {
                Err(Fail::Unsupported)
            };
        }
        if let Some(name) = provider.as_str() {
            let named = tuff_vanilla_data::provider::int_provider(&qualified(name))
                .ok_or(Fail::Unsupported)?;
            return Ok(self.run_provider(|context| named.get(context)));
        }
        let kind = Self::kind(provider)?;
        let one = |key: &str| self.int_unsafe(&provider[key]);
        let all = || -> Eval<Vec<i32>> {
            Self::inputs(provider)?
                .into_iter()
                .map(|input| self.int_unsafe(input))
                .collect()
        };
        match kind.as_str() {
            "minecraft:constant" => provider["value"]
                .as_i64()
                .ok_or(Fail::Unsupported)
                .and_then(long_to_int),
            "minecraft:uniform" => {
                let (min, max) = (one("min")?, one("max")?);
                Ok(self.world()?.random_int(i64::from(min), i64::from(max)) as i32)
            }
            "minecraft:binomial" => {
                let n = one("n")?;
                let p = self.float_unsafe(&provider["p"])?;
                let world = self.world()?;
                Ok((0..n).filter(|_| world.random() < p).count() as i32)
            }
            "minecraft:score" => {
                let world = self.world()?;
                let objective = provider["score"].as_str().ok_or(Fail::Unsupported)?;
                let holder = self.score_holder(&provider["target"]);
                match holder.and_then(|holder| world.score(&holder, objective)) {
                    Some(score) => Ok(score),
                    None => self.fallback_int(provider),
                }
            }
            "minecraft:storage" => match self.storage(provider)? {
                Some((value, _)) => Ok(value as i32),
                None => self.fallback_int(provider),
            },
            "minecraft:abs" => one("input")?.checked_abs().ok_or(Fail::Arithmetic),
            "minecraft:negate" => one("input")?.checked_neg().ok_or(Fail::Arithmetic),
            "minecraft:add" => long_to_int(all()?.into_iter().map(i64::from).sum()),
            "minecraft:mul" => long_to_int(
                all()?
                    .into_iter()
                    .fold(1i64, |product, v| product.wrapping_mul(i64::from(v))),
            ),
            "minecraft:avg" => {
                let values = all()?;
                let sum: i64 = values.iter().copied().map(i64::from).sum();
                long_to_int(sum / values.len() as i64)
            }
            "minecraft:min" => Ok(all()?.into_iter().fold(i32::MAX, i32::min)),
            "minecraft:max" => Ok(all()?.into_iter().fold(i32::MIN, i32::max)),
            "minecraft:sub" => one("left")?
                .checked_sub(one("right")?)
                .ok_or(Fail::Arithmetic),
            "minecraft:div" => one("left")?
                .checked_div(one("right")?)
                .ok_or(Fail::Arithmetic),
            "minecraft:mod" => one("left")?
                .checked_rem(one("right")?)
                .ok_or(Fail::Arithmetic),
            "minecraft:floor_div" => {
                let (left, right) = (one("left")?, one("right")?);
                if right == 0 || (left == i32::MIN && right == -1) {
                    return Err(Fail::Arithmetic);
                }
                let quotient = left / right;
                Ok(if left % right != 0 && ((left < 0) != (right < 0)) {
                    quotient - 1
                } else {
                    quotient
                })
            }
            "minecraft:floor_mod" => {
                let (left, right) = (one("left")?, one("right")?);
                if right == 0 {
                    return Err(Fail::Arithmetic);
                }
                let rem = left.wrapping_rem(right);
                Ok(if rem != 0 && (rem ^ right) < 0 {
                    rem + right
                } else {
                    rem
                })
            }
            "minecraft:pow" => {
                let (base, exponent) = (one("base")?, one("exponent")?);
                if (base == 0 && exponent == 0) || exponent < 0 {
                    return Err(Fail::Arithmetic);
                }
                base.checked_pow(exponent as u32).ok_or(Fail::Arithmetic)
            }
            "minecraft:from_float" => {
                let value = self.float_unsafe(&provider["input"])?;
                if !value.is_finite() {
                    return Err(Fail::Arithmetic);
                }
                long_to_int(value as i64)
            }
            "minecraft:conditional" | "minecraft:number_dispatcher" => {
                match self.select(provider, &kind) {
                    Some(value) => self.int_unsafe(value),
                    None => Ok(0),
                }
            }
            "minecraft:weighted_list" => self.int_unsafe(self.weighted(provider)?),
            "minecraft:environment_attribute" => Err(Fail::Unsupported),
            _ => Err(Fail::Unsupported),
        }
    }

    /// A named provider run against the block at the origin.
    fn run_provider<T>(
        &self,
        run: impl FnOnce(&mut tuff_vanilla_data::provider::Context<'_>) -> T,
    ) -> T {
        let world = self.world;
        let mut random = |bound: u32| {
            world.map_or(0, |world| {
                world.random_int(0, i64::from(bound) - 1).max(0) as u32
            })
        };
        run(&mut tuff_vanilla_data::provider::Context {
            block: self.context.location.and_then(|location| location.block),
            random: &mut random,
        })
    }

    fn attribute(&self, provider: &Value) -> Eval<Value> {
        let key = provider["attribute"].as_str().ok_or(Fail::Unsupported)?;
        self.world()?
            .attribute(&qualified(key), self.context.origin)
            .ok_or(Fail::Unsupported)
    }

    fn fallback_int(&self, provider: &Value) -> Eval<i32> {
        match provider.get("fallback") {
            Some(fallback) => self.int_unsafe(fallback),
            None => Ok(0),
        }
    }

    fn fallback_float(&self, provider: &Value) -> Eval<f32> {
        match provider.get("fallback") {
            Some(fallback) => self.float_unsafe(fallback),
            None => Ok(0.0),
        }
    }

    fn float_unsafe(&self, provider: &Value) -> Eval<f32> {
        if let Some(value) = provider.as_f64() {
            return Ok(value as f32);
        }
        if let Some(name) = provider.as_str() {
            let named = tuff_vanilla_data::provider::float_provider(&qualified(name))
                .ok_or(Fail::Unsupported)?;
            return Ok(self.run_provider(|context| named.get(context)));
        }
        let kind = Self::kind(provider)?;
        let one = |key: &str| self.float_unsafe(&provider[key]);
        let all = || -> Eval<Vec<f32>> {
            Self::inputs(provider)?
                .into_iter()
                .map(|input| self.float_unsafe(input))
                .collect()
        };
        Ok(match kind.as_str() {
            "minecraft:constant" => provider["value"].as_f64().ok_or(Fail::Unsupported)? as f32,
            "minecraft:uniform" => {
                let (min, max) = (one("min")?, one("max")?);
                if min >= max {
                    min
                } else {
                    self.world()?.random() * (max - min) + min
                }
            }
            "minecraft:storage" => match self.storage(provider)? {
                Some((_, value)) => value as f32,
                None => self.fallback_float(provider)?,
            },
            "minecraft:abs" => one("input")?.abs(),
            "minecraft:negate" => -one("input")?,
            "minecraft:ceil" => {
                let value = one("input")?;
                let truncated = value as i32;
                (if value > truncated as f32 {
                    truncated.wrapping_add(1)
                } else {
                    truncated
                }) as f32
            }
            "minecraft:floor" => one("input")?.floor(),
            "minecraft:round" => {
                let value = one("input")?;
                (value + 0.5).floor() as i32 as f32
            }
            "minecraft:truncate" => one("input")?.trunc(),
            "minecraft:sqrt" => one("input")?.sqrt(),
            "minecraft:sin" => table_sin((f64::from(one("input")?) * SIN_SCALE) as i64),
            "minecraft:cos" => table_sin((f64::from(one("input")?) * SIN_SCALE + 16384.0) as i64),
            "minecraft:add" => all()?.into_iter().fold(0.0, |sum, v| sum + v),
            "minecraft:mul" => all()?.into_iter().fold(1.0, |product, v| product * v),
            "minecraft:avg" => {
                let values = all()?;
                values.iter().fold(0.0f32, |sum, v| sum + v) / values.len() as f32
            }
            "minecraft:min" => all()?.into_iter().fold(f32::MAX, f32::min),
            "minecraft:max" => all()?.into_iter().fold(f32::MIN, f32::max),
            "minecraft:length" => all()?.into_iter().fold(0.0f32, |sum, v| sum + v * v).sqrt(),
            "minecraft:sub" => one("left")? - one("right")?,
            "minecraft:div" => one("left")? / one("right")?,
            "minecraft:mod" => {
                let right = one("right")?;
                if right == 0.0 {
                    f32::NAN
                } else {
                    one("left")? % right
                }
            }
            "minecraft:pow" => {
                let (base, exponent) = (one("base")?, one("exponent")?);
                if base == 0.0 && exponent == 0.0 {
                    f32::NAN
                } else {
                    f64::from(base).powf(f64::from(exponent)) as f32
                }
            }
            "minecraft:from_int" => self.int_unsafe(&provider["input"])? as f32,
            "minecraft:conditional" | "minecraft:number_dispatcher" => {
                match self.select(provider, &kind) {
                    Some(value) => self.float_unsafe(value)?,
                    None => 0.0,
                }
            }
            "minecraft:weighted_list" => self.float_unsafe(self.weighted(provider)?)?,
            "minecraft:enchantment_level" => {
                let level = self.context.enchantment_level.unwrap_or(0);
                level_based(&provider["amount"], level)?
            }
            "minecraft:environment_attribute" => match self.attribute(provider)? {
                Value::Number(number) => number.as_f64().ok_or(Fail::Unsupported)? as f32,
                _ => return Err(Fail::Unsupported),
            },
            _ => return Err(Fail::Unsupported),
        })
    }

    /// The provider's value as the game reads an int: 0 after an
    /// arithmetic failure.
    pub fn int(&self, provider: &Value) -> Option<i64> {
        match self.int_unsafe(provider) {
            Ok(value) => Some(i64::from(value)),
            Err(Fail::Arithmetic) => Some(0),
            Err(Fail::Unsupported) => None,
        }
    }

    /// The provider's value as the game reads a float: 0 when not finite.
    pub fn float(&self, provider: &Value) -> Option<f64> {
        match self.float_unsafe(provider) {
            Ok(value) if value.is_finite() => Some(f64::from(value)),
            Ok(_) | Err(Fail::Arithmetic) => Some(0.0),
            Err(Fail::Unsupported) => None,
        }
    }

    /// Whose score a `score` provider reads: `this` (inline or as a
    /// `context` target) or a `fixed` name.
    fn score_holder(&self, target: &Value) -> Option<String> {
        let this = || self.context.this.and_then(|this| this.holder.clone());
        let target_entity = || self.context.target.and_then(|target| target.holder.clone());
        match target {
            Value::String(target) if target == "this" => this(),
            Value::String(target) if target == "target_entity" => target_entity(),
            Value::Object(_) => match Self::kind(target).ok()?.as_str() {
                "minecraft:fixed" => target["name"].as_str().map(str::to_owned),
                "minecraft:context" if target["target"] == "this" => this(),
                "minecraft:context" if target["target"] == "target_entity" => target_entity(),
                _ => None,
            },
            _ => None,
        }
    }

    /// An exact value (a provider) or `{min, max}` of optional providers.
    pub fn int_range(&self, range: &Value, value: i64) -> bool {
        if range.is_number() || range.get("type").is_some() {
            return self.int(range) == Some(value);
        }
        let Some(edges) = range.as_object() else {
            return false;
        };
        let edge = |key: &str| edges.get(key).map(|provider| self.int(provider));
        edges.keys().all(|key| key == "min" || key == "max")
            && edge("min").is_none_or(|min| min.is_some_and(|min| value >= min))
            && edge("max").is_none_or(|max| max.is_some_and(|max| value <= max))
    }

    pub fn float_range(&self, range: &Value, value: f64) -> bool {
        if range.is_number() || range.get("type").is_some() {
            return self.float(range) == Some(value);
        }
        let Some(edges) = range.as_object() else {
            return false;
        };
        let edge = |key: &str| edges.get(key).map(|provider| self.float(provider));
        edges.keys().all(|key| key == "min" || key == "max")
            && edge("min").is_none_or(|min| min.is_some_and(|min| value >= min))
            && edge("max").is_none_or(|max| max.is_some_and(|max| value <= max))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::world::predicate::LocationFacts;
    use tuff_nbt::Compound;

    struct Fixed;

    impl PredicateWorld for Fixed {
        fn location(&self, position: [f64; 3]) -> LocationFacts {
            LocationFacts {
                position,
                ..LocationFacts::default()
            }
        }
        fn raining(&self) -> bool {
            true
        }
        fn thundering(&self) -> bool {
            false
        }
        fn clock(&self, _: &str) -> Option<i64> {
            None
        }
        fn score(&self, holder: &str, objective: &str) -> Option<i32> {
            (holder == "tgt" && objective == "o").then_some(7)
        }
        fn team(&self, _: &str) -> Option<&str> {
            None
        }
        fn random(&self) -> f32 {
            0.5
        }
        fn random_int(&self, min: i64, _: i64) -> i64 {
            min
        }
        fn storage(&self, id: &str) -> Option<Compound> {
            (id == "minecraft:s").then(|| Compound::new().with("a", 5).with("f", 2.5f64))
        }
        fn attribute(&self, key: &str, _: [f64; 3]) -> Option<Value> {
            match key {
                "minecraft:gameplay/sky_light_level" => Some(json!(7.5)),
                "minecraft:gameplay/monsters_burn" => Some(json!(true)),
                _ => None,
            }
        }
    }

    fn with_numbers<R>(f: impl FnOnce(Numbers<'_>) -> R) -> R {
        let context = Context {
            this: None,
            target: None,
            origin: [0.0; 3],
            location: None,
            tool: None,
            enchantment_level: Some(3),
        };
        f(Numbers {
            context: &context,
            world: Some(&Fixed),
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn enchantment_level_and_attribute_floats() {
        with_numbers(|n| {
            let float = |v: Value| n.float(&v);
            let level =
                |amount: Value| float(json!({"type": "enchantment_level", "amount": amount}));
            assert_eq!(
                level(json!({"type": "linear", "base": 1, "per_level_above_first": 0.5})),
                Some(2.0)
            );
            assert_eq!(
                level(json!({"type": "levels_squared", "added": 1})),
                Some(10.0)
            );
            assert_eq!(level(json!(4.5)), Some(4.5));
            assert_eq!(
                level(
                    json!({"type": "lookup", "values": [1, 2], "fallback": {"type": "linear", "base": 0, "per_level_above_first": 10}})
                ),
                Some(20.0)
            );
            assert_eq!(
                level(
                    json!({"type": "clamped", "value": {"type": "levels_squared", "added": 0}, "min": 0, "max": 5})
                ),
                Some(5.0)
            );
            assert_eq!(
                float(
                    json!({"type": "environment_attribute", "attribute": "gameplay/sky_light_level"})
                ),
                Some(7.5)
            );
            assert_eq!(
                float(
                    json!({"type": "environment_attribute", "attribute": "gameplay/monsters_burn"})
                ),
                None
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn environment_attribute_condition_compares_values() {
        with_numbers(|n| {
            let check = |attribute: &str, value: Value| {
                n.condition(&json!({"type": "environment_attribute_check", "attribute": attribute, "value": value}))
            };
            assert!(check("minecraft:gameplay/monsters_burn", json!(true)));
            assert!(!check("minecraft:gameplay/monsters_burn", json!(false)));
            assert!(check("gameplay/sky_light_level", json!(7.5)));
            assert!(!check("gameplay/unmodelled", json!(true)));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn registry_referenced_providers_resolve() {
        with_numbers(|n| {
            assert_eq!(n.int(&json!("minecraft:compostable/low")), Some(1));
            assert_eq!(n.int(&json!("minecraft:nope")), None);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn score_provider_reads_the_target_entity() {
        let target = crate::world::predicate::EntityFacts {
            holder: Some("tgt".to_owned()),
            ..Default::default()
        };
        let context = Context {
            this: None,
            target: Some(&target),
            origin: [0.0; 3],
            location: None,
            tool: None,
            enchantment_level: Some(3),
        };
        let n = Numbers {
            context: &context,
            world: Some(&Fixed),
        };
        let direct = json!({"type": "score", "target": "target_entity", "score": "o"});
        let wrapped = json!({"type": "score", "target": {"type": "context", "target": "target_entity"}, "score": "o"});
        assert_eq!(n.int(&direct), Some(7));
        assert_eq!(n.int(&wrapped), Some(7));
        let this = json!({"type": "score", "target": "this", "score": "o"});
        assert_ne!(n.int(&this), Some(7));
    }

    #[tuff_core::watchdog]
    #[test]
    fn int_providers_compute_with_checked_arithmetic() {
        with_numbers(|n| {
            let int = |v: Value| n.int(&v);
            assert_eq!(
                int(json!({"type": "add", "inputs": [1, 2, {"type": "constant", "value": 3}]})),
                Some(6)
            );
            assert_eq!(
                int(json!({"type": "minecraft:sub", "left": 1, "right": 5})),
                Some(-4)
            );
            assert_eq!(
                int(json!({"type": "mul", "inputs": [65536, 65536]})),
                Some(0)
            );
            assert_eq!(int(json!({"type": "div", "left": 7, "right": 0})), Some(0));
            assert_eq!(
                int(json!({"type": "div", "left": -7, "right": 2})),
                Some(-3)
            );
            assert_eq!(
                int(json!({"type": "floor_div", "left": -7, "right": 2})),
                Some(-4)
            );
            assert_eq!(
                int(json!({"type": "mod", "left": -7, "right": 3})),
                Some(-1)
            );
            assert_eq!(
                int(json!({"type": "floor_mod", "left": -7, "right": 3})),
                Some(2)
            );
            assert_eq!(
                int(json!({"type": "pow", "base": 2, "exponent": 10})),
                Some(1024)
            );
            assert_eq!(
                int(json!({"type": "pow", "base": 0, "exponent": 0})),
                Some(0)
            );
            assert_eq!(int(json!({"type": "abs", "input": -3})), Some(3));
            assert_eq!(
                int(json!({"type": "negate", "input": -2147483648i64})),
                Some(0)
            );
            assert_eq!(int(json!({"type": "min", "inputs": [4, -1, 9]})), Some(-1));
            assert_eq!(int(json!({"type": "max", "inputs": [4, -1, 9]})), Some(9));
            assert_eq!(int(json!({"type": "avg", "inputs": [1, 2]})), Some(1));
            assert_eq!(int(json!({"type": "from_float", "input": 2.9})), Some(2));
            assert_eq!(
                int(json!({"type": "storage", "storage": "s", "path": "a"})),
                Some(5)
            );
            assert_eq!(
                int(json!({"type": "storage", "storage": "s", "path": "b", "fallback": 4})),
                Some(4)
            );
            assert_eq!(
                int(
                    json!({"type": "conditional", "condition": {"type": "weather_check", "raining": true},
                    "on_true": 1, "on_false": 2})
                ),
                Some(1)
            );
            assert_eq!(
                int(json!({"type": "number_dispatcher", "cases": [
                    {"condition": {"type": "weather_check", "raining": false}, "value": 1}],
                    "default": 7})),
                Some(7)
            );
            assert_eq!(
                int(json!({"type": "weighted_list", "distribution": [
                    {"data": 3, "weight": 2}, {"data": 4, "weight": 1}]})),
                Some(3)
            );
            assert_eq!(
                int(json!({"type": "environment_attribute", "attribute": "x"})),
                None
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn float_providers_compute_in_single_precision() {
        with_numbers(|n| {
            let float = |v: Value| n.float(&v);
            assert_eq!(float(json!({"type": "add", "inputs": [0.5, 1]})), Some(1.5));
            assert_eq!(
                float(json!({"type": "div", "left": 1, "right": 0})),
                Some(0.0)
            );
            assert_eq!(
                float(json!({"type": "mod", "left": 5.5, "right": 2})),
                Some(1.5)
            );
            assert_eq!(float(json!({"type": "sqrt", "input": 16})), Some(4.0));
            assert_eq!(
                float(json!({"type": "length", "inputs": [3, 4]})),
                Some(5.0)
            );
            assert_eq!(float(json!({"type": "ceil", "input": 1.2})), Some(2.0));
            assert_eq!(float(json!({"type": "round", "input": -1.5})), Some(-1.0));
            assert_eq!(
                float(json!({"type": "truncate", "input": -1.7})),
                Some(-1.0)
            );
            assert_eq!(float(json!({"type": "sin", "input": 0})), Some(0.0));
            assert_eq!(float(json!({"type": "cos", "input": 0})), Some(1.0));
            assert_eq!(float(json!({"type": "from_int", "input": 3})), Some(3.0));
            assert_eq!(
                float(json!({"type": "uniform", "min": 2, "max": 4})),
                Some(3.0)
            );
            assert_eq!(
                float(json!({"type": "storage", "storage": "s", "path": "f"})),
                Some(2.5)
            );
            assert_eq!(
                float(json!({"type": "pow", "base": 0, "exponent": 0})),
                Some(0.0)
            );
        });
    }
}
