//! Context number providers from the data pack (`context_int_provider/`,
//! `context_float_provider/`): small expression trees such as a fuel's burn
//! time, which the game evaluates against the block using it.

use crate::block::BlockState;
use crate::registry::Block;

/// What a provider is evaluated against.
pub struct Context<'a> {
    /// The block state in question, e.g. the furnace burning a fuel.
    pub block: Option<BlockState>,
    /// `random(bound)`: a uniform integer in `0..bound`.
    pub random: &'a mut dyn FnMut(u32) -> u32,
}

#[derive(Debug)]
pub enum Condition {
    /// The block is one of `blocks` and has every listed property value.
    MatchBlock {
        blocks: &'static [Block],
        state: &'static [(&'static str, &'static str)],
    },
}

impl Condition {
    pub fn test(&self, context: &Context<'_>) -> bool {
        match self {
            Self::MatchBlock { blocks, state } => context.block.is_some_and(|block| {
                blocks.contains(&block.block())
                    && state
                        .iter()
                        .all(|(property, value)| block.get(property) == Some(*value))
            }),
        }
    }
}

#[derive(Debug)]
pub enum IntProvider {
    Constant(i32),
    /// Integer division, truncating toward zero.
    Div(&'static IntProvider, &'static IntProvider),
    Conditional {
        condition: &'static Condition,
        on_true: &'static IntProvider,
        on_false: &'static IntProvider,
    },
    /// The value of the first case whose condition holds, else `default`.
    Dispatcher {
        cases: &'static [(&'static Condition, &'static IntProvider)],
        default: &'static IntProvider,
    },
    /// One entry picked by weight.
    WeightedList(&'static [(&'static IntProvider, u32)]),
    Unary(IntUnary, &'static IntProvider),
    Binary(IntBinary, &'static IntProvider, &'static IntProvider),
    Aggregate(IntAggregate, &'static [&'static IntProvider]),
    /// A uniform integer in `min..=max`.
    Uniform(&'static IntProvider, &'static IntProvider),
    /// How many of `n` trials succeed with probability `p`.
    Binomial(&'static IntProvider, &'static FloatProvider),
    FromFloat(&'static FloatProvider),
}

#[derive(Clone, Copy, Debug)]
pub enum IntUnary {
    Abs,
    Negate,
}

#[derive(Clone, Copy, Debug)]
pub enum IntBinary {
    Sub,
    Mod,
    FloorMod,
    FloorDiv,
    Pow,
}

#[derive(Clone, Copy, Debug)]
pub enum IntAggregate {
    Sum,
    Product,
    Min,
    Max,
    Avg,
}

