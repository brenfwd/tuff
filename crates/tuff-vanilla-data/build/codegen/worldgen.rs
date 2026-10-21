//! `worldgen/noise`, `density_function`, `noise_settings`, `material_rule`,
//! `material_condition` and the multi-noise parameter list reports. Every
//! object is checked field by field and an unknown kind or field fails the
//! build. References to named entries are resolved here, so a missing name
//! also fails the build.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use crate::input::Input;
use crate::names::{shouty, str_literal};

struct Gen<'a> {
    input: &'a Input,
    noises: BTreeMap<String, &'a Value>,
    functions: BTreeMap<String, &'a Value>,
    rules: BTreeMap<String, &'a Value>,
    conditions: BTreeMap<String, &'a Value>,
    settings: BTreeMap<String, &'a Value>,
}

fn collect<'a>(input: &'a Input, dir: &str) -> BTreeMap<String, &'a Value> {
    let mut out = BTreeMap::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        if let Some(name) = rest
            .strip_prefix(dir)
            .and_then(|r| r.strip_prefix('/'))
            .and_then(|r| r.strip_suffix(".json"))
        {
            out.insert(format!("{namespace}:{name}"), json);
        }
    }
    out
}

/// The built-in pack's noise, density function, material rule and
/// condition, and noise settings files, by path below `data/`, for tests
/// that decode them at run time.
pub fn pack_json(input: &Input) -> BTreeMap<&str, &Value> {
    const DIRS: [&str; 7] = [
        "/worldgen/noise/",
        "/worldgen/density_function/",
        "/worldgen/material_rule/",
        "/worldgen/material_condition/",
        "/worldgen/noise_settings/",
        "/worldgen/structure/",
        "/worldgen/structure_set/",
    ];
    input
        .data_json
        .iter()
        .filter(|(path, _)| DIRS.iter().any(|dir| path.contains(dir)))
        .map(|(path, json)| (path.as_str(), json))
        .collect()
}

fn qualify(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

pub fn f32_lit(v: f32) -> String {
    if v.is_infinite() {
        return if v > 0.0 {
            "f32::INFINITY".into()
        } else {
            "f32::NEG_INFINITY".into()
        };
    }
    format!("{v:?}f32")
}

pub fn f64_lit(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 {
            "f64::INFINITY".into()
        } else {
            "f64::NEG_INFINITY".into()
        };
    }
    format!("{v:?}f64")
}

/// A JSON object whose keys must all be in `allowed`.
pub fn object<'v>(value: &'v Value, allowed: &[&str]) -> Result<&'v Map<String, Value>> {
    let map = value.as_object().context("expected an object")?;
    for key in map.keys() {
        ensure!(allowed.contains(&key.as_str()), "unknown field {key}");
    }
    Ok(map)
}

pub fn num(map: &Map<String, Value>, key: &str) -> Result<f64> {
    map.get(key)
        .and_then(Value::as_f64)
        .with_context(|| format!("missing number {key}"))
}

pub fn int(map: &Map<String, Value>, key: &str) -> Result<i64> {
    map.get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("missing integer {key}"))
}

pub fn boolean(map: &Map<String, Value>, key: &str) -> Result<bool> {
    map.get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("missing boolean {key}"))
}

pub fn string<'v>(map: &'v Map<String, Value>, key: &str) -> Result<&'v str> {
    map.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string {key}"))
}

pub fn field<'v>(map: &'v Map<String, Value>, key: &str) -> Result<&'v Value> {
    map.get(key).with_context(|| format!("missing {key}"))
}

/// A block's default state as a `BlockState` literal.
pub fn block_state(input: &Input, value: &Value) -> Result<String> {
    let name = qualify(value.as_str().context("block states are block names")?);
    let block = input
        .blocks
        .get(&name)
        .with_context(|| format!("unknown block {name}"))?;
    let state = block
        .states
        .iter()
        .find(|s| s.default)
        .with_context(|| format!("{name} has no default state"))?;
    Ok(format!("crate::BlockState::raw({})", state.id))
}

pub fn quantize(v: f64) -> i64 {
    ((v as f32) * 10000.0f32) as i64
}

pub fn climate_parameter(value: &Value) -> Result<String> {
    let (min, max) = match value {
        Value::Number(n) => {
            let v = n.as_f64().context("bad number")?;
            (v, v)
        }
        Value::Array(pair) => {
            ensure!(pair.len() == 2, "climate ranges have two ends");
            (
                pair[0].as_f64().context("bad min")?,
                pair[1].as_f64().context("bad max")?,
            )
        }
        Value::Object(map) => {
            ensure!(map.len() == 2, "climate ranges have min and max only");
            (num(map, "min")?, num(map, "max")?)
        }
        _ => bail!("bad climate parameter"),
    };
    ensure!(min as f32 <= max as f32, "climate range min > max");
    Ok(format!(
        "ClimateParameter {{ min: {}, max: {} }}",
        quantize(min),
        quantize(max)
    ))
}

