//! Context number providers from `context_int_provider/` and
//! `context_float_provider/`, emitted as static expression trees. A
//! reference to another provider file becomes a reference to its static;
//! a condition that names a stored predicate is inlined. Unknown provider
//! or condition kinds and fields fail the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::{registries, tags};
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

const INT_DIR: &str = "context_int_provider/";
const FLOAT_DIR: &str = "context_float_provider/";

/// The static identifier of the named provider in the generated code.
pub fn int_ident(name: &str) -> String {
    format!("INT_PROVIDER_{}", ident(name))
}

pub fn float_ident(name: &str) -> String {
    format!("FLOAT_PROVIDER_{}", ident(name))
}

fn ident(name: &str) -> String {
    match name.split_once(':') {
        Some((namespace, path)) if namespace != "minecraft" => {
            format!("{}_{}", shouty(namespace), shouty(path))
        }
        _ => shouty(name),
    }
}

/// Provider files of one kind, `name -> json`, sorted by name.
pub fn files<'a>(input: &'a Input, dir: &str) -> BTreeMap<String, &'a Value> {
    input
        .data_json
        .iter()
        .filter_map(|(path, json)| {
            let (namespace, rest) = path.split_once('/')?;
            let name = rest.strip_prefix(dir)?.strip_suffix(".json")?;
            Some((format!("{namespace}:{name}"), json))
        })
        .collect()
}

struct Ctx<'a> {
    input: &'a Input,
    ints: BTreeMap<String, &'a Value>,
    floats: BTreeMap<String, &'a Value>,
    blocks: BTreeMap<&'a str, String>,
    block_tags: tags::Tags,
}

