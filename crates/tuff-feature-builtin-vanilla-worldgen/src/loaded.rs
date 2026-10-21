//! Noises, density functions, material rules and conditions, and noise
//! settings from data pack JSON, decoded into the same static shapes the
//! generated tables use so the loader, the density compiler and the surface
//! system run them unchanged. Built-in entries that reference a pack entry
//! are relinked to it; everything else stays the generated table. Decoded
//! values live for the rest of the process (worldgen registries are fixed
//! at server start).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::worldgen::{
    Aquifers, Axis, BinaryOp, CaveSurface, ClimateParameter, DensityFunction, DistanceMetric,
    MaterialCondition, MaterialRule, NamedDensityFunction, NamedMaterialCondition,
    NamedMaterialRule, Noise, NoiseRouter, NoiseSettings, Normalize, SpawnTarget, Spline,
    SplinePoint, Tiling, UnaryOp,
};

use crate::json::{Result, anchor, block_state, kind, obj, qualify};

pub mod biomes;
mod decode;
pub mod features;
pub mod structures;
use decode::{
    biomes, climate_parameter, clone_condition, clone_df, clone_rule, clone_spline, decode_noise,
};

pub const NOISE: &str = "minecraft:worldgen/noise";
pub const DENSITY_FUNCTION: &str = "minecraft:worldgen/density_function";
pub const MATERIAL_RULE: &str = "minecraft:worldgen/material_rule";
pub const MATERIAL_CONDITION: &str = "minecraft:worldgen/material_condition";
pub const NOISE_SETTINGS: &str = "minecraft:worldgen/noise_settings";

/// The registries this module reads.
pub const REGISTRIES: [&str; 5] = [
    NOISE,
    DENSITY_FUNCTION,
    MATERIAL_RULE,
    MATERIAL_CONDITION,
    NOISE_SETTINGS,
];

/// What a generator built from loaded settings needs besides the settings:
/// the noises by name (the surface system looks some up directly) and
/// whether density functions run as op-list programs or tree-walked.
#[derive(Clone, Default)]
pub struct Options {
    noises: Option<Arc<HashMap<String, &'static Noise>>>,
    pub interpret: bool,
    pub features: Option<Arc<features::FeatureSources>>,
    pub biomes: Option<Arc<biomes::PackBiomes>>,
    pub structures: Option<Arc<structures::StructureSources>>,
}

impl Options {
    /// The pack-added biomes and parameter lists, if any.
    pub fn biomes(&self) -> &biomes::PackBiomes {
        self.biomes
            .as_deref()
            .unwrap_or_else(|| biomes::PackBiomes::none())
    }

    pub fn noise(&self, name: &str) -> Option<&'static Noise> {
        self.noises
            .as_ref()
            .and_then(|noises| noises.get(name).copied())
            .or_else(|| Noise::get(name))
    }
}

fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn same<T>(a: &T, b: &T) -> bool {
    std::ptr::eq(a, b)
}

/// Pack JSON by registry and id; entries absent here are built in.
#[derive(Default)]
pub struct Sources {
    files: HashMap<&'static str, HashMap<String, Arc<Value>>>,
}

impl Sources {
    pub fn insert(&mut self, registry: &'static str, id: &str, json: Arc<Value>) {
        self.files
            .entry(registry)
            .or_default()
            .insert(qualify(id), json);
    }

    pub fn is_empty(&self) -> bool {
        self.files.values().all(HashMap::is_empty)
    }

    fn get(&self, registry: &str, id: &str) -> Option<Arc<Value>> {
        self.files.get(registry)?.get(id).cloned()
    }
}

/// Resolves entries against pack sources, decoding each once.
pub struct Linker {
    sources: Sources,
    noises: HashMap<String, &'static Noise>,
    functions: HashMap<String, &'static NamedDensityFunction>,
    rules: HashMap<String, &'static NamedMaterialRule>,
    conditions: HashMap<String, &'static NamedMaterialCondition>,
    settings: HashMap<String, &'static NoiseSettings>,
    resolving: HashSet<(&'static str, String)>,
}

impl Linker {
    pub fn new(sources: Sources) -> Self {
        Self {
            sources,
            noises: HashMap::new(),
            functions: HashMap::new(),
            rules: HashMap::new(),
            conditions: HashMap::new(),
            settings: HashMap::new(),
            resolving: HashSet::new(),
        }
    }

    /// The options for generators built from this linker's settings.
    pub fn options(&mut self, interpret: bool) -> Result<Options> {
        let ids: Vec<String> = self
            .sources
            .files
            .get(NOISE)
            .map(|files| files.keys().cloned().collect())
            .unwrap_or_default();
        let mut noises = HashMap::new();
        for id in ids {
            noises.insert(id.clone(), self.noise(&id)?);
        }
        Ok(Options {
            noises: Some(Arc::new(noises)),
            interpret,
            features: None,
            biomes: None,
            structures: None,
        })
    }

    fn enter(&mut self, registry: &'static str, id: &str) -> Result<()> {
        if !self.resolving.insert((registry, id.to_owned())) {
            return Err(format!("{registry} {id} references itself"));
        }
        Ok(())
    }

    fn leave(&mut self, registry: &'static str, id: &str) {
        self.resolving.remove(&(registry, id.to_owned()));
    }

    pub fn noise(&mut self, id: &str) -> Result<&'static Noise> {
        let id = qualify(id);
        if let Some(noise) = self.noises.get(&id) {
            return Ok(noise);
        }
        let noise = match self.sources.get(NOISE, &id) {
            Some(json) => leak(decode_noise(&id, &json).map_err(|e| format!("noise {id}: {e}"))?),
            None => Noise::get(&id).ok_or_else(|| format!("unknown noise {id}"))?,
        };
        self.noises.insert(id, noise);
        Ok(noise)
    }

