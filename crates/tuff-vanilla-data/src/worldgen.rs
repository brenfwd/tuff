//! World generation data from the built-in data pack: noise definitions,
//! density functions, noise generator settings (router, aquifers, material
//! rules), material rules and conditions, and the multi-noise biome
//! parameter lists. Named entries stay behind `Reference` variants so their
//! identity survives; every kind the data uses is typed and an unknown kind
//! fails the build.

use crate::BlockState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalize {
    Disabled,
    Enabled,
    Legacy,
}

/// A `worldgen/noise` entry.
#[derive(Debug)]
pub struct Noise {
    pub name: &'static str,
    pub base_octave: i32,
    pub base_amplitude: f64,
    pub octave_count: i32,
    pub normalize: Normalize,
    /// Empty when every octave has weight one.
    pub amplitude_modifiers: &'static [f64],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tiling {
    ClampToEdge,
    Repeat,
    MirroredRepeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Chebyshev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Abs,
    Square,
    Cube,
    HalfNegative,
    QuarterNegative,
    Negate,
    Squeeze,
    Sqrt,
    Reciprocal,
    Log,
    Sign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Pow,
    Floor,
    Round,
    Ceil,
    Truncate,
}

/// A named `worldgen/density_function` entry.
#[derive(Debug)]
pub struct NamedDensityFunction {
    pub name: &'static str,
    pub function: DensityFunction,
}