pub fn generate(input: &Input) -> Result<String> {
    let ctx = Ctx {
        input,
        ints: files(input, INT_DIR),
        floats: files(input, FLOAT_DIR),
        blocks: registries::variants(&input.registries["minecraft:block"])?,
        block_tags: tags::resolved_registry_tags(input, "minecraft:block")?,
    };
    ensure!(
        !ctx.ints.is_empty(),
        "no context int providers in the data pack"
    );
    ensure!(
        !ctx.floats.is_empty(),
        "no context float providers in the data pack"
    );
    let int_idents = names::unique(ctx.ints.keys().map(String::as_str), int_ident)?;
    let float_idents = names::unique(ctx.floats.keys().map(String::as_str), float_ident)?;

    let mut out = String::new();
    for ((name, json), ident) in ctx.ints.iter().zip(&int_idents) {
        let code = ctx
            .int(json)
            .with_context(|| format!("context int provider {name}"))?;
        writeln!(
            out,
            "#[doc = \"`{name}`\"]\npub static {ident}: IntProvider = {code};"
        )?;
    }
    for ((name, json), ident) in ctx.floats.iter().zip(&float_idents) {
        let code = ctx
            .float(json)
            .with_context(|| format!("context float provider {name}"))?;
        writeln!(
            out,
            "#[doc = \"`{name}`\"]\npub static {ident}: FloatProvider = {code};"
        )?;
    }
    writeln!(
        out,
        "static INT_PROVIDERS: [(&str, &IntProvider); {}] = [",
        ctx.ints.len()
    )?;
    for (name, ident) in ctx.ints.keys().zip(&int_idents) {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "static FLOAT_PROVIDERS: [(&str, &FloatProvider); {}] = [",
        ctx.floats.len()
    )?;
    for (name, ident) in ctx.floats.keys().zip(&float_idents) {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

/// A provider value as a component field holds it: a reference to a named
/// provider file or an inline constant. Returns a `&'static IntProvider`
/// expression.
pub fn int_reference(input: &Input, value: &Value) -> Result<String> {
    match value {
        Value::String(name) => {
            ensure!(
                files(input, INT_DIR).contains_key(name),
                "unknown context int provider {name}"
            );
            Ok(format!("&crate::provider::{}", int_ident(name)))
        }
        Value::Number(_) => Ok(format!(
            "&crate::provider::IntProvider::Constant({})",
            int_constant(value)?
        )),
        _ => bail!("bad int provider reference {value}"),
    }
}

pub fn float_reference(input: &Input, value: &Value) -> Result<String> {
    match value {
        Value::String(name) => {
            ensure!(
                files(input, FLOAT_DIR).contains_key(name),
                "unknown context float provider {name}"
            );
            Ok(format!("&crate::provider::{}", float_ident(name)))
        }
        Value::Number(_) => Ok(format!(
            "&crate::provider::FloatProvider::Constant({})",
            float_constant(value)?
        )),
        _ => bail!("bad float provider reference {value}"),
    }
}

fn int_constant(value: &Value) -> Result<String> {
    let value = value
        .as_i64()
        .with_context(|| format!("expected an integer, got {value}"))?;
    let value = i32::try_from(value).context("int provider constant out of range")?;
    Ok(format!("{value}i32"))
}

fn float_constant(value: &Value) -> Result<String> {
    let value = value
        .as_f64()
        .with_context(|| format!("expected a number, got {value}"))? as f32;
    ensure!(value.is_finite(), "non-finite float");
    Ok(format!("{value:?}_f32"))
}

fn field<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a Value> {
    object.get(key).with_context(|| format!("missing {key}"))
}

fn kind(value: &Value) -> Result<(&str, &serde_json::Map<String, Value>)> {
    let object = value
        .as_object()
        .with_context(|| format!("expected an object, got {value}"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .with_context(|| format!("provider without a type: {value}"))?;
    Ok((kind, object))
}

fn check_fields(object: &serde_json::Map<String, Value>, known: &[&str]) -> Result<()> {
    for key in object.keys() {
        ensure!(
            key == "type" || known.contains(&key.as_str()),
            "unknown field {key:?}"
        );
    }
    Ok(())
}

impl Ctx<'_> {
    /// An int provider as `&'static IntProvider`: named files by reference,
    /// everything else inline.
    fn int_ref(&self, value: &Value) -> Result<String> {
        match value {
            Value::String(name) => {
                ensure!(
                    self.ints.contains_key(name),
                    "unknown context int provider {name}"
                );
                Ok(format!("&{}", int_ident(name)))
            }
            _ => Ok(format!("&{}", self.int(value)?)),
        }
    }

    fn float_ref(&self, value: &Value) -> Result<String> {
        match value {
            Value::String(name) => {
                ensure!(
                    self.floats.contains_key(name),
                    "unknown context float provider {name}"
                );
                Ok(format!("&{}", float_ident(name)))
            }
            _ => Ok(format!("&{}", self.float(value)?)),
        }
    }

    fn int(&self, value: &Value) -> Result<String> {
        if value.is_number() {
            return Ok(format!("IntProvider::Constant({})", int_constant(value)?));
        }
        let (kind, object) = kind(value)?;
        Ok(match kind {
            "minecraft:constant" => {
                check_fields(object, &["value"])?;
                format!(
                    "IntProvider::Constant({})",
                    int_constant(object.get("value").context("constant without value")?)?
                )
            }
            "minecraft:div" => {
                check_fields(object, &["left", "right"])?;
                format!(
                    "IntProvider::Div({}, {})",
                    self.int_ref(object.get("left").context("div without left")?)?,
                    self.int_ref(object.get("right").context("div without right")?)?,
                )
            }
            "minecraft:conditional" => {
                check_fields(object, &["condition", "on_true", "on_false"])?;
                let on_false = match object.get("on_false") {
                    Some(value) => self.int_ref(value)?,
                    None => "&IntProvider::Constant(0i32)".to_string(),
                };
                format!(
                    "IntProvider::Conditional {{ condition: {}, on_true: {}, on_false: {on_false} }}",
                    self.condition(
                        object
                            .get("condition")
                            .context("conditional without condition")?
                    )?,
                    self.int_ref(
                        object
                            .get("on_true")
                            .context("conditional without on_true")?
                    )?,
                )
            }
            "minecraft:number_dispatcher" => {
                check_fields(object, &["cases", "default"])?;
                let mut cases = Vec::new();
                for case in object
                    .get("cases")
                    .and_then(Value::as_array)
                    .context("dispatcher without cases")?
                {
                    let fields = case.as_object().context("bad dispatcher case")?;
                    check_fields(fields, &["condition", "value"])?;
                    cases.push(format!(
                        "({}, {})",
                        self.condition(fields.get("condition").context("case without condition")?)?,
                        self.int_ref(fields.get("value").context("case without value")?)?,
                    ));
                }
                let default = match object.get("default") {
                    Some(value) => self.int_ref(value)?,
                    None => "&IntProvider::Constant(0i32)".to_string(),
                };
                format!(
                    "IntProvider::Dispatcher {{ cases: &[{}], default: {default} }}",
                    cases.join(", ")
                )
            }
            "minecraft:weighted_list" => {
                check_fields(object, &["distribution"])?;
                let entries = self.distribution(object, |value| self.int_ref(value))?;
                format!("IntProvider::WeightedList(&[{}])", entries.join(", "))
            }
            "minecraft:abs" | "minecraft:negate" => {
                check_fields(object, &["input"])?;
                let op = if kind == "minecraft:abs" {
                    "Abs"
                } else {
                    "Negate"
                };
                format!(
                    "IntProvider::Unary(IntUnary::{op}, {})",
                    self.int_ref(field(object, "input")?)?
                )
            }
            "minecraft:sub" | "minecraft:mod" | "minecraft:floor_mod" | "minecraft:floor_div" => {
                check_fields(object, &["left", "right"])?;
                let op = match kind {
                    "minecraft:sub" => "Sub",
                    "minecraft:mod" => "Mod",
                    "minecraft:floor_mod" => "FloorMod",
                    _ => "FloorDiv",
                };
                format!(
                    "IntProvider::Binary(IntBinary::{op}, {}, {})",
                    self.int_ref(field(object, "left")?)?,
                    self.int_ref(field(object, "right")?)?
                )
            }
            "minecraft:pow" => {
                check_fields(object, &["base", "exponent"])?;
                format!(
                    "IntProvider::Binary(IntBinary::Pow, {}, {})",
                    self.int_ref(field(object, "base")?)?,
                    self.int_ref(field(object, "exponent")?)?
                )
            }
            "minecraft:add" | "minecraft:mul" | "minecraft:min" | "minecraft:max"
            | "minecraft:avg" => {
                check_fields(object, &["inputs"])?;
                let op = match kind {
                    "minecraft:add" => "Sum",
                    "minecraft:mul" => "Product",
                    "minecraft:min" => "Min",
                    "minecraft:max" => "Max",
                    _ => "Avg",
                };
                let inputs = self.aggregate(object, |value| self.int_ref(value))?;
                format!("IntProvider::Aggregate(IntAggregate::{op}, &[{inputs}])")
            }
            "minecraft:uniform" => {
                check_fields(object, &["min", "max"])?;
                format!(
                    "IntProvider::Uniform({}, {})",
                    self.int_ref(field(object, "min")?)?,
                    self.int_ref(field(object, "max")?)?
                )
            }
            "minecraft:binomial" => {
                check_fields(object, &["n", "p"])?;
                format!(
                    "IntProvider::Binomial({}, {})",
                    self.int_ref(field(object, "n")?)?,
                    self.float_ref(field(object, "p")?)?
                )
            }
            "minecraft:from_float" => {
                check_fields(object, &["input"])?;
                format!(
                    "IntProvider::FromFloat({})",
                    self.float_ref(field(object, "input")?)?
                )
            }
            other => bail!("unknown context int provider kind {other}"),
        })
    }

    fn float(&self, value: &Value) -> Result<String> {
        if value.is_number() {
            return Ok(format!(
                "FloatProvider::Constant({})",
                float_constant(value)?
            ));
        }
        let (kind, object) = kind(value)?;
        Ok(match kind {
            "minecraft:constant" => {
                check_fields(object, &["value"])?;
                format!(
                    "FloatProvider::Constant({})",
                    float_constant(object.get("value").context("constant without value")?)?
                )
            }
            "minecraft:div" => {
                check_fields(object, &["left", "right"])?;
                format!(
                    "FloatProvider::Div({}, {})",
                    self.float_ref(object.get("left").context("div without left")?)?,
                    self.float_ref(object.get("right").context("div without right")?)?,
                )
            }
            "minecraft:conditional" => {
                check_fields(object, &["condition", "on_true", "on_false"])?;
                let on_false = match object.get("on_false") {
                    Some(value) => self.float_ref(value)?,
                    None => "&FloatProvider::Constant(0.0_f32)".to_string(),
                };
                format!(
                    "FloatProvider::Conditional {{ condition: {}, on_true: {}, on_false: {on_false} }}",
                    self.condition(
                        object
                            .get("condition")
                            .context("conditional without condition")?
                    )?,
                    self.float_ref(
                        object
                            .get("on_true")
                            .context("conditional without on_true")?
                    )?,
                )
            }
            "minecraft:number_dispatcher" => {
                check_fields(object, &["cases", "default"])?;
                let mut cases = Vec::new();
                for case in object
                    .get("cases")
                    .and_then(Value::as_array)
                    .context("dispatcher without cases")?
                {
                    let fields = case.as_object().context("bad dispatcher case")?;
                    check_fields(fields, &["condition", "value"])?;
                    cases.push(format!(
                        "({}, {})",
                        self.condition(fields.get("condition").context("case without condition")?)?,
                        self.float_ref(fields.get("value").context("case without value")?)?,
                    ));
                }
                let default = match object.get("default") {
                    Some(value) => self.float_ref(value)?,
                    None => "&FloatProvider::Constant(0.0_f32)".to_string(),
                };
                format!(
                    "FloatProvider::Dispatcher {{ cases: &[{}], default: {default} }}",
                    cases.join(", ")
                )
            }
            "minecraft:weighted_list" => {
                check_fields(object, &["distribution"])?;
                let entries = self.distribution(object, |value| self.float_ref(value))?;
                format!("FloatProvider::WeightedList(&[{}])", entries.join(", "))
            }
            "minecraft:abs" | "minecraft:ceil" | "minecraft:floor" | "minecraft:round"
            | "minecraft:truncate" | "minecraft:negate" | "minecraft:sin" | "minecraft:cos"
            | "minecraft:sqrt" => {
                check_fields(object, &["input"])?;
                let op = match kind {
                    "minecraft:abs" => "Abs",
                    "minecraft:ceil" => "Ceil",
                    "minecraft:floor" => "Floor",
                    "minecraft:round" => "Round",
                    "minecraft:truncate" => "Truncate",
                    "minecraft:negate" => "Negate",
                    "minecraft:sin" => "Sin",
                    "minecraft:cos" => "Cos",
                    _ => "Sqrt",
                };
                format!(
                    "FloatProvider::Unary(FloatUnary::{op}, {})",
                    self.float_ref(field(object, "input")?)?
                )
            }
            "minecraft:sub" | "minecraft:mod" => {
                check_fields(object, &["left", "right"])?;
                let op = if kind == "minecraft:sub" {
                    "Sub"
                } else {
                    "Mod"
                };
                format!(
                    "FloatProvider::Binary(FloatBinary::{op}, {}, {})",
                    self.float_ref(field(object, "left")?)?,
                    self.float_ref(field(object, "right")?)?
                )
            }
            "minecraft:pow" => {
                check_fields(object, &["base", "exponent"])?;
                format!(
                    "FloatProvider::Binary(FloatBinary::Pow, {}, {})",
                    self.float_ref(field(object, "base")?)?,
                    self.float_ref(field(object, "exponent")?)?
                )
            }
            "minecraft:add" | "minecraft:mul" | "minecraft:min" | "minecraft:max"
            | "minecraft:avg" | "minecraft:length" => {
                check_fields(object, &["inputs"])?;
                let op = match kind {
                    "minecraft:add" => "Sum",
                    "minecraft:mul" => "Product",
                    "minecraft:min" => "Min",
                    "minecraft:max" => "Max",
                    "minecraft:avg" => "Avg",
                    _ => "Length",
                };
                let inputs = self.aggregate(object, |value| self.float_ref(value))?;
                format!("FloatProvider::Aggregate(FloatAggregate::{op}, &[{inputs}])")
            }
            "minecraft:uniform" => {
                check_fields(object, &["min", "max"])?;
                format!(
                    "FloatProvider::Uniform({}, {})",
                    self.float_ref(field(object, "min")?)?,
                    self.float_ref(field(object, "max")?)?
                )
            }
            "minecraft:from_int" => {
                check_fields(object, &["input"])?;
                format!(
                    "FloatProvider::FromInt({})",
                    self.int_ref(field(object, "input")?)?
                )
            }
            other => bail!("unknown context float provider kind {other}"),
        })
    }

    fn aggregate(
        &self,
        object: &serde_json::Map<String, Value>,
        value: impl Fn(&Value) -> Result<String>,
    ) -> Result<String> {
        let inputs = match field(object, "inputs")? {
            Value::Array(inputs) => inputs.iter().collect::<Vec<_>>(),
            single => vec![single],
        };
        ensure!(!inputs.is_empty(), "aggregate without inputs");
        let inputs: Vec<String> = inputs.into_iter().map(value).collect::<Result<_>>()?;
        Ok(inputs.join(", "))
    }

    fn distribution(
        &self,
        object: &serde_json::Map<String, Value>,
        value: impl Fn(&Value) -> Result<String>,
    ) -> Result<Vec<String>> {
        let mut entries = Vec::new();
        for entry in object
            .get("distribution")
            .and_then(Value::as_array)
            .context("weighted list without distribution")?
        {
            let fields = entry.as_object().context("bad weighted entry")?;
            check_fields(fields, &["data", "weight"])?;
            let weight = match fields.get("weight") {
                None => 1,
                Some(weight) => weight.as_u64().context("bad weight")?,
            };
            entries.push(format!(
                "({}, {weight}u32)",
                value(fields.get("data").context("weighted entry without data")?)?
            ));
        }
        ensure!(!entries.is_empty(), "empty weighted list");
        Ok(entries)
    }

    /// A condition as `&'static Condition`; a bare name is a stored
    /// predicate file, inlined.
    fn condition(&self, value: &Value) -> Result<String> {
        let value = match value {
            Value::String(name) => self.input.predicate(name)?,
            _ => value,
        };
        let (kind, object) = kind(value)?;
        Ok(match kind {
            "minecraft:match_block" => {
                check_fields(object, &["blocks", "state"])?;
                let mut names = Vec::new();
                block_list(
                    object.get("blocks").context("match_block without blocks")?,
                    &self.block_tags,
                    &mut names,
                )?;
                let mut blocks: Vec<String> = names
                    .iter()
                    .map(|name| {
                        self.blocks
                            .get(name.as_str())
                            .map(|variant| format!("Block::{variant}"))
                            .with_context(|| format!("unknown block {name}"))
                    })
                    .collect::<Result<_>>()?;
                blocks.sort();
                blocks.dedup();
                let mut state = Vec::new();
                if let Some(properties) = object.get("state") {
                    for (property, expected) in properties
                        .as_object()
                        .context("match_block state is not an object")?
                    {
                        let expected = expected
                            .as_str()
                            .with_context(|| format!("state {property} is not an exact value"))?;
                        state.push(format!(
                            "({}, {})",
                            str_literal(property),
                            str_literal(expected)
                        ));
                    }
                }
                format!(
                    "&Condition::MatchBlock {{ blocks: &[{}], state: &[{}] }}",
                    blocks.join(", "),
                    state.join(", ")
                )
            }
            other => bail!("unknown provider condition kind {other}"),
        })
    }
}

fn block_list(value: &Value, tags: &tags::Tags, out: &mut Vec<String>) -> Result<()> {
    match value {
        Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => out.extend(
                tags.get(tag)
                    .with_context(|| format!("unknown block tag {entry}"))?
                    .iter()
                    .cloned(),
            ),
            None => out.push(entry.clone()),
        },
        Value::Array(entries) => {
            for entry in entries {
                block_list(entry, tags, out)?;
            }
        }
        _ => bail!("bad block list {value}"),
    }
    Ok(())
}