fn saturate(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn unit_random(context: &mut Context<'_>) -> f32 {
    const STEPS: u32 = 1 << 24;
    (context.random)(STEPS) as f32 / STEPS as f32
}

impl IntProvider {
    pub fn get(&self, context: &mut Context<'_>) -> i32 {
        match self {
            Self::Constant(value) => *value,
            Self::Div(left, right) => {
                let left = left.get(context);
                let right = right.get(context);
                left.checked_div(right).unwrap_or(0)
            }
            Self::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                if condition.test(context) {
                    on_true.get(context)
                } else {
                    on_false.get(context)
                }
            }
            Self::Dispatcher { cases, default } => cases
                .iter()
                .find(|(condition, _)| condition.test(context))
                .map_or(default, |(_, value)| value)
                .get(context),
            Self::WeightedList(entries) => weighted(entries, context).get(context),
            Self::Unary(op, input) => {
                let value = input.get(context);
                match op {
                    IntUnary::Abs => value.saturating_abs(),
                    IntUnary::Negate => value.saturating_neg(),
                }
            }
            Self::Binary(op, left, right) => {
                let left = left.get(context);
                let right = right.get(context);
                match op {
                    IntBinary::Sub => left.saturating_sub(right),
                    IntBinary::Mod => left.checked_rem(right).unwrap_or(0),
                    IntBinary::FloorMod => left.checked_rem_euclid(right).map_or(0, |rem| {
                        if right < 0 && rem != 0 {
                            rem + right
                        } else {
                            rem
                        }
                    }),
                    IntBinary::FloorDiv => left.checked_div(right).map_or(0, |quotient| {
                        if (left % right != 0) && ((left < 0) != (right < 0)) {
                            quotient - 1
                        } else {
                            quotient
                        }
                    }),
                    IntBinary::Pow => {
                        if left == 0 && right == 0 {
                            0
                        } else {
                            u32::try_from(right)
                                .ok()
                                .and_then(|exponent| left.checked_pow(exponent))
                                .unwrap_or(0)
                        }
                    }
                }
            }
            Self::Aggregate(op, inputs) => {
                let values = inputs.iter().map(|input| i64::from(input.get(context)));
                match op {
                    IntAggregate::Sum => saturate(values.sum()),
                    IntAggregate::Product => {
                        saturate(values.fold(1i64, |product, value| product.saturating_mul(value)))
                    }
                    IntAggregate::Min => values.min().map_or(0, saturate),
                    IntAggregate::Max => values.max().map_or(0, saturate),
                    IntAggregate::Avg => {
                        let count = inputs.len().max(1) as i64;
                        saturate(values.sum::<i64>() / count)
                    }
                }
            }
            Self::Uniform(min, max) => {
                let (min, max) = (min.get(context), max.get(context));
                if min >= max {
                    min
                } else {
                    let span = (i64::from(max) - i64::from(min) + 1) as u32;
                    min.saturating_add((context.random)(span) as i32)
                }
            }
            Self::Binomial(trials, chance) => {
                let trials = trials.get(context);
                let chance = chance.get(context);
                (0..trials.max(0))
                    .filter(|_| unit_random(context) < chance)
                    .count() as i32
            }
            Self::FromFloat(input) => input.get(context) as i32,
        }
    }
}

#[derive(Debug)]
pub enum FloatProvider {
    Constant(f32),
    Div(&'static FloatProvider, &'static FloatProvider),
    Conditional {
        condition: &'static Condition,
        on_true: &'static FloatProvider,
        on_false: &'static FloatProvider,
    },
    Dispatcher {
        cases: &'static [(&'static Condition, &'static FloatProvider)],
        default: &'static FloatProvider,
    },
    WeightedList(&'static [(&'static FloatProvider, u32)]),
    Unary(FloatUnary, &'static FloatProvider),
    Binary(FloatBinary, &'static FloatProvider, &'static FloatProvider),
    Aggregate(FloatAggregate, &'static [&'static FloatProvider]),
    Uniform(&'static FloatProvider, &'static FloatProvider),
    FromInt(&'static IntProvider),
}

#[derive(Clone, Copy, Debug)]
pub enum FloatUnary {
    Abs,
    Ceil,
    Floor,
    Round,
    Truncate,
    Negate,
    Sin,
    Cos,
    Sqrt,
}

#[derive(Clone, Copy, Debug)]
pub enum FloatBinary {
    Sub,
    Mod,
    Pow,
}

#[derive(Clone, Copy, Debug)]
pub enum FloatAggregate {
    Sum,
    Product,
    Min,
    Max,
    Avg,
    Length,
}