    pub fn function(&mut self, id: &str) -> Result<&'static NamedDensityFunction> {
        let id = qualify(id);
        if let Some(function) = self.functions.get(&id) {
            return Ok(function);
        }
        self.enter(DENSITY_FUNCTION, &id)?;
        let resolved = match self.sources.get(DENSITY_FUNCTION, &id) {
            Some(json) => self.df(&json).map(|function| {
                leak(NamedDensityFunction {
                    name: Box::leak(id.clone().into_boxed_str()),
                    function,
                })
            }),
            None => match NamedDensityFunction::get(&id) {
                None => Err(format!("unknown density function {id}")),
                Some(builtin) => self.relink_df(&builtin.function).map(|function| {
                    if same(function, &builtin.function) {
                        builtin
                    } else {
                        leak(NamedDensityFunction {
                            name: builtin.name,
                            function: clone_df(function),
                        })
                    }
                }),
            },
        };
        self.leave(DENSITY_FUNCTION, &id);
        let function = resolved.map_err(|e| format!("density function {id}: {e}"))?;
        self.functions.insert(id, function);
        Ok(function)
    }

    pub fn rule(&mut self, id: &str) -> Result<&'static NamedMaterialRule> {
        let id = qualify(id);
        if let Some(rule) = self.rules.get(&id) {
            return Ok(rule);
        }
        self.enter(MATERIAL_RULE, &id)?;
        let resolved = match self.sources.get(MATERIAL_RULE, &id) {
            Some(json) => self.material_rule(&json).map(|rule| {
                leak(NamedMaterialRule {
                    name: Box::leak(id.clone().into_boxed_str()),
                    rule,
                })
            }),
            None => match NamedMaterialRule::get(&id) {
                None => Err(format!("unknown material rule {id}")),
                Some(builtin) => self.relink_rule(&builtin.rule).map(|rule| {
                    if same(rule, &builtin.rule) {
                        builtin
                    } else {
                        leak(NamedMaterialRule {
                            name: builtin.name,
                            rule: clone_rule(rule),
                        })
                    }
                }),
            },
        };
        self.leave(MATERIAL_RULE, &id);
        let rule = resolved.map_err(|e| format!("material rule {id}: {e}"))?;
        self.rules.insert(id, rule);
        Ok(rule)
    }

    pub fn condition(&mut self, id: &str) -> Result<&'static NamedMaterialCondition> {
        let id = qualify(id);
        if let Some(condition) = self.conditions.get(&id) {
            return Ok(condition);
        }
        self.enter(MATERIAL_CONDITION, &id)?;
        let resolved = match self.sources.get(MATERIAL_CONDITION, &id) {
            Some(json) => self.material_condition(&json).map(|condition| {
                leak(NamedMaterialCondition {
                    name: Box::leak(id.clone().into_boxed_str()),
                    condition,
                })
            }),
            None => match tuff_vanilla_data::worldgen::NamedMaterialCondition::get(&id) {
                None => Err(format!("unknown material condition {id}")),
                Some(builtin) => self.relink_condition(&builtin.condition).map(|condition| {
                    if same(condition, &builtin.condition) {
                        builtin
                    } else {
                        leak(NamedMaterialCondition {
                            name: builtin.name,
                            condition: clone_condition(condition),
                        })
                    }
                }),
            },
        };
        self.leave(MATERIAL_CONDITION, &id);
        let condition = resolved.map_err(|e| format!("material condition {id}: {e}"))?;
        self.conditions.insert(id, condition);
        Ok(condition)
    }

    /// The noise settings `id`, decoded from the pack or relinked from the
    /// built-in table.
    pub fn settings(&mut self, id: &str) -> Result<&'static NoiseSettings> {
        let id = qualify(id);
        if let Some(settings) = self.settings.get(&id) {
            return Ok(settings);
        }
        let settings = match self.sources.get(NOISE_SETTINGS, &id) {
            Some(json) => self.decode_settings(&id, &json).map(leak),
            None => {
                let builtin = NoiseSettings::get(&id)
                    .ok_or_else(|| format!("unknown noise settings {id}"))?;
                self.relink_settings(builtin)
            }
        }
        .map_err(|e| format!("noise settings {id}: {e}"))?;
        self.settings.insert(id, settings);
        Ok(settings)
    }

    fn child(&mut self, value: &Value) -> Result<&'static DensityFunction> {
        Ok(leak(self.df(value)?))
    }

    /// A density function: a constant, a reference by name, or a typed
    /// object.
    pub fn df(&mut self, value: &Value) -> Result<DensityFunction> {
        match value {
            Value::Number(n) => Ok(DensityFunction::Constant(
                n.as_f64().ok_or("bad constant")? as f32
            )),
            Value::String(name) => Ok(DensityFunction::Reference(self.function(name)?)),
            Value::Object(_) => {
                let kind = kind(value)?.to_owned();
                self.df_object(&qualify(&kind), value)
                    .map_err(|e| format!("density function {kind}: {e}"))
            }
            _ => Err(format!("bad density function {value}")),
        }
    }

    fn df_object(&mut self, kind: &str, value: &Value) -> Result<DensityFunction> {
        let unary = |op| -> Option<UnaryOp> { Some(op) };
        let unary_op = match kind {
            "minecraft:abs" => unary(UnaryOp::Abs),
            "minecraft:square" => unary(UnaryOp::Square),
            "minecraft:cube" => unary(UnaryOp::Cube),
            "minecraft:half_negative" => unary(UnaryOp::HalfNegative),
            "minecraft:quarter_negative" => unary(UnaryOp::QuarterNegative),
            "minecraft:negate" => unary(UnaryOp::Negate),
            "minecraft:squeeze" => unary(UnaryOp::Squeeze),
            "minecraft:sqrt" => unary(UnaryOp::Sqrt),
            "minecraft:reciprocal" => unary(UnaryOp::Reciprocal),
            "minecraft:log" => unary(UnaryOp::Log),
            "minecraft:sign" => unary(UnaryOp::Sign),
            _ => None,
        };
        if let Some(op) = unary_op {
            let o = obj(value, &["input"], kind)?;
            return Ok(DensityFunction::Unary(op, self.child(o.req("input")?)?));
        }
        let binary_op = match kind {
            "minecraft:add" => Some(BinaryOp::Add),
            "minecraft:sub" => Some(BinaryOp::Sub),
            "minecraft:mul" => Some(BinaryOp::Mul),
            "minecraft:div" => Some(BinaryOp::Div),
            "minecraft:min" => Some(BinaryOp::Min),
            "minecraft:max" => Some(BinaryOp::Max),
            _ => None,
        };
        if let Some(op) = binary_op {
            let o = obj(value, &["left", "right"], kind)?;
            return Ok(DensityFunction::Binary(
                op,
                self.child(o.req("left")?)?,
                self.child(o.req("right")?)?,
            ));
        }
        let simple = |variant: DensityFunction| -> Result<DensityFunction> {
            obj(value, &[], kind)?;
            Ok(variant)
        };
        let axis = |o: &crate::json::Obj<'_>| -> Result<Axis> {
            Ok(match o.str("axis")? {
                "x" => Axis::X,
                "y" => Axis::Y,
                "z" => Axis::Z,
                other => return Err(format!("unknown axis {other}")),
            })
        };
        Ok(match kind {
            "minecraft:constant" => {
                DensityFunction::Constant(obj(value, &["argument"], kind)?.f32("argument")?)
            }
            "minecraft:pow" => {
                let o = obj(value, &["base", "exponent"], kind)?;
                DensityFunction::Binary(
                    BinaryOp::Pow,
                    self.child(o.req("base")?)?,
                    self.child(o.req("exponent")?)?,
                )
            }
            "minecraft:floor" | "minecraft:round" | "minecraft:ceil" | "minecraft:truncate" => {
                let o = obj(value, &["input", "multiple"], kind)?;
                let op = match kind {
                    "minecraft:floor" => BinaryOp::Floor,
                    "minecraft:round" => BinaryOp::Round,
                    "minecraft:ceil" => BinaryOp::Ceil,
                    _ => BinaryOp::Truncate,
                };
                let input = self.child(o.req("input")?)?;
                let multiple = match o.get("multiple") {
                    Some(v) => self.child(v)?,
                    None => leak(DensityFunction::Constant(1.0)),
                };
                DensityFunction::Binary(op, input, multiple)
            }
            "minecraft:blend_alpha" => simple(DensityFunction::BlendAlpha)?,
            "minecraft:blend_offset" => simple(DensityFunction::BlendOffset)?,
            "minecraft:beardifier" => simple(DensityFunction::Beardifier)?,
            "minecraft:end_outer_islands" => simple(DensityFunction::EndIslands)?,
            "minecraft:noise" => {
                let o = obj(
                    value,
                    &[
                        "noise", "xz_scale", "y_scale", "shift_x", "shift_y", "shift_z",
                    ],
                    kind,
                )?;
                let mut shift = |key: &str| -> Result<&'static DensityFunction> {
                    match o.get(key) {
                        Some(v) => self.child(v),
                        None => Ok(leak(DensityFunction::Constant(0.0))),
                    }
                };
                let (shift_x, shift_y, shift_z) =
                    (shift("shift_x")?, shift("shift_y")?, shift("shift_z")?);
                DensityFunction::Noise {
                    noise: self.noise(o.str("noise")?)?,
                    xz_scale: o.f64("xz_scale")?,
                    y_scale: o.f64("y_scale")?,
                    shift_x,
                    shift_y,
                    shift_z,
                }
            }
            "minecraft:shift_a" | "minecraft:shift_b" | "minecraft:shift" => {
                let o = obj(value, &["argument", "noise"], kind)?;
                let name = o
                    .get("argument")
                    .or_else(|| o.get("noise"))
                    .and_then(Value::as_str)
                    .ok_or("missing noise")?;
                let noise = self.noise(name)?;
                match kind {
                    "minecraft:shift_a" => DensityFunction::ShiftA(noise),
                    "minecraft:shift_b" => DensityFunction::ShiftB(noise),
                    _ => DensityFunction::Shift(noise),
                }
            }
            "minecraft:distance_to_point" => {
                let o = obj(value, &["point", "metric"], kind)?;
                DensityFunction::DistanceToPoint {
                    point: crate::json::vec3i(o.req("point")?)?,
                    metric: match o.str("metric")? {
                        "euclidean" => DistanceMetric::Euclidean,
                        "euclidean_squared" => DistanceMetric::EuclideanSquared,
                        "manhattan" => DistanceMetric::Manhattan,
                        "chebyshev" => DistanceMetric::Chebyshev,
                        other => return Err(format!("unknown metric {other}")),
                    },
                }
            }
            "minecraft:gradient" => {
                let o = obj(
                    value,
                    &[
                        "axis",
                        "tiling",
                        "from_coordinate",
                        "to_coordinate",
                        "from_value",
                        "to_value",
                    ],
                    kind,
                )?;
                let tiling = match o.get("tiling").map(|t| t.as_str().ok_or("bad tiling")) {
                    None | Some(Ok("clamp_to_edge")) => Tiling::ClampToEdge,
                    Some(Ok("repeat")) => Tiling::Repeat,
                    Some(Ok("mirrored_repeat")) => Tiling::MirroredRepeat,
                    Some(Ok(other)) => return Err(format!("unknown tiling {other}")),
                    Some(Err(e)) => return Err(e.into()),
                };
                let (from, to) = (o.i32("from_coordinate")?, o.i32("to_coordinate")?);
                if from == to {
                    return Err("gradient coordinates must differ".into());
                }
                DensityFunction::Gradient {
                    axis: axis(&o)?,
                    tiling,
                    from_coordinate: from,
                    to_coordinate: to,
                    from_value: o.f32("from_value")?,
                    to_value: o.f32("to_value")?,
                }
            }
            "minecraft:spline" => {
                let o = obj(value, &["spline"], kind)?;
                DensityFunction::Spline(leak(self.spline(o.req("spline")?)?))
            }
            "minecraft:lerp" => {
                let o = obj(value, &["alpha", "first", "second"], kind)?;
                DensityFunction::Lerp {
                    alpha: self.child(o.req("alpha")?)?,
                    first: self.child(o.req("first")?)?,
                    second: self.child(o.req("second")?)?,
                }
            }
            "minecraft:clamp" => {
                let o = obj(value, &["input", "min", "max"], kind)?;
                let (min, max) = (o.f32("min")?, o.f32("max")?);
                if min > max {
                    return Err("clamp min > max".into());
                }
                DensityFunction::Clamp {
                    input: self.child(o.req("input")?)?,
                    min,
                    max,
                }
            }
            "minecraft:range_choice" => {
                let o = obj(
                    value,
                    &[
                        "input",
                        "min_inclusive",
                        "max_exclusive",
                        "when_in_range",
                        "when_out_of_range",
                    ],
                    kind,
                )?;
                DensityFunction::RangeChoice {
                    input: self.child(o.req("input")?)?,
                    min_inclusive: o.f32("min_inclusive")?,
                    max_exclusive: o.f32("max_exclusive")?,
                    when_in_range: self.child(o.req("when_in_range")?)?,
                    when_out_of_range: self.child(o.req("when_out_of_range")?)?,
                }
            }
            "minecraft:interval_select" => {
                let o = obj(value, &["input", "thresholds", "functions"], kind)?;
                let thresholds = o
                    .list("thresholds")?
                    .iter()
                    .map(|v| v.as_f64().map(|v| v as f32).ok_or("bad threshold"))
                    .collect::<std::result::Result<Vec<f32>, _>>()?;
                let functions = o.list("functions")?;
                if thresholds.len() + 1 != functions.len() {
                    return Err("interval_select needs one more function than thresholds".into());
                }
                if !thresholds.windows(2).all(|w| w[0] <= w[1]) {
                    return Err("interval_select thresholds must be sorted".into());
                }
                let functions = functions
                    .iter()
                    .map(|f| self.df(f))
                    .collect::<Result<Vec<_>>>()?;
                DensityFunction::IntervalSelect {
                    input: self.child(o.req("input")?)?,
                    thresholds: thresholds.leak(),
                    functions: functions.leak(),
                }
            }
            "minecraft:cache" => {
                DensityFunction::Cache(self.child(obj(value, &["input"], kind)?.req("input")?)?)
            }
            "minecraft:blend_density" => DensityFunction::BlendDensity(
                self.child(obj(value, &["input"], kind)?.req("input")?)?,
            ),
            "minecraft:interpolated" => {
                let o = obj(value, &["input", "cell_size_xz", "cell_size_y"], kind)?;
                let (cell_xz, cell_y) = (o.i32("cell_size_xz")?, o.i32("cell_size_y")?);
                if cell_xz <= 0 || cell_y <= 0 {
                    return Err("cell sizes are positive".into());
                }
                DensityFunction::Interpolated {
                    input: self.child(o.req("input")?)?,
                    cell_xz,
                    cell_y,
                }
            }
            "minecraft:slice" => {
                let o = obj(value, &["axis", "coordinate", "input"], kind)?;
                DensityFunction::Slice {
                    axis: axis(&o)?,
                    coordinate: o.i32("coordinate")?,
                    input: self.child(o.req("input")?)?,
                }
            }
            "minecraft:find_top_surface" => {
                let o = obj(
                    value,
                    &["density", "upper_bound", "lower_bound", "cell_height"],
                    kind,
                )?;
                DensityFunction::FindTopSurface {
                    density: self.child(o.req("density")?)?,
                    upper_bound: self.child(o.req("upper_bound")?)?,
                    lower_bound: o.i32("lower_bound")?,
                    cell_height: o.i32("cell_height")?,
                }
            }
            "minecraft:old_blended_noise" => {
                let o = obj(
                    value,
                    &[
                        "xz_scale",
                        "y_scale",
                        "xz_factor",
                        "y_factor",
                        "smear_scale_multiplier",
                    ],
                    kind,
                )?;
                DensityFunction::OldBlendedNoise {
                    xz_scale: o.f64("xz_scale")?,
                    y_scale: o.f64("y_scale")?,
                    xz_factor: o.f64("xz_factor")?,
                    y_factor: o.f64("y_factor")?,
                    smear_scale_multiplier: o.f64("smear_scale_multiplier")?,
                }
            }
            other => return Err(format!("unknown density function type {other}")),
        })
    }

    fn spline(&mut self, value: &Value) -> Result<Spline> {
        if let Some(v) = value.as_f64() {
            return Ok(Spline::Constant(v as f32));
        }
        let o = obj(value, &["coordinate", "points"], "spline")?;
        let points = o.list("points")?;
        if points.is_empty() {
            return Err("splines need points".into());
        }
        let coordinate = self.child(o.req("coordinate")?)?;
        let mut out = Vec::with_capacity(points.len());
        let mut last = f32::NEG_INFINITY;
        for point in points {
            let p = obj(point, &["location", "value", "derivative"], "spline point")?;
            let location = p.f32("location")?;
            if location <= last {
                return Err("spline points must ascend".into());
            }
            last = location;
            out.push(SplinePoint {
                location,
                value: self.spline(p.req("value")?)?,
                derivative: p.f32("derivative")?,
            });
        }
        Ok(Spline::Multipoint {
            coordinate,
            points: out.leak(),
        })
    }

    pub fn material_condition(&mut self, value: &Value) -> Result<MaterialCondition> {
        if let Some(name) = value.as_str() {
            return Ok(MaterialCondition::Reference(self.condition(name)?));
        }
        let kind = qualify(kind(value)?);
        let k = kind.as_str();
        Ok(match k {
            "minecraft:biome" => {
                MaterialCondition::Biome(biomes(obj(value, &["biome_is"], k)?.req("biome_is")?)?)
            }
            "minecraft:noise_threshold" => {
                let o = obj(
                    value,
                    &["noise", "min_threshold", "max_threshold", "is_3d"],
                    k,
                )?;
                MaterialCondition::NoiseThreshold {
                    noise: self.noise(o.str("noise")?)?,
                    min_threshold: o.f64("min_threshold")?,
                    max_threshold: o.f64("max_threshold")?,
                    is_3d: o.bool_or("is_3d", false)?,
                }
            }
            "minecraft:vertical_gradient" => {
                let o = obj(
                    value,
                    &["random_name", "true_at_and_below", "false_at_and_above"],
                    k,
                )?;
                MaterialCondition::VerticalGradient {
                    random_name: Box::leak(qualify(o.str("random_name")?).into_boxed_str()),
                    true_at_and_below: anchor(o.req("true_at_and_below")?)?,
                    false_at_and_above: anchor(o.req("false_at_and_above")?)?,
                }
            }
            "minecraft:y_above" => {
                let o = obj(
                    value,
                    &["anchor", "surface_depth_multiplier", "add_stone_depth"],
                    k,
                )?;
                MaterialCondition::YAbove {
                    anchor: anchor(o.req("anchor")?)?,
                    surface_depth_multiplier: o.i32("surface_depth_multiplier")?,
                    add_stone_depth: o.bool("add_stone_depth")?,
                }
            }
            "minecraft:water" => {
                let o = obj(
                    value,
                    &["offset", "surface_depth_multiplier", "add_stone_depth"],
                    k,
                )?;
                MaterialCondition::Water {
                    offset: o.i32("offset")?,
                    surface_depth_multiplier: o.i32("surface_depth_multiplier")?,
                    add_stone_depth: o.bool("add_stone_depth")?,
                }
            }
            "minecraft:stone_depth" => {
                let o = obj(
                    value,
                    &[
                        "offset",
                        "add_surface_depth",
                        "secondary_depth_range",
                        "surface_type",
                    ],
                    k,
                )?;
                MaterialCondition::StoneDepth {
                    offset: o.i32("offset")?,
                    add_surface_depth: o.bool("add_surface_depth")?,
                    secondary_depth_range: o.i32("secondary_depth_range")?,
                    surface_type: match o.str("surface_type")? {
                        "floor" => CaveSurface::Floor,
                        "ceiling" => CaveSurface::Ceiling,
                        other => return Err(format!("unknown surface type {other}")),
                    },
                }
            }
            "minecraft:not" => MaterialCondition::Not(leak(
                self.material_condition(obj(value, &["invert"], k)?.req("invert")?)?,
            )),
            "minecraft:steep" => {
                obj(value, &[], k)?;
                MaterialCondition::Steep
            }
            "minecraft:hole" => {
                obj(value, &[], k)?;
                MaterialCondition::Hole
            }
            "minecraft:temperature" => {
                obj(value, &[], k)?;
                MaterialCondition::Temperature
            }
            "minecraft:above_preliminary_surface" => {
                obj(value, &[], k)?;
                MaterialCondition::AbovePreliminarySurface
            }
            other => return Err(format!("unknown material condition {other}")),
        })
    }

    pub fn material_rule(&mut self, value: &Value) -> Result<MaterialRule> {
        if let Some(name) = value.as_str() {
            return Ok(MaterialRule::Reference(self.rule(name)?));
        }
        let kind = qualify(kind(value)?);
        let k = kind.as_str();
        Ok(match k {
            "minecraft:block" => MaterialRule::Block(block_state(
                obj(value, &["result_state"], k)?.req("result_state")?,
            )?),
            "minecraft:sequence" => {
                let rules = obj(value, &["sequence"], k)?
                    .list("sequence")?
                    .iter()
                    .map(|r| self.material_rule(r))
                    .collect::<Result<Vec<_>>>()?;
                MaterialRule::Sequence(rules.leak())
            }
            "minecraft:condition" => {
                let o = obj(value, &["if_true", "then_run"], k)?;
                MaterialRule::Condition {
                    if_true: leak(self.material_condition(o.req("if_true")?)?),
                    then_run: leak(self.material_rule(o.req("then_run")?)?),
                }
            }
            "minecraft:bandlands" => {
                obj(value, &[], k)?;
                MaterialRule::Bandlands
            }
            "minecraft:ore_vein" => {
                let o = obj(
                    value,
                    &[
                        "ore_block",
                        "raw_ore_block",
                        "filler_block",
                        "raw_ore_chance",
                        "density",
                        "richness",
                        "filler_gap",
                    ],
                    k,
                )?;
                MaterialRule::OreVein {
                    ore_block: block_state(o.req("ore_block")?)?,
                    raw_ore_block: block_state(o.req("raw_ore_block")?)?,
                    filler_block: block_state(o.req("filler_block")?)?,
                    raw_ore_chance: o.f32("raw_ore_chance")?,
                    density: self.child(o.req("density")?)?,
                    richness: self.child(o.req("richness")?)?,
                    filler_gap: self.child(o.req("filler_gap")?)?,
                }
            }
            other => return Err(format!("unknown material rule {other}")),
        })
    }

    fn decode_settings(&mut self, id: &str, value: &Value) -> Result<NoiseSettings> {
        let o = obj(
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
            "noise settings",
        )?;
        let shape = obj(o.req("noise")?, &["min_y", "height"], "noise")?;
        let r = obj(
            o.req("noise_router")?,
            &[
                "temperature",
                "vegetation",
                "continents",
                "erosion",
                "depth",
                "ridges",
                "chunk_surface_level",
                "final_density",
            ],
            "noise router",
        )?;
        let router = NoiseRouter {
            temperature: self.child(r.req("temperature")?)?,
            vegetation: self.child(r.req("vegetation")?)?,
            continents: self.child(r.req("continents")?)?,
            erosion: self.child(r.req("erosion")?)?,
            depth: self.child(r.req("depth")?)?,
            ridges: self.child(r.req("ridges")?)?,
            chunk_surface_level: self.child(r.req("chunk_surface_level")?)?,
            final_density: self.child(r.req("final_density")?)?,
        };
        let aquifers = match o.get("aquifers") {
            None => None,
            Some(value) => {
                let a = obj(
                    value,
                    &[
                        "barrier",
                        "fluid_level_floodedness",
                        "fluid_level_spread",
                        "lava",
                        "exclusion",
                        "surface_level",
                    ],
                    "aquifers",
                )?;
                Some(Aquifers {
                    barrier: self.child(a.req("barrier")?)?,
                    fluid_level_floodedness: self.child(a.req("fluid_level_floodedness")?)?,
                    fluid_level_spread: self.child(a.req("fluid_level_spread")?)?,
                    lava: self.child(a.req("lava")?)?,
                    exclusion: self.child(a.req("exclusion")?)?,
                    surface_level: self.child(a.req("surface_level")?)?,
                })
            }
        };
        let mut targets = Vec::new();
        for target in o.list("spawn_target")? {
            let mut parameters = Vec::new();
            for (name, range) in target.as_object().ok_or("spawn targets are maps")? {
                parameters.push((self.function(name)?, climate_parameter(range)?));
            }
            targets.push(SpawnTarget {
                parameters: parameters.leak(),
            });
        }
        if let Some(debug) = o.get("debug_functions") {
            for entry in debug.as_array().ok_or("debug functions are a list")? {
                let e = obj(entry, &["function", "label"], "debug function")?;
                self.df(e.req("function")?)?;
            }
        }
        let height = shape.i32("height")?;
        let min_y = shape.i32("min_y")?;
        if height <= 0 || height % 16 != 0 || min_y % 16 != 0 {
            return Err("noise min_y and height must be multiples of 16".into());
        }
        Ok(NoiseSettings {
            name: Box::leak(id.to_owned().into_boxed_str()),
            min_y,
            height,
            sea_level: o.i32("sea_level")?,
            default_block: block_state(o.req("default_block")?)?,
            default_fluid: block_state(o.req("default_fluid")?)?,
            legacy_random_source: o.bool("legacy_random_source")?,
            disable_mob_generation: o.bool("disable_mob_generation")?,
            router,
            aquifers,
            material_rule: leak(self.material_rule(o.req("material_rule")?)?),
            spawn_target: targets.leak(),
        })
    }

    fn relink_settings(&mut self, s: &'static NoiseSettings) -> Result<&'static NoiseSettings> {
        let r = &s.router;
        let router = NoiseRouter {
            temperature: self.relink_df(r.temperature)?,
            vegetation: self.relink_df(r.vegetation)?,
            continents: self.relink_df(r.continents)?,
            erosion: self.relink_df(r.erosion)?,
            depth: self.relink_df(r.depth)?,
            ridges: self.relink_df(r.ridges)?,
            chunk_surface_level: self.relink_df(r.chunk_surface_level)?,
            final_density: self.relink_df(r.final_density)?,
        };
        let aquifers = match &s.aquifers {
            None => None,
            Some(a) => Some(Aquifers {
                barrier: self.relink_df(a.barrier)?,
                fluid_level_floodedness: self.relink_df(a.fluid_level_floodedness)?,
                fluid_level_spread: self.relink_df(a.fluid_level_spread)?,
                lava: self.relink_df(a.lava)?,
                exclusion: self.relink_df(a.exclusion)?,
                surface_level: self.relink_df(a.surface_level)?,
            }),
        };
        let material_rule = self.relink_rule(s.material_rule)?;
        let mut targets_changed = false;
        let mut targets = Vec::with_capacity(s.spawn_target.len());
        for target in s.spawn_target {
            let mut parameters = Vec::with_capacity(target.parameters.len());
            for (function, range) in target.parameters {
                let linked = self.function(function.name)?;
                targets_changed |= !same(linked, *function);
                parameters.push((linked, *range));
            }
            targets.push(SpawnTarget {
                parameters: parameters.leak(),
            });
        }
        let router_same = [
            (router.temperature, r.temperature),
            (router.vegetation, r.vegetation),
            (router.continents, r.continents),
            (router.erosion, r.erosion),
            (router.depth, r.depth),
            (router.ridges, r.ridges),
            (router.chunk_surface_level, r.chunk_surface_level),
            (router.final_density, r.final_density),
        ]
        .iter()
        .all(|(a, b)| same(*a, *b));
        let aquifers_same = match (&aquifers, &s.aquifers) {
            (Some(a), Some(b)) => [
                (a.barrier, b.barrier),
                (a.fluid_level_floodedness, b.fluid_level_floodedness),
                (a.fluid_level_spread, b.fluid_level_spread),
                (a.lava, b.lava),
                (a.exclusion, b.exclusion),
                (a.surface_level, b.surface_level),
            ]
            .iter()
            .all(|(a, b)| same(*a, *b)),
            _ => true,
        };
        if router_same && aquifers_same && same(material_rule, s.material_rule) && !targets_changed
        {
            return Ok(s);
        }
        Ok(leak(NoiseSettings {
            name: s.name,
            min_y: s.min_y,
            height: s.height,
            sea_level: s.sea_level,
            default_block: s.default_block,
            default_fluid: s.default_fluid,
            legacy_random_source: s.legacy_random_source,
            disable_mob_generation: s.disable_mob_generation,
            router,
            aquifers,
            material_rule,
            spawn_target: if targets_changed {
                targets.leak()
            } else {
                s.spawn_target
            },
        }))
    }

    /// `f` with its references resolved through this linker; `f` itself
    /// when nothing below it changed.
    fn relink_df(&mut self, f: &'static DensityFunction) -> Result<&'static DensityFunction> {
        use DensityFunction as D;
        let changed = |new: &DensityFunction| -> &'static DensityFunction { leak(clone_df(new)) };
        Ok(match f {
            D::Constant(_)
            | D::EndIslands
            | D::DistanceToPoint { .. }
            | D::Gradient { .. }
            | D::OldBlendedNoise { .. }
            | D::BlendAlpha
            | D::BlendOffset
            | D::Beardifier => f,
            D::Reference(named) => {
                let linked = self.function(named.name)?;
                if same(linked, *named) {
                    f
                } else {
                    changed(&D::Reference(linked))
                }
            }
            D::Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let n = self.noise(noise.name)?;
                let (x, y, z) = (
                    self.relink_df(shift_x)?,
                    self.relink_df(shift_y)?,
                    self.relink_df(shift_z)?,
                );
                if same(n, *noise) && same(x, *shift_x) && same(y, *shift_y) && same(z, *shift_z) {
                    f
                } else {
                    changed(&D::Noise {
                        noise: n,
                        xz_scale: *xz_scale,
                        y_scale: *y_scale,
                        shift_x: x,
                        shift_y: y,
                        shift_z: z,
                    })
                }
            }
            D::ShiftA(noise) | D::ShiftB(noise) | D::Shift(noise) => {
                let n = self.noise(noise.name)?;
                if same(n, *noise) {
                    f
                } else {
                    changed(&match f {
                        D::ShiftA(_) => D::ShiftA(n),
                        D::ShiftB(_) => D::ShiftB(n),
                        _ => D::Shift(n),
                    })
                }
            }
            D::Unary(op, input) => {
                let i = self.relink_df(input)?;
                if same(i, *input) {
                    f
                } else {
                    changed(&D::Unary(*op, i))
                }
            }
            D::Binary(op, l, r) => {
                let (nl, nr) = (self.relink_df(l)?, self.relink_df(r)?);
                if same(nl, *l) && same(nr, *r) {
                    f
                } else {
                    changed(&D::Binary(*op, nl, nr))
                }
            }
            D::Spline(spline) => {
                let s = self.relink_spline(spline)?;
                if same(s, *spline) {
                    f
                } else {
                    changed(&D::Spline(s))
                }
            }
            D::Lerp {
                alpha,
                first,
                second,
            } => {
                let (a, b, c) = (
                    self.relink_df(alpha)?,
                    self.relink_df(first)?,
                    self.relink_df(second)?,
                );
                if same(a, *alpha) && same(b, *first) && same(c, *second) {
                    f
                } else {
                    changed(&D::Lerp {
                        alpha: a,
                        first: b,
                        second: c,
                    })
                }
            }
            D::Clamp { input, min, max } => {
                let i = self.relink_df(input)?;
                if same(i, *input) {
                    f
                } else {
                    changed(&D::Clamp {
                        input: i,
                        min: *min,
                        max: *max,
                    })
                }
            }
            D::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let (i, a, b) = (
                    self.relink_df(input)?,
                    self.relink_df(when_in_range)?,
                    self.relink_df(when_out_of_range)?,
                );
                if same(i, *input) && same(a, *when_in_range) && same(b, *when_out_of_range) {
                    f
                } else {
                    changed(&D::RangeChoice {
                        input: i,
                        min_inclusive: *min_inclusive,
                        max_exclusive: *max_exclusive,
                        when_in_range: a,
                        when_out_of_range: b,
                    })
                }
            }
            D::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let i = self.relink_df(input)?;
                let linked = functions
                    .iter()
                    .map(|function| self.relink_df(function))
                    .collect::<Result<Vec<_>>>()?;
                if same(i, *input) && linked.iter().zip(*functions).all(|(a, b)| same(*a, b)) {
                    f
                } else {
                    changed(&D::IntervalSelect {
                        input: i,
                        thresholds,
                        functions: linked.into_iter().map(clone_df).collect::<Vec<_>>().leak(),
                    })
                }
            }
            D::Cache(input) | D::BlendDensity(input) => {
                let i = self.relink_df(input)?;
                if same(i, *input) {
                    f
                } else if matches!(f, D::Cache(_)) {
                    changed(&D::Cache(i))
                } else {
                    changed(&D::BlendDensity(i))
                }
            }
            D::Interpolated {
                input,
                cell_xz,
                cell_y,
            } => {
                let i = self.relink_df(input)?;
                if same(i, *input) {
                    f
                } else {
                    changed(&D::Interpolated {
                        input: i,
                        cell_xz: *cell_xz,
                        cell_y: *cell_y,
                    })
                }
            }
            D::Slice {
                axis,
                coordinate,
                input,
            } => {
                let i = self.relink_df(input)?;
                if same(i, *input) {
                    f
                } else {
                    changed(&D::Slice {
                        axis: *axis,
                        coordinate: *coordinate,
                        input: i,
                    })
                }
            }
            D::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let (d, u) = (self.relink_df(density)?, self.relink_df(upper_bound)?);
                if same(d, *density) && same(u, *upper_bound) {
                    f
                } else {
                    changed(&D::FindTopSurface {
                        density: d,
                        upper_bound: u,
                        lower_bound: *lower_bound,
                        cell_height: *cell_height,
                    })
                }
            }
        })
    }

    fn relink_spline(&mut self, spline: &'static Spline) -> Result<&'static Spline> {
        match spline {
            Spline::Constant(_) => Ok(spline),
            Spline::Multipoint { coordinate, points } => {
                let c = self.relink_df(coordinate)?;
                let mut values = Vec::with_capacity(points.len());
                for point in *points {
                    values.push(self.relink_spline(&point.value)?);
                }
                if same(c, *coordinate)
                    && values.iter().zip(*points).all(|(a, p)| same(*a, &p.value))
                {
                    return Ok(spline);
                }
                Ok(leak(Spline::Multipoint {
                    coordinate: c,
                    points: points
                        .iter()
                        .zip(values)
                        .map(|(p, value)| SplinePoint {
                            location: p.location,
                            value: clone_spline(value),
                            derivative: p.derivative,
                        })
                        .collect::<Vec<_>>()
                        .leak(),
                }))
            }
        }
    }

    fn relink_rule(&mut self, rule: &'static MaterialRule) -> Result<&'static MaterialRule> {
        Ok(match rule {
            MaterialRule::Block(_) | MaterialRule::Bandlands => rule,
            MaterialRule::Reference(named) => {
                let linked = self.rule(named.name)?;
                if same(linked, *named) {
                    rule
                } else {
                    leak(MaterialRule::Reference(linked))
                }
            }
            MaterialRule::Sequence(rules) => {
                let linked = rules
                    .iter()
                    .map(|r| self.relink_rule(r))
                    .collect::<Result<Vec<_>>>()?;
                if linked.iter().zip(*rules).all(|(a, b)| same(*a, b)) {
                    rule
                } else {
                    leak(MaterialRule::Sequence(
                        linked
                            .into_iter()
                            .map(clone_rule)
                            .collect::<Vec<_>>()
                            .leak(),
                    ))
                }
            }
            MaterialRule::Condition { if_true, then_run } => {
                let (c, r) = (self.relink_condition(if_true)?, self.relink_rule(then_run)?);
                if same(c, *if_true) && same(r, *then_run) {
                    rule
                } else {
                    leak(MaterialRule::Condition {
                        if_true: c,
                        then_run: r,
                    })
                }
            }
            MaterialRule::OreVein {
                ore_block,
                raw_ore_block,
                filler_block,
                raw_ore_chance,
                density,
                richness,
                filler_gap,
            } => {
                let (d, r, g) = (
                    self.relink_df(density)?,
                    self.relink_df(richness)?,
                    self.relink_df(filler_gap)?,
                );
                if same(d, *density) && same(r, *richness) && same(g, *filler_gap) {
                    rule
                } else {
                    leak(MaterialRule::OreVein {
                        ore_block: *ore_block,
                        raw_ore_block: *raw_ore_block,
                        filler_block: *filler_block,
                        raw_ore_chance: *raw_ore_chance,
                        density: d,
                        richness: r,
                        filler_gap: g,
                    })
                }
            }
        })
    }

    fn relink_condition(
        &mut self,
        condition: &'static MaterialCondition,
    ) -> Result<&'static MaterialCondition> {
        Ok(match condition {
            MaterialCondition::Reference(named) => {
                let linked = self.condition(named.name)?;
                if same(linked, *named) {
                    condition
                } else {
                    leak(MaterialCondition::Reference(linked))
                }
            }
            MaterialCondition::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
                is_3d,
            } => {
                let n = self.noise(noise.name)?;
                if same(n, *noise) {
                    condition
                } else {
                    leak(MaterialCondition::NoiseThreshold {
                        noise: n,
                        min_threshold: *min_threshold,
                        max_threshold: *max_threshold,
                        is_3d: *is_3d,
                    })
                }
            }
            MaterialCondition::Not(inner) => {
                let i = self.relink_condition(inner)?;
                if same(i, *inner) {
                    condition
                } else {
                    leak(MaterialCondition::Not(i))
                }
            }
            _ => condition,
        })
    }
}