impl Gen<'_> {
    fn noise_ref(&self, value: &Value) -> Result<String> {
        let name = qualify(value.as_str().context("noises are referenced by name")?);
        ensure!(self.noises.contains_key(&name), "unknown noise {name}");
        Ok(format!("&NOISE_{}", shouty(&name)))
    }

    fn function_ref(&self, name: &str) -> Result<String> {
        let name = qualify(name);
        ensure!(
            self.functions.contains_key(&name),
            "unknown density function {name}"
        );
        Ok(format!("&DF_{}", shouty(&name)))
    }

    fn df_child(&self, value: &Value) -> Result<String> {
        Ok(format!("&{}", self.df(value)?))
    }

    fn df(&self, value: &Value) -> Result<String> {
        match value {
            Value::Number(n) => Ok(format!(
                "DensityFunction::Constant({})",
                f32_lit(n.as_f64().context("bad constant")? as f32)
            )),
            Value::String(name) => Ok(format!(
                "DensityFunction::Reference({})",
                self.function_ref(name)?
            )),
            Value::Object(map) => {
                let kind = string(map, "type")?;
                self.df_object(kind, value)
                    .with_context(|| format!("density function {kind}"))
            }
            _ => bail!("bad density function"),
        }
    }

    fn df_object(&self, kind: &str, value: &Value) -> Result<String> {
        let unary = |op: &str| -> Result<String> {
            let m = object(value, &["type", "input"])?;
            Ok(format!(
                "DensityFunction::Unary(UnaryOp::{op}, {})",
                self.df_child(field(m, "input")?)?
            ))
        };
        let binary = |op: &str| -> Result<String> {
            let m = object(value, &["type", "left", "right"])?;
            Ok(format!(
                "DensityFunction::Binary(BinaryOp::{op}, {}, {})",
                self.df_child(field(m, "left")?)?,
                self.df_child(field(m, "right")?)?
            ))
        };
        let simple = |variant: &str| -> Result<String> {
            object(value, &["type"])?;
            Ok(format!("DensityFunction::{variant}"))
        };
        let axis = |m: &Map<String, Value>| -> Result<&'static str> {
            Ok(match string(m, "axis")? {
                "x" => "Axis::X",
                "y" => "Axis::Y",
                "z" => "Axis::Z",
                other => bail!("unknown axis {other}"),
            })
        };
        Ok(match kind {
            "minecraft:constant" => {
                let m = object(value, &["type", "argument"])?;
                format!(
                    "DensityFunction::Constant({})",
                    f32_lit(num(m, "argument")? as f32)
                )
            }
            "minecraft:abs" => unary("Abs")?,
            "minecraft:square" => unary("Square")?,
            "minecraft:cube" => unary("Cube")?,
            "minecraft:half_negative" => unary("HalfNegative")?,
            "minecraft:quarter_negative" => unary("QuarterNegative")?,
            "minecraft:negate" => unary("Negate")?,
            "minecraft:squeeze" => unary("Squeeze")?,
            "minecraft:sqrt" => unary("Sqrt")?,
            "minecraft:reciprocal" => unary("Reciprocal")?,
            "minecraft:log" => unary("Log")?,
            "minecraft:sign" => unary("Sign")?,
            "minecraft:pow" => {
                let m = object(value, &["type", "base", "exponent"])?;
                format!(
                    "DensityFunction::Binary(BinaryOp::Pow, {}, {})",
                    self.df_child(field(m, "base")?)?,
                    self.df_child(field(m, "exponent")?)?
                )
            }
            "minecraft:floor" | "minecraft:round" | "minecraft:ceil" | "minecraft:truncate" => {
                let m = object(value, &["type", "input", "multiple"])?;
                let op = match kind {
                    "minecraft:floor" => "Floor",
                    "minecraft:round" => "Round",
                    "minecraft:ceil" => "Ceil",
                    _ => "Truncate",
                };
                let multiple = match m.get("multiple") {
                    Some(v) => self.df_child(v)?,
                    None => "&DensityFunction::Constant(1.0f32)".into(),
                };
                format!(
                    "DensityFunction::Binary(BinaryOp::{op}, {}, {multiple})",
                    self.df_child(field(m, "input")?)?
                )
            }
            "minecraft:add" => binary("Add")?,
            "minecraft:sub" => binary("Sub")?,
            "minecraft:mul" => binary("Mul")?,
            "minecraft:div" => binary("Div")?,
            "minecraft:min" => binary("Min")?,
            "minecraft:max" => binary("Max")?,
            "minecraft:blend_alpha" => simple("BlendAlpha")?,
            "minecraft:blend_offset" => simple("BlendOffset")?,
            "minecraft:beardifier" => simple("Beardifier")?,
            "minecraft:end_outer_islands" => simple("EndIslands")?,
            "minecraft:noise" => {
                let m = object(
                    value,
                    &[
                        "type", "noise", "xz_scale", "y_scale", "shift_x", "shift_y", "shift_z",
                    ],
                )?;
                let shift = |key: &str| -> Result<String> {
                    match m.get(key) {
                        Some(v) => self.df_child(v),
                        None => Ok("&DensityFunction::Constant(0.0f32)".into()),
                    }
                };
                format!(
                    "DensityFunction::Noise {{ noise: {}, xz_scale: {}, y_scale: {}, shift_x: {}, shift_y: {}, shift_z: {} }}",
                    self.noise_ref(field(m, "noise")?)?,
                    f64_lit(num(m, "xz_scale")?),
                    f64_lit(num(m, "y_scale")?),
                    shift("shift_x")?,
                    shift("shift_y")?,
                    shift("shift_z")?
                )
            }
            "minecraft:shift_a" | "minecraft:shift_b" | "minecraft:shift" => {
                let m = object(value, &["type", "argument"])
                    .or_else(|_| object(value, &["type", "noise"]))?;
                let noise = m
                    .get("argument")
                    .or_else(|| m.get("noise"))
                    .context("missing noise")?;
                let variant = match kind {
                    "minecraft:shift_a" => "ShiftA",
                    "minecraft:shift_b" => "ShiftB",
                    _ => "Shift",
                };
                format!("DensityFunction::{variant}({})", self.noise_ref(noise)?)
            }
            "minecraft:distance_to_point" => {
                let m = object(value, &["type", "point", "metric"])?;
                let point = field(m, "point")?.as_array().context("point is a list")?;
                ensure!(point.len() == 3, "points have three coordinates");
                let coords: Vec<i64> = point
                    .iter()
                    .map(|v| v.as_i64().context("integer coordinate"))
                    .collect::<Result<_>>()?;
                let metric = match string(m, "metric")? {
                    "euclidean" => "Euclidean",
                    "euclidean_squared" => "EuclideanSquared",
                    "manhattan" => "Manhattan",
                    "chebyshev" => "Chebyshev",
                    other => bail!("unknown metric {other}"),
                };
                format!(
                    "DensityFunction::DistanceToPoint {{ point: [{}, {}, {}], metric: DistanceMetric::{metric} }}",
                    coords[0], coords[1], coords[2]
                )
            }
            "minecraft:gradient" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "axis",
                        "tiling",
                        "from_coordinate",
                        "to_coordinate",
                        "from_value",
                        "to_value",
                    ],
                )?;
                let tiling = match m.get("tiling").and_then(Value::as_str) {
                    None | Some("clamp_to_edge") => "ClampToEdge",
                    Some("repeat") => "Repeat",
                    Some("mirrored_repeat") => "MirroredRepeat",
                    Some(other) => bail!("unknown tiling {other}"),
                };
                let from = int(m, "from_coordinate")?;
                let to = int(m, "to_coordinate")?;
                ensure!(from != to, "gradient coordinates must differ");
                format!(
                    "DensityFunction::Gradient {{ axis: {}, tiling: Tiling::{tiling}, from_coordinate: {from}, to_coordinate: {to}, from_value: {}, to_value: {} }}",
                    axis(m)?,
                    f32_lit(num(m, "from_value")? as f32),
                    f32_lit(num(m, "to_value")? as f32)
                )
            }
            "minecraft:spline" => {
                let m = object(value, &["type", "spline"])?;
                format!(
                    "DensityFunction::Spline(&{})",
                    self.spline(field(m, "spline")?)?
                )
            }
            "minecraft:lerp" => {
                let m = object(value, &["type", "alpha", "first", "second"])?;
                format!(
                    "DensityFunction::Lerp {{ alpha: {}, first: {}, second: {} }}",
                    self.df_child(field(m, "alpha")?)?,
                    self.df_child(field(m, "first")?)?,
                    self.df_child(field(m, "second")?)?
                )
            }
            "minecraft:clamp" => {
                let m = object(value, &["type", "input", "min", "max"])?;
                let (min, max) = (num(m, "min")? as f32, num(m, "max")? as f32);
                ensure!(min <= max, "clamp min > max");
                format!(
                    "DensityFunction::Clamp {{ input: {}, min: {}, max: {} }}",
                    self.df_child(field(m, "input")?)?,
                    f32_lit(min),
                    f32_lit(max)
                )
            }
            "minecraft:range_choice" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "input",
                        "min_inclusive",
                        "max_exclusive",
                        "when_in_range",
                        "when_out_of_range",
                    ],
                )?;
                format!(
                    "DensityFunction::RangeChoice {{ input: {}, min_inclusive: {}, max_exclusive: {}, when_in_range: {}, when_out_of_range: {} }}",
                    self.df_child(field(m, "input")?)?,
                    f32_lit(num(m, "min_inclusive")? as f32),
                    f32_lit(num(m, "max_exclusive")? as f32),
                    self.df_child(field(m, "when_in_range")?)?,
                    self.df_child(field(m, "when_out_of_range")?)?
                )
            }
            "minecraft:interval_select" => {
                let m = object(value, &["type", "input", "thresholds", "functions"])?;
                let thresholds: Vec<f32> = field(m, "thresholds")?
                    .as_array()
                    .context("thresholds is a list")?
                    .iter()
                    .map(|v| v.as_f64().map(|v| v as f32).context("threshold"))
                    .collect::<Result<_>>()?;
                let functions = field(m, "functions")?
                    .as_array()
                    .context("functions is a list")?;
                ensure!(
                    thresholds.len() + 1 == functions.len(),
                    "interval_select needs one more function than thresholds"
                );
                ensure!(
                    thresholds.windows(2).all(|w| w[0] <= w[1]),
                    "interval_select thresholds must be sorted"
                );
                let functions: Vec<String> = functions
                    .iter()
                    .map(|f| self.df(f))
                    .collect::<Result<_>>()?;
                format!(
                    "DensityFunction::IntervalSelect {{ input: {}, thresholds: &[{}], functions: &[{}] }}",
                    self.df_child(field(m, "input")?)?,
                    thresholds
                        .iter()
                        .map(|t| f32_lit(*t))
                        .collect::<Vec<_>>()
                        .join(", "),
                    functions.join(", ")
                )
            }
            "minecraft:cache" => {
                let m = object(value, &["type", "input"])?;
                format!(
                    "DensityFunction::Cache({})",
                    self.df_child(field(m, "input")?)?
                )
            }
            "minecraft:blend_density" => {
                let m = object(value, &["type", "input"])?;
                format!(
                    "DensityFunction::BlendDensity({})",
                    self.df_child(field(m, "input")?)?
                )
            }
            "minecraft:interpolated" => {
                let m = object(value, &["type", "input", "cell_size_xz", "cell_size_y"])?;
                let (xz, y) = (int(m, "cell_size_xz")?, int(m, "cell_size_y")?);
                ensure!(xz > 0 && y > 0, "cell sizes are positive");
                format!(
                    "DensityFunction::Interpolated {{ input: {}, cell_xz: {xz}, cell_y: {y} }}",
                    self.df_child(field(m, "input")?)?
                )
            }
            "minecraft:slice" => {
                let m = object(value, &["type", "axis", "coordinate", "input"])?;
                format!(
                    "DensityFunction::Slice {{ axis: {}, coordinate: {}, input: {} }}",
                    axis(m)?,
                    int(m, "coordinate")?,
                    self.df_child(field(m, "input")?)?
                )
            }
            "minecraft:find_top_surface" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "density",
                        "upper_bound",
                        "lower_bound",
                        "cell_height",
                    ],
                )?;
                format!(
                    "DensityFunction::FindTopSurface {{ density: {}, upper_bound: {}, lower_bound: {}, cell_height: {} }}",
                    self.df_child(field(m, "density")?)?,
                    self.df_child(field(m, "upper_bound")?)?,
                    int(m, "lower_bound")?,
                    int(m, "cell_height")?
                )
            }
            "minecraft:old_blended_noise" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "xz_scale",
                        "y_scale",
                        "xz_factor",
                        "y_factor",
                        "smear_scale_multiplier",
                    ],
                )?;
                format!(
                    "DensityFunction::OldBlendedNoise {{ xz_scale: {}, y_scale: {}, xz_factor: {}, y_factor: {}, smear_scale_multiplier: {} }}",
                    f64_lit(num(m, "xz_scale")?),
                    f64_lit(num(m, "y_scale")?),
                    f64_lit(num(m, "xz_factor")?),
                    f64_lit(num(m, "y_factor")?),
                    f64_lit(num(m, "smear_scale_multiplier")?)
                )
            }
            other => bail!("unmodeled density function type {other}"),
        })
    }

    fn spline(&self, value: &Value) -> Result<String> {
        if let Some(v) = value.as_f64() {
            return Ok(format!("Spline::Constant({})", f32_lit(v as f32)));
        }
        let m = object(value, &["coordinate", "points"])?;
        let points = field(m, "points")?.as_array().context("points is a list")?;
        ensure!(!points.is_empty(), "splines need points");
        let mut out = Vec::new();
        let mut last = f32::NEG_INFINITY;
        for point in points {
            let p = object(point, &["location", "value", "derivative"])?;
            let location = num(p, "location")? as f32;
            ensure!(location > last, "spline points must ascend");
            last = location;
            out.push(format!(
                "SplinePoint {{ location: {}, value: {}, derivative: {} }}",
                f32_lit(location),
                self.spline(field(p, "value")?)?,
                f32_lit(num(p, "derivative")? as f32)
            ));
        }
        Ok(format!(
            "Spline::Multipoint {{ coordinate: {}, points: &[{}] }}",
            self.df_child(field(m, "coordinate")?)?,
            out.join(", ")
        ))
    }

    fn anchor(&self, value: &Value) -> Result<String> {
        let m = value.as_object().context("anchors are objects")?;
        ensure!(m.len() == 1, "anchors have one field");
        let (key, v) = m.iter().next().unwrap();
        let v = v.as_i64().context("anchor offsets are integers")?;
        Ok(match key.as_str() {
            "absolute" => format!("VerticalAnchor::Absolute({v})"),
            "above_bottom" => format!("VerticalAnchor::AboveBottom({v})"),
            "below_top" => format!("VerticalAnchor::BelowTop({v})"),
            "relative_to_sea_level" => format!("VerticalAnchor::RelativeToSeaLevel({v})"),
            other => bail!("unknown anchor {other}"),
        })
    }

    fn biomes(&self, value: &Value) -> Result<String> {
        let names: Vec<String> = match value {
            Value::String(name) if name.starts_with('#') => {
                let tag = qualify(&name[1..]);
                let tags =
                    super::tags::resolved_registry_tags(self.input, "minecraft:worldgen/biome")?;
                tags.get(&tag)
                    .with_context(|| format!("unknown biome tag {tag}"))?
                    .clone()
            }
            Value::String(name) => vec![qualify(name)],
            Value::Array(list) => list
                .iter()
                .map(|v| v.as_str().map(qualify).context("biome names"))
                .collect::<Result<_>>()?,
            _ => bail!("bad biome set"),
        };
        for name in &names {
            ensure!(
                self.input.data_json.contains_key(&format!(
                    "{}/worldgen/biome/{}.json",
                    name.split_once(':').unwrap().0,
                    name.split_once(':').unwrap().1
                )),
                "unknown biome {name}"
            );
        }
        Ok(format!(
            "&[{}]",
            names
                .iter()
                .map(|n| str_literal(n))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    fn condition(&self, value: &Value) -> Result<String> {
        if let Some(name) = value.as_str() {
            let name = qualify(name);
            ensure!(
                self.conditions.contains_key(&name),
                "unknown material condition {name}"
            );
            return Ok(format!(
                "MaterialCondition::Reference(&MC_{})",
                shouty(&name)
            ));
        }
        let m = value.as_object().context("conditions are objects")?;
        let kind = string(m, "type")?;
        Ok(match kind {
            "minecraft:biome" => {
                let m = object(value, &["type", "biome_is"])?;
                format!(
                    "MaterialCondition::Biome({})",
                    self.biomes(field(m, "biome_is")?)?
                )
            }
            "minecraft:noise_threshold" => {
                let m = object(
                    value,
                    &["type", "noise", "min_threshold", "max_threshold", "is_3d"],
                )?;
                format!(
                    "MaterialCondition::NoiseThreshold {{ noise: {}, min_threshold: {}, max_threshold: {}, is_3d: {} }}",
                    self.noise_ref(field(m, "noise")?)?,
                    f64_lit(num(m, "min_threshold")?),
                    f64_lit(num(m, "max_threshold")?),
                    m.get("is_3d").and_then(Value::as_bool).unwrap_or(false)
                )
            }
            "minecraft:vertical_gradient" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "random_name",
                        "true_at_and_below",
                        "false_at_and_above",
                    ],
                )?;
                format!(
                    "MaterialCondition::VerticalGradient {{ random_name: {}, true_at_and_below: {}, false_at_and_above: {} }}",
                    str_literal(&qualify(string(m, "random_name")?)),
                    self.anchor(field(m, "true_at_and_below")?)?,
                    self.anchor(field(m, "false_at_and_above")?)?
                )
            }
            "minecraft:y_above" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "anchor",
                        "surface_depth_multiplier",
                        "add_stone_depth",
                    ],
                )?;
                format!(
                    "MaterialCondition::YAbove {{ anchor: {}, surface_depth_multiplier: {}, add_stone_depth: {} }}",
                    self.anchor(field(m, "anchor")?)?,
                    int(m, "surface_depth_multiplier")?,
                    boolean(m, "add_stone_depth")?
                )
            }
            "minecraft:water" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "offset",
                        "surface_depth_multiplier",
                        "add_stone_depth",
                    ],
                )?;
                format!(
                    "MaterialCondition::Water {{ offset: {}, surface_depth_multiplier: {}, add_stone_depth: {} }}",
                    int(m, "offset")?,
                    int(m, "surface_depth_multiplier")?,
                    boolean(m, "add_stone_depth")?
                )
            }
            "minecraft:stone_depth" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "offset",
                        "add_surface_depth",
                        "secondary_depth_range",
                        "surface_type",
                    ],
                )?;
                let surface = match string(m, "surface_type")? {
                    "floor" => "Floor",
                    "ceiling" => "Ceiling",
                    other => bail!("unknown surface type {other}"),
                };
                format!(
                    "MaterialCondition::StoneDepth {{ offset: {}, add_surface_depth: {}, secondary_depth_range: {}, surface_type: CaveSurface::{surface} }}",
                    int(m, "offset")?,
                    boolean(m, "add_surface_depth")?,
                    int(m, "secondary_depth_range")?
                )
            }
            "minecraft:not" => {
                let m = object(value, &["type", "invert"])?;
                format!(
                    "MaterialCondition::Not(&{})",
                    self.condition(field(m, "invert")?)?
                )
            }
            "minecraft:steep"
            | "minecraft:hole"
            | "minecraft:above_preliminary_surface"
            | "minecraft:temperature" => {
                object(value, &["type"])?;
                match kind {
                    "minecraft:steep" => "MaterialCondition::Steep".into(),
                    "minecraft:hole" => "MaterialCondition::Hole".into(),
                    "minecraft:temperature" => "MaterialCondition::Temperature".into(),
                    _ => "MaterialCondition::AbovePreliminarySurface".into(),
                }
            }
            other => bail!("unmodeled material condition {other}"),
        })
    }

    fn rule(&self, value: &Value) -> Result<String> {
        if let Some(name) = value.as_str() {
            let name = qualify(name);
            ensure!(
                self.rules.contains_key(&name),
                "unknown material rule {name}"
            );
            return Ok(format!("MaterialRule::Reference(&MR_{})", shouty(&name)));
        }
        let m = value.as_object().context("rules are objects")?;
        let kind = string(m, "type")?;
        Ok(match kind {
            "minecraft:block" => {
                let m = object(value, &["type", "result_state"])?;
                format!(
                    "MaterialRule::Block({})",
                    block_state(self.input, field(m, "result_state")?)?
                )
            }
            "minecraft:sequence" => {
                let m = object(value, &["type", "sequence"])?;
                let rules: Vec<String> = field(m, "sequence")?
                    .as_array()
                    .context("sequence is a list")?
                    .iter()
                    .map(|r| self.rule(r))
                    .collect::<Result<_>>()?;
                format!("MaterialRule::Sequence(&[{}])", rules.join(", "))
            }
            "minecraft:condition" => {
                let m = object(value, &["type", "if_true", "then_run"])?;
                format!(
                    "MaterialRule::Condition {{ if_true: &{}, then_run: &{} }}",
                    self.condition(field(m, "if_true")?)?,
                    self.rule(field(m, "then_run")?)?
                )
            }
            "minecraft:bandlands" => {
                object(value, &["type"])?;
                "MaterialRule::Bandlands".into()
            }
            "minecraft:ore_vein" => {
                let m = object(
                    value,
                    &[
                        "type",
                        "ore_block",
                        "raw_ore_block",
                        "filler_block",
                        "raw_ore_chance",
                        "density",
                        "richness",
                        "filler_gap",
                    ],
                )?;
                format!(
                    "MaterialRule::OreVein {{ ore_block: {}, raw_ore_block: {}, filler_block: {}, raw_ore_chance: {}, density: {}, richness: {}, filler_gap: {} }}",
                    block_state(self.input, field(m, "ore_block")?)?,
                    block_state(self.input, field(m, "raw_ore_block")?)?,
                    block_state(self.input, field(m, "filler_block")?)?,
                    f32_lit(num(m, "raw_ore_chance")? as f32),
                    self.df_child(field(m, "density")?)?,
                    self.df_child(field(m, "richness")?)?,
                    self.df_child(field(m, "filler_gap")?)?
                )
            }
            other => bail!("unmodeled material rule {other}"),
        })
    }

    fn noise(&self, value: &Value) -> Result<String> {
        let m = object(
            value,
            &[
                "base_amplitude",
                "base_octave",
                "octave_count",
                "normalize",
                "amplitude_modifiers",
            ],
        )?;
        let count = m.get("octave_count").and_then(Value::as_i64).unwrap_or(1);
        ensure!((1..=32).contains(&count), "octave count out of range");
        let base_octave = int(m, "base_octave")?;
        ensure!(
            (-32..=32).contains(&base_octave),
            "base octave out of range"
        );
        let normalize = match m.get("normalize") {
            None | Some(Value::Bool(true)) => "Enabled",
            Some(Value::Bool(false)) => "Disabled",
            Some(Value::String(s)) if s == "legacy" => "Legacy",
            Some(other) => bail!("bad normalization {other}"),
        };
        let modifiers: Vec<f64> = match m.get("amplitude_modifiers") {
            None => Vec::new(),
            Some(list) => list
                .as_array()
                .context("amplitude modifiers are a list")?
                .iter()
                .map(|v| v.as_f64().context("amplitude"))
                .collect::<Result<_>>()?,
        };
        ensure!(
            modifiers.is_empty() || modifiers.len() as i64 == count,
            "amplitude modifiers must match the octave count"
        );
        Ok(format!(
            "base_octave: {base_octave}, base_amplitude: {}, octave_count: {count}, normalize: Normalize::{normalize}, amplitude_modifiers: &[{}]",
            f64_lit(
                m.get("base_amplitude")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
            ),
            modifiers
                .iter()
                .map(|v| f64_lit(*v))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    fn settings(&self, value: &Value) -> Result<String> {
        let m = object(
            value,
            &[
                "noise",
                "default_block",
                "default_fluid",
                "noise_router",
                "material_rule",
                "spawn_target",
                "sea_level",
                "disable_mob_generation",
                "aquifers",
                "legacy_random_source",
                "debug_functions",
            ],
        )?;
        let noise = object(field(m, "noise")?, &["min_y", "height"])?;
        let router_fields = [
            "temperature",
            "vegetation",
            "continents",
            "erosion",
            "depth",
            "ridges",
            "chunk_surface_level",
            "final_density",
        ];
        let router = object(field(m, "noise_router")?, &router_fields)?;
        let mut router_out = String::new();
        for key in router_fields {
            write!(
                router_out,
                "{key}: {}, ",
                self.df_child(field(router, key)?)?
            )?;
        }
        let aquifer_fields = [
            "barrier",
            "fluid_level_floodedness",
            "fluid_level_spread",
            "lava",
            "exclusion",
            "surface_level",
        ];
        let aquifers = match m.get("aquifers") {
            None => "None".to_string(),
            Some(value) => {
                let a = object(value, &aquifer_fields)?;
                let mut out = String::new();
                for key in aquifer_fields {
                    write!(out, "{key}: {}, ", self.df_child(field(a, key)?)?)?;
                }
                format!("Some(Aquifers {{ {out} }})")
            }
        };
        let mut targets = Vec::new();
        for target in field(m, "spawn_target")?
            .as_array()
            .context("spawn targets")?
        {
            let mut parameters = Vec::new();
            for (name, range) in target.as_object().context("spawn target is a map")? {
                parameters.push(format!(
                    "({}, {})",
                    self.function_ref(name)?,
                    climate_parameter(range)?
                ));
            }
            targets.push(format!(
                "SpawnTarget {{ parameters: &[{}] }}",
                parameters.join(", ")
            ));
        }
        if let Some(debug) = m.get("debug_functions") {
            for entry in debug.as_array().context("debug functions")? {
                let e = object(entry, &["function", "label"])?;
                self.df(field(e, "function")?)?;
            }
        }
        Ok(format!(
            "min_y: {}, height: {}, sea_level: {}, default_block: {}, default_fluid: {}, legacy_random_source: {}, disable_mob_generation: {}, router: NoiseRouter {{ {router_out} }}, aquifers: {aquifers}, material_rule: &{}, spawn_target: &[{}]",
            int(noise, "min_y")?,
            int(noise, "height")?,
            int(m, "sea_level")?,
            block_state(self.input, field(m, "default_block")?)?,
            block_state(self.input, field(m, "default_fluid")?)?,
            boolean(m, "legacy_random_source")?,
            boolean(m, "disable_mob_generation")?,
            self.rule(field(m, "material_rule")?)?,
            targets.join(", ")
        ))
    }

    fn parameter_list(&self, value: &Value) -> Result<String> {
        let m = object(value, &["biomes"])?;
        let mut entries = Vec::new();
        for entry in field(m, "biomes")?.as_array().context("biomes is a list")? {
            let e = object(entry, &["biome", "parameters"])?;
            let biome = qualify(string(e, "biome")?);
            let p = object(
                field(e, "parameters")?,
                &[
                    "temperature",
                    "humidity",
                    "continentalness",
                    "erosion",
                    "depth",
                    "weirdness",
                    "offset",
                ],
            )?;
            let offset = num(p, "offset")?;
            ensure!((0.0..=1.0).contains(&offset), "offset out of range");
            entries.push(format!(
                "ClimatePoint {{ biome: {}, temperature: {}, humidity: {}, continentalness: {}, erosion: {}, depth: {}, weirdness: {}, offset: {} }}",
                str_literal(&biome),
                climate_parameter(field(p, "temperature")?)?,
                climate_parameter(field(p, "humidity")?)?,
                climate_parameter(field(p, "continentalness")?)?,
                climate_parameter(field(p, "erosion")?)?,
                climate_parameter(field(p, "depth")?)?,
                climate_parameter(field(p, "weirdness")?)?,
                quantize(offset)
            ));
        }
        ensure!(!entries.is_empty(), "parameter lists are non-empty");
        Ok(format!("&[{}]", entries.join(",\n    ")))
    }
}

pub fn generate(input: &Input) -> Result<String> {
    let g = Gen {
        input,
        noises: collect(input, "worldgen/noise"),
        functions: collect(input, "worldgen/density_function"),
        rules: collect(input, "worldgen/material_rule"),
        conditions: collect(input, "worldgen/material_condition"),
        settings: collect(input, "worldgen/noise_settings"),
    };
    let mut out = String::new();

    let mut table = Vec::new();
    for (name, value) in &g.noises {
        let ident = format!("NOISE_{}", shouty(name));
        let body = g.noise(value).with_context(|| format!("noise {name}"))?;
        writeln!(
            out,
            "static {ident}: Noise = Noise {{ name: {}, {body} }};",
            str_literal(name)
        )?;
        table.push(format!("({}, &{ident})", str_literal(name)));
    }
    writeln!(
        out,
        "static NOISES: &[(&str, &Noise)] = &[{}];",
        table.join(", ")
    )?;

    let mut table = Vec::new();
    for (name, value) in &g.functions {
        let ident = format!("DF_{}", shouty(name));
        let body = g
            .df(value)
            .with_context(|| format!("density function {name}"))?;
        writeln!(
            out,
            "static {ident}: NamedDensityFunction = NamedDensityFunction {{ name: {}, function: {body} }};",
            str_literal(name)
        )?;
        table.push(format!("({}, &{ident})", str_literal(name)));
    }
    writeln!(
        out,
        "static DENSITY_FUNCTIONS: &[(&str, &NamedDensityFunction)] = &[{}];",
        table.join(", ")
    )?;

    let mut conditions = Vec::new();
    for (name, value) in &g.conditions {
        let body = g
            .condition(value)
            .with_context(|| format!("material condition {name}"))?;
        writeln!(
            out,
            "static MC_{}: NamedMaterialCondition = NamedMaterialCondition {{ name: {}, condition: {body} }};",
            shouty(name),
            str_literal(name)
        )?;
        conditions.push(format!("({}, &MC_{})", str_literal(name), shouty(name)));
    }
    writeln!(
        out,
        "static MATERIAL_CONDITIONS: &[(&str, &NamedMaterialCondition)] = &[{}];",
        conditions.join(", ")
    )?;

    let mut table = Vec::new();
    for (name, value) in &g.rules {
        let ident = format!("MR_{}", shouty(name));
        let body = g
            .rule(value)
            .with_context(|| format!("material rule {name}"))?;
        writeln!(
            out,
            "static {ident}: NamedMaterialRule = NamedMaterialRule {{ name: {}, rule: {body} }};",
            str_literal(name)
        )?;
        table.push(format!("({}, &{ident})", str_literal(name)));
    }
    writeln!(
        out,
        "static MATERIAL_RULES: &[(&str, &NamedMaterialRule)] = &[{}];",
        table.join(", ")
    )?;

    let mut table = Vec::new();
    for (name, value) in &g.settings {
        let ident = format!("NS_{}", shouty(name));
        let body = g
            .settings(value)
            .with_context(|| format!("noise settings {name}"))?;
        writeln!(
            out,
            "static {ident}: NoiseSettings = NoiseSettings {{ name: {}, {body} }};",
            str_literal(name)
        )?;
        table.push(format!("({}, &{ident})", str_literal(name)));
    }
    writeln!(
        out,
        "static NOISE_SETTINGS: &[(&str, &NoiseSettings)] = &[{}];",
        table.join(", ")
    )?;

    let mut table = Vec::new();
    for (name, value) in &input.biome_parameters {
        let body = g
            .parameter_list(value)
            .with_context(|| format!("parameter list {name}"))?;
        let ident = format!("PL_{}", shouty(name));
        writeln!(out, "static {ident}: &[ClimatePoint] = {body};")?;
        table.push(format!("({}, {ident})", str_literal(name)));
    }
    let listed = collect(input, "worldgen/multi_noise_biome_source_parameter_list");
    for (name, value) in &listed {
        let preset = object(value, &["preset"])?;
        let preset = qualify(string(preset, "preset")?);
        ensure!(
            input.biome_parameters.contains_key(&preset),
            "parameter list {name} names preset {preset} with no report"
        );
    }
    writeln!(
        out,
        "static PARAMETER_LISTS: &[(&str, &[ClimatePoint])] = &[{}];",
        table.join(", ")
    )?;

    let mut table = Vec::new();
    for (name, value) in &collect(input, "worldgen/carver") {
        let ident = format!("CARVER_{}", shouty(name));
        let body = carver(value).with_context(|| format!("carver {name}"))?;
        writeln!(
            out,
            "static {ident}: Carver = Carver {{ name: {}, {body} }};",
            str_literal(name)
        )?;
        table.push(format!("({}, &{ident})", str_literal(name)));
    }
    writeln!(
        out,
        "pub static CARVERS: &[(&str, &Carver)] = &[{}];",
        table.join(", ")
    )?;

    let configured = collect(input, "worldgen/feature");
    let mut table = Vec::new();
    for (name, value) in &configured {
        check_typed(value, FEATURE_FIELDS, "feature").with_context(|| format!("feature {name}"))?;
        table.push(format!(
            "({}, {})",
            str_literal(name),
            super::json::literal(value)
        ));
    }
    writeln!(
        out,
        "pub static CONFIGURED_FEATURES: &[(&str, &str)] = &[{}];",
        table.join(",\n    ")
    )?;
    let placed = collect(input, "worldgen/placed_feature");
    let mut table = Vec::new();
    for (name, value) in &placed {
        check_placed(value, &configured).with_context(|| format!("placed feature {name}"))?;
        table.push(format!(
            "({}, {})",
            str_literal(name),
            super::json::literal(value)
        ));
    }
    writeln!(
        out,
        "pub static PLACED_FEATURES: &[(&str, &str)] = &[{}];",
        table.join(",\n    ")
    )?;
    for (path, json) in &input.data_json {
        if path.contains("/worldgen/biome/") {
            for step in json
                .get("features")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for feature in step.as_array().context("feature steps are lists")? {
                    let name = qualify(feature.as_str().context("biome features are names")?);
                    ensure!(
                        placed.contains_key(&name),
                        "{path} names unknown placed feature {name}"
                    );
                }
            }
        }
    }
    let providers = collect(input, "worldgen/block_state_provider");
    let table: Vec<String> = providers
        .iter()
        .map(|(name, value)| format!("({}, {})", str_literal(name), super::json::literal(value)))
        .collect();
    writeln!(
        out,
        "pub static BLOCK_STATE_PROVIDERS: &[(&str, &str)] = &[{}];",
        table.join(",\n    ")
    )?;
    Ok(out)
}
mod providers;
pub use providers::*;