impl FloatProvider {
    pub fn get(&self, context: &mut Context<'_>) -> f32 {
        match self {
            Self::Constant(value) => *value,
            Self::Div(left, right) => left.get(context) / right.get(context),
            Self::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                if condition.test(context) {
                    on_true.get(context)
                } else {
                    on_false.get(context)
                }
            }
            Self::Dispatcher { cases, default } => cases
                .iter()
                .find(|(condition, _)| condition.test(context))
                .map_or(default, |(_, value)| value)
                .get(context),
            Self::WeightedList(entries) => weighted(entries, context).get(context),
            Self::Unary(op, input) => {
                let value = input.get(context);
                match op {
                    FloatUnary::Abs => value.abs(),
                    FloatUnary::Ceil => value.ceil(),
                    FloatUnary::Floor => value.floor(),
                    FloatUnary::Round => (value + 0.5).floor(),
                    FloatUnary::Truncate => value.trunc(),
                    FloatUnary::Negate => -value,
                    FloatUnary::Sin => value.sin(),
                    FloatUnary::Cos => value.cos(),
                    FloatUnary::Sqrt => value.sqrt(),
                }
            }
            Self::Binary(op, left, right) => {
                let left = left.get(context);
                let right = right.get(context);
                match op {
                    FloatBinary::Sub => left - right,
                    FloatBinary::Mod if right == 0.0 => f32::NAN,
                    FloatBinary::Mod => left % right,
                    FloatBinary::Pow if left == 0.0 && right == 0.0 => f32::NAN,
                    FloatBinary::Pow => left.powf(right),
                }
            }
            Self::Aggregate(op, inputs) => {
                let values = inputs.iter().map(|input| input.get(context));
                match op {
                    FloatAggregate::Sum => values.sum(),
                    FloatAggregate::Product => values.product(),
                    FloatAggregate::Min => values.fold(f32::MAX, f32::min),
                    FloatAggregate::Max => values.fold(f32::MIN, f32::max),
                    FloatAggregate::Avg => values.sum::<f32>() / inputs.len() as f32,
                    FloatAggregate::Length => values.map(|value| value * value).sum::<f32>().sqrt(),
                }
            }
            Self::Uniform(min, max) => {
                let (min, max) = (min.get(context), max.get(context));
                if min >= max {
                    min
                } else {
                    unit_random(context) * (max - min) + min
                }
            }
            Self::FromInt(input) => input.get(context) as f32,
        }
    }
}

fn weighted<'a, T>(entries: &'a [(&'a T, u32)], context: &mut Context<'_>) -> &'a T {
    let total: u32 = entries.iter().map(|(_, weight)| *weight).sum();
    let mut roll = (context.random)(total.max(1));
    for (value, weight) in entries {
        if roll < *weight {
            return value;
        }
        roll -= weight;
    }
    entries.last().expect("a weighted list is never empty").0
}

/// Named providers, sorted by name.
pub fn int_provider(name: &str) -> Option<&'static IntProvider> {
    crate::lookup(&INT_PROVIDERS, name)
}

pub fn float_provider(name: &str) -> Option<&'static FloatProvider> {
    crate::lookup(&FLOAT_PROVIDERS, name)
}