#[derive(Debug)]
pub enum DensityFunction {
    Constant(f32),
    Reference(&'static NamedDensityFunction),
    Noise {
        noise: &'static Noise,
        xz_scale: f64,
        y_scale: f64,
        shift_x: &'static DensityFunction,
        shift_y: &'static DensityFunction,
        shift_z: &'static DensityFunction,
    },
    ShiftA(&'static Noise),
    ShiftB(&'static Noise),
    Shift(&'static Noise),
    EndIslands,
    DistanceToPoint {
        point: [i32; 3],
        metric: DistanceMetric,
    },
    Gradient {
        axis: Axis,
        tiling: Tiling,
        from_coordinate: i32,
        to_coordinate: i32,
        from_value: f32,
        to_value: f32,
    },
    Unary(UnaryOp, &'static DensityFunction),
    Binary(BinaryOp, &'static DensityFunction, &'static DensityFunction),
    Spline(&'static Spline),
    Lerp {
        alpha: &'static DensityFunction,
        first: &'static DensityFunction,
        second: &'static DensityFunction,
    },
    Clamp {
        input: &'static DensityFunction,
        min: f32,
        max: f32,
    },
    RangeChoice {
        input: &'static DensityFunction,
        min_inclusive: f32,
        max_exclusive: f32,
        when_in_range: &'static DensityFunction,
        when_out_of_range: &'static DensityFunction,
    },
    IntervalSelect {
        input: &'static DensityFunction,
        thresholds: &'static [f32],
        functions: &'static [DensityFunction],
    },
    Cache(&'static DensityFunction),
    BlendDensity(&'static DensityFunction),
    Interpolated {
        input: &'static DensityFunction,
        cell_xz: i32,
        cell_y: i32,
    },
    Slice {
        axis: Axis,
        coordinate: i32,
        input: &'static DensityFunction,
    },
    FindTopSurface {
        density: &'static DensityFunction,
        upper_bound: &'static DensityFunction,
        lower_bound: i32,
        cell_height: i32,
    },
    OldBlendedNoise {
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    },
    BlendAlpha,
    BlendOffset,
    Beardifier,
}

#[derive(Debug)]
pub enum Spline {
    Constant(f32),
    Multipoint {
        coordinate: &'static DensityFunction,
        points: &'static [SplinePoint],
    },
}

#[derive(Debug)]
pub struct SplinePoint {
    pub location: f32,
    pub value: Spline,
    pub derivative: f32,
}

#[derive(Debug)]
pub struct NoiseRouter {
    pub temperature: &'static DensityFunction,
    pub vegetation: &'static DensityFunction,
    pub continents: &'static DensityFunction,
    pub erosion: &'static DensityFunction,
    pub depth: &'static DensityFunction,
    pub ridges: &'static DensityFunction,
    pub chunk_surface_level: &'static DensityFunction,
    pub final_density: &'static DensityFunction,
}

#[derive(Debug)]
pub struct Aquifers {
    pub barrier: &'static DensityFunction,
    pub fluid_level_floodedness: &'static DensityFunction,
    pub fluid_level_spread: &'static DensityFunction,
    pub lava: &'static DensityFunction,
    pub exclusion: &'static DensityFunction,
    pub surface_level: &'static DensityFunction,
}

/// A climate parameter range, already quantized like the game's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClimateParameter {
    pub min: i64,
    pub max: i64,
}

#[derive(Debug)]
pub struct SpawnTarget {
    /// `(function, range)` in the data's order.
    pub parameters: &'static [(&'static NamedDensityFunction, ClimateParameter)],
}

/// A `worldgen/noise_settings` entry.
#[derive(Debug)]
pub struct NoiseSettings {
    pub name: &'static str,
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
    pub default_block: BlockState,
    pub default_fluid: BlockState,
    pub legacy_random_source: bool,
    pub disable_mob_generation: bool,
    pub router: NoiseRouter,
    pub aquifers: Option<Aquifers>,
    pub material_rule: &'static MaterialRule,
    pub spawn_target: &'static [SpawnTarget],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAnchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
    RelativeToSeaLevel(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaveSurface {
    Floor,
    Ceiling,
}

#[derive(Debug)]
pub struct NamedMaterialRule {
    pub name: &'static str,
    pub rule: MaterialRule,
}

#[derive(Debug)]
pub struct NamedMaterialCondition {
    pub name: &'static str,
    pub condition: MaterialCondition,
}

#[derive(Debug)]
pub enum MaterialRule {
    Reference(&'static NamedMaterialRule),
    Block(BlockState),
    Sequence(&'static [MaterialRule]),
    Condition {
        if_true: &'static MaterialCondition,
        then_run: &'static MaterialRule,
    },
    Bandlands,
    OreVein {
        ore_block: BlockState,
        raw_ore_block: BlockState,
        filler_block: BlockState,
        raw_ore_chance: f32,
        density: &'static DensityFunction,
        richness: &'static DensityFunction,
        filler_gap: &'static DensityFunction,
    },
}

#[derive(Debug)]
pub enum MaterialCondition {
    Reference(&'static NamedMaterialCondition),
    Biome(&'static [&'static str]),
    NoiseThreshold {
        noise: &'static Noise,
        min_threshold: f64,
        max_threshold: f64,
        is_3d: bool,
    },
    VerticalGradient {
        random_name: &'static str,
        true_at_and_below: VerticalAnchor,
        false_at_and_above: VerticalAnchor,
    },
    YAbove {
        anchor: VerticalAnchor,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    Water {
        offset: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_depth_range: i32,
        surface_type: CaveSurface,
    },
    Not(&'static MaterialCondition),
    Steep,
    Hole,
    AbovePreliminarySurface,
    Temperature,
}

/// One entry of a multi-noise parameter list.
#[derive(Debug)]
pub struct ClimatePoint {
    pub biome: &'static str,
    pub temperature: ClimateParameter,
    pub humidity: ClimateParameter,
    pub continentalness: ClimateParameter,
    pub erosion: ClimateParameter,
    pub depth: ClimateParameter,
    pub weirdness: ClimateParameter,
    pub offset: i64,
}

impl Noise {
    pub fn get(name: &str) -> Option<&'static Noise> {
        crate::lookup(NOISES, name)
    }
}

impl NamedDensityFunction {
    pub fn get(name: &str) -> Option<&'static NamedDensityFunction> {
        crate::lookup(DENSITY_FUNCTIONS, name)
    }
}

impl NoiseSettings {
    pub fn get(name: &str) -> Option<&'static NoiseSettings> {
        crate::lookup(NOISE_SETTINGS, name)
    }
}

impl NamedMaterialCondition {
    pub fn get(name: &str) -> Option<&'static NamedMaterialCondition> {
        crate::lookup(MATERIAL_CONDITIONS, name)
    }
}

impl NamedMaterialRule {
    pub fn get(name: &str) -> Option<&'static NamedMaterialRule> {
        crate::lookup(MATERIAL_RULES, name)
    }
}

/// The multi-noise biome source parameter list of a preset.
pub fn parameter_list(name: &str) -> Option<&'static [ClimatePoint]> {
    crate::lookup(PARAMETER_LISTS, name)
}

include!(concat!(env!("OUT_DIR"), "/worldgen.rs"));

/// A JSON object of the built-in pack's noise, density function, material
/// rule and condition, and noise settings files by path below `data/`
/// (`minecraft/worldgen/noise/surface.json`). Written by the build for
/// tests; not shipped.
pub const PACK_JSON_PATH: &str = concat!(env!("OUT_DIR"), "/worldgen_pack.json");

#[derive(Debug)]
pub enum IntProvider {
    Constant(i32),
    Uniform {
        min: i32,
        max: i32,
    },
    BiasedToBottom {
        min: i32,
        max: i32,
    },
    VeryBiasedToBottom {
        min: i32,
        max: i32,
    },
    Clamped {
        source: &'static IntProvider,
        min: i32,
        max: i32,
    },
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
    WeightedList(&'static [(&'static IntProvider, i32)]),
}

#[derive(Debug)]
pub enum FloatProvider {
    Constant(f32),
    Uniform {
        min: f32,
        max: f32,
    },
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

#[derive(Debug)]
pub enum HeightProvider {
    Constant(VerticalAnchor),
    Uniform {
        min: VerticalAnchor,
        max: VerticalAnchor,
    },
    BiasedToBottom {
        min: VerticalAnchor,
        max: VerticalAnchor,
        inner: i32,
    },
    VeryBiasedToBottom {
        min: VerticalAnchor,
        max: VerticalAnchor,
        inner: i32,
    },
    Trapezoid {
        min: VerticalAnchor,
        max: VerticalAnchor,
        plateau: i32,
    },
    WeightedList(&'static [(&'static HeightProvider, i32)]),
}

/// A `worldgen/configured_carver` entry.
#[derive(Debug)]
pub struct Carver {
    pub name: &'static str,
    pub kind: &'static str,
    pub config: CarverConfig,
}

#[derive(Debug)]
pub enum CarverConfig {
    Cave {
        probability: f32,
        y: &'static HeightProvider,
        count: &'static IntProvider,
        thickness: &'static FloatProvider,
        weird_thickness_bias: bool,
        room_vertical_radius_multiplier: &'static FloatProvider,
        horizontal_radius_multiplier: &'static FloatProvider,
        vertical_radius_multiplier: &'static FloatProvider,
        start_vertical_radius_multiplier: &'static FloatProvider,
        floor_level: &'static FloatProvider,
    },
    Canyon {
        probability: f32,
        y: &'static HeightProvider,
        vertical_rotation: &'static FloatProvider,
        distance_factor: &'static FloatProvider,
        thickness: &'static FloatProvider,
        width_smoothness: i32,
        horizontal_radius_factor: &'static FloatProvider,
        vertical_radius_default_factor: f32,
        vertical_radius_center_factor: f32,
        y_scale: &'static FloatProvider,
    },
}

impl Carver {
    pub fn get(name: &str) -> Option<&'static Carver> {
        crate::lookup(CARVERS, name)
    }
}