/// The pack entries of the worldgen registries this module reads.
pub fn sources(registries: &tuff_item::registries::Registries) -> Sources {
    let mut sources = Sources::default();
    for key in REGISTRIES {
        let Some(registry) = registries.get(key) else {
            continue;
        };
        if !registry.modified() {
            continue;
        }
        for (id, entry) in registry.iter() {
            if let Some(json) = &entry.value().json {
                sources.insert(key, id, json.clone());
            }
        }
    }
    sources
}

/// The noise settings `id` as the packs leave them, with the options to
/// generate with; `None` when no pack touched these registries.
pub fn resolve(
    registries: &tuff_item::registries::Registries,
    id: &str,
    compile: bool,
) -> Option<Result<(&'static NoiseSettings, Options)>> {
    let sources = sources(registries);
    let features = match features::FeatureSources::from_registries(registries) {
        Ok(features) => features,
        Err((registry, entry, _, message)) => {
            return Some(Err(format!("{registry} {entry}: {message}")));
        }
    };
    let biomes = match biomes::PackBiomes::from_registries(registries) {
        Ok(biomes) => biomes,
        Err((registry, entry, _, message)) => {
            return Some(Err(format!("{registry} {entry}: {message}")));
        }
    };
    let structures = match structures::StructureSources::from_registries(registries) {
        Ok(structures) => structures,
        Err((registry, entry, _, message)) => {
            return Some(Err(format!("{registry} {entry}: {message}")));
        }
    };
    if sources.is_empty() && features.is_empty() && biomes.is_empty() && structures.is_empty() {
        return None;
    }
    let mut linker = Linker::new(sources);
    Some(linker.settings(id).and_then(|settings| {
        let mut options = linker.options(!compile)?;
        if !features.is_empty() {
            options.features = Some(Arc::new(features));
        }
        if !biomes.is_empty() {
            options.biomes = Some(Arc::new(biomes));
        }
        if !structures.is_empty() {
            options.structures = Some(Arc::new(structures));
        }
        Ok((settings, options))
    }))
}
/// The pack biomes, features and structures a flat generator uses;
/// `None` when no pack changes them.
pub fn flat_options(registries: &tuff_item::registries::Registries) -> Result<Option<Options>> {
    let biomes = biomes::PackBiomes::from_registries(registries)
        .map_err(|(registry, id, _, message)| format!("{registry} {id}: {message}"))?;
    let features = features::FeatureSources::from_registries(registries)
        .map_err(|(registry, id, _, message)| format!("{registry} {id}: {message}"))?;
    let structures = structures::StructureSources::from_registries(registries)
        .map_err(|(registry, id, _, message)| format!("{registry} {id}: {message}"))?;
    if biomes.is_empty() && features.is_empty() && structures.is_empty() {
        return Ok(None);
    }
    let mut options = Options::default();
    if !biomes.is_empty() {
        options.biomes = Some(Arc::new(biomes));
    }
    if !features.is_empty() {
        options.features = Some(Arc::new(features));
    }
    if !structures.is_empty() {
        options.structures = Some(Arc::new(structures));
    }
    Ok(Some(options))
}

mod validate;
pub use validate::*;