include!(concat!(env!("OUT_DIR"), "/providers.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    fn int(provider: &IntProvider, block: Option<BlockState>, roll: u32) -> i32 {
        let mut random = move |bound: u32| roll % bound;
        provider.get(&mut Context {
            block,
            random: &mut random,
        })
    }

    fn float(provider: &FloatProvider, block: Option<BlockState>) -> f32 {
        let mut random = |_: u32| 0;
        provider.get(&mut Context {
            block,
            random: &mut random,
        })
    }

    #[test]
    fn cooking_times_halve_in_fast_cooking_blocks() {
        let coal = int_provider("minecraft:cooking/time_coal").unwrap();
        let furnace = Some(Block::Furnace.default_state());
        let blast = Some(Block::BlastFurnace.default_state());
        let smoker = Some(Block::Smoker.default_state());
        assert_eq!(int(coal, furnace, 0), 1600);
        assert_eq!(int(coal, blast, 0), 800);
        assert_eq!(int(coal, smoker, 0), 800);
        assert_eq!(int(coal, None, 0), 1600);
        let kelp = int_provider("cooking/time_dried_kelp_block").unwrap();
        assert_eq!(int(kelp, furnace, 0), 4001);
        assert_eq!(int(kelp, blast, 0), 2000);
        let speed = float_provider("minecraft:cooking/speed_default").unwrap();
        assert_eq!(float(speed, furnace), 1.0);
        assert_eq!(float(speed, blast), 2.0);
        assert!(int_provider("minecraft:cooking/nothing").is_none());
    }

    #[test]
    fn compostable_dispatchers_read_the_composter_level() {
        let low = int_provider("minecraft:compostable/low").unwrap();
        let empty = Block::Composter.default_state();
        assert_eq!(int(low, Some(empty), 99), 1);
        let full = Some(empty.with("level", "3").unwrap());
        let rolls: Vec<i32> = [0, 29, 30, 99]
            .into_iter()
            .map(|roll| int(low, full, roll))
            .collect();
        assert_eq!(rolls, vec![1, 1, 0, 0]);
    }

    static TWO: IntProvider = IntProvider::Constant(2);
    static FIVE: IntProvider = IntProvider::Constant(5);
    static NEG_SEVEN: IntProvider = IntProvider::Constant(-7);
    static HALF: FloatProvider = FloatProvider::Constant(0.5);
    static NINE: FloatProvider = FloatProvider::Constant(9.0);
    static THREE_F: FloatProvider = FloatProvider::Constant(3.0);

    #[test]
    fn integer_math_providers_evaluate() {
        let abs = IntProvider::Unary(IntUnary::Abs, &NEG_SEVEN);
        assert_eq!(int(&abs, None, 0), 7);
        let floor_mod = IntProvider::Binary(IntBinary::FloorMod, &NEG_SEVEN, &FIVE);
        assert_eq!(int(&floor_mod, None, 0), 3);
        let floor_div = IntProvider::Binary(IntBinary::FloorDiv, &NEG_SEVEN, &TWO);
        assert_eq!(int(&floor_div, None, 0), -4);
        let rem = IntProvider::Binary(IntBinary::Mod, &NEG_SEVEN, &FIVE);
        assert_eq!(int(&rem, None, 0), -2);
        let pow = IntProvider::Binary(IntBinary::Pow, &FIVE, &TWO);
        assert_eq!(int(&pow, None, 0), 25);
        static SUM: IntProvider =
            IntProvider::Aggregate(IntAggregate::Sum, &[&TWO, &FIVE, &NEG_SEVEN]);
        let sum = &SUM;
        assert_eq!(int(sum, None, 0), 0);
        static AVG: IntProvider = IntProvider::Aggregate(IntAggregate::Avg, &[&TWO, &FIVE]);
        let avg = &AVG;
        assert_eq!(int(avg, None, 0), 3);
        let uniform = IntProvider::Uniform(&TWO, &FIVE);
        assert_eq!(int(&uniform, None, 3), 5);
        let binomial = IntProvider::Binomial(&FIVE, &HALF);
        assert!((0..=5).contains(&int(&binomial, None, 7)));
    }

    #[test]
    fn float_math_providers_evaluate() {
        let sqrt = FloatProvider::Unary(FloatUnary::Sqrt, &NINE);
        assert_eq!(float(&sqrt, None), 3.0);
        let round = FloatProvider::Unary(FloatUnary::Round, &HALF);
        assert_eq!(float(&round, None), 1.0);
        let pow = FloatProvider::Binary(FloatBinary::Pow, &THREE_F, &THREE_F);
        assert_eq!(float(&pow, None), 27.0);
        assert!(
            float(
                &FloatProvider::Binary(FloatBinary::Mod, &NINE, &FloatProvider::Constant(0.0)),
                None
            )
            .is_nan()
        );
        static FOUR: FloatProvider = FloatProvider::Constant(4.0);
        static LENGTH: FloatProvider =
            FloatProvider::Aggregate(FloatAggregate::Length, &[&THREE_F, &FOUR]);
        let length = &LENGTH;
        assert_eq!(float(length, None), 5.0);
        let from_int = FloatProvider::FromInt(&FIVE);
        assert_eq!(float(&from_int, None), 5.0);
    }
}
