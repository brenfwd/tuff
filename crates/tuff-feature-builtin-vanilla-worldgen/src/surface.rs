//! Material rules (the data-driven surface rules): the per-column scan that
//! tracks stone depth and water, the rule tree evaluated per block, and the
//! eroded badlands and frozen ocean extensions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tuff_vanilla_data::worldgen::{CaveSurface, MaterialCondition, MaterialRule, VerticalAnchor};
use tuff_vanilla_data::{Block, BlockState};

use crate::biome_zoom::BiomeLookup;
use crate::density::{Buf, Context, DensityVolume, Sampler};
use crate::noise::NoiseStack;
use crate::proto::{HeightmapKind, ProtoChunk};
use crate::state::RandomState;
use crate::temperature;
use tuff_item::random::{PositionalRandom, Random, RandomSource};
use tuff_world_model::chunk::{biome_id, is_air};

const WAY_BELOW_MIN_Y: i32 = -2032 * 2;

#[inline]
fn is_fluid(state: BlockState) -> bool {
    state.flags() & BlockState::FLAG_SURFACE_FLUID != 0
}

/// The vertical span world generation works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenContext {
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
}

impl GenContext {
    pub fn resolve(&self, anchor: VerticalAnchor) -> i32 {
        match anchor {
            VerticalAnchor::Absolute(y) => y,
            VerticalAnchor::AboveBottom(o) => self.min_y + o,
            VerticalAnchor::BelowTop(o) => self.height - 1 + self.min_y - o,
            VerticalAnchor::RelativeToSeaLevel(o) => self.sea_level + o,
        }
    }
}

/// Per-dimension state of the material rules: noises, the badlands clay
/// bands, and the compiled density functions the rules read.
pub struct MaterialSystem {
    /// Off under the debug switch that places no ore veins.
    pub ore_veins: bool,
    /// Walks the rule tree instead of running the compiled op list (loaded
    /// settings with compiling off, and differential tests).
    pub interpret: bool,
    default_block: BlockState,
    sea_level: i32,
    clay_bands: [BlockState; BAND_COUNT],
    clay_bands_offset: Arc<NoiseStack>,
    surface: Arc<NoiseStack>,
    surface_secondary: Arc<NoiseStack>,
    badlands_pillar: Arc<NoiseStack>,
    badlands_pillar_roof: Arc<NoiseStack>,
    badlands_surface: Arc<NoiseStack>,
    iceberg_pillar: Arc<NoiseStack>,
    iceberg_pillar_roof: Arc<NoiseStack>,
    iceberg_surface: Arc<NoiseStack>,
    noise_random: PositionalRandom,
    preliminary_surface: Arc<Sampler>,
    rule: &'static MaterialRule,
    /// Noises and densities the rule tree references, by name or address.
    noises: HashMap<&'static str, Arc<NoiseStack>>,
    densities: HashMap<usize, Arc<Sampler>>,
    random_factories: HashMap<&'static str, PositionalRandom>,
    templates: Mutex<Vec<(GenContext, Arc<RuleTemplate>)>>,
    first_rule_cache: usize,
    eroded_badlands: u16,
    frozen_ocean: u16,
    deep_frozen_ocean: u16,
    pack_biomes: Option<Arc<crate::loaded::biomes::PackBiomes>>,
}

const BAND_COUNT: usize = 192;

/// Colored stripe passes laid over the plain terracotta, in draw order:
/// the color and the minimum stripe thickness.
const STRIPE_PASSES: [(Block, i32); 3] = [
    (Block::YellowTerracotta, 1),
    (Block::BrownTerracotta, 2),
    (Block::RedTerracotta, 1),
];

fn generate_bands(random: &mut Random) -> [BlockState; BAND_COUNT] {
    let mut bands = [Block::Terracotta.default_state(); BAND_COUNT];
    scatter_orange(random, &mut bands);
    for (block, thickness) in STRIPE_PASSES {
        paint_stripes(random, &mut bands, thickness, block.default_state());
    }
    paint_white_lines(random, &mut bands);
    bands
}

fn scatter_orange(random: &mut Random, bands: &mut [BlockState; BAND_COUNT]) {
    let orange = Block::OrangeTerracotta.default_state();
    let mut gap_start = 0usize;
    while gap_start < BAND_COUNT {
        let mark = gap_start + 1 + random.next_int_bounded(5) as usize;
        if let Some(slot) = bands.get_mut(mark) {
            *slot = orange;
        }
        gap_start = mark + 1;
    }
}

fn paint_stripes(
    random: &mut Random,
    bands: &mut [BlockState; BAND_COUNT],
    thickness: i32,
    color: BlockState,
) {
    let stripes = random.next_int_between_inclusive(6, 15);
    for _ in 0..stripes {
        let span = (thickness + random.next_int_bounded(3)) as usize;
        let from = random.next_int_bounded(BAND_COUNT as i32) as usize;
        let to = (from + span).min(BAND_COUNT);
        bands[from..to].fill(color);
    }
}

fn paint_white_lines(random: &mut Random, bands: &mut [BlockState; BAND_COUNT]) {
    let white = Block::WhiteTerracotta.default_state();
    let gray = Block::LightGrayTerracotta.default_state();
    let lines = random.next_int_between_inclusive(9, 15);
    let mut row = 0usize;
    for _ in 0..lines {
        if row >= BAND_COUNT {
            break;
        }
        bands[row] = white;
        if row >= 2 && random.next_bool() {
            bands[row - 1] = gray;
        }
        if row + 1 < BAND_COUNT && random.next_bool() {
            bands[row + 1] = gray;
        }
        row += random.next_int_bounded(16) as usize + 4;
    }
}

impl MaterialSystem {
    pub fn new(
        state: &mut RandomState,
        rule: &'static MaterialRule,
        default_block: BlockState,
        sea_level: i32,
    ) -> Self {
        let preliminary_surface = state.router.chunk_surface_level.clone();
        let noise_random = state.random;
        let mut noise = |name: &str| {
            let noise = state.options().noise(name).expect("vanilla noise");
            let holder = state.loader().noise(noise);
            state.compiler().noise(&holder)
        };
        let clay_bands_offset = noise("minecraft:clay_bands_offset");
        let surface = noise("minecraft:surface");
        let surface_secondary = noise("minecraft:surface_secondary");
        let badlands_pillar = noise("minecraft:badlands_pillar");
        let badlands_pillar_roof = noise("minecraft:badlands_pillar_roof");
        let badlands_surface = noise("minecraft:badlands_surface");
        let iceberg_pillar = noise("minecraft:iceberg_pillar");
        let iceberg_pillar_roof = noise("minecraft:iceberg_pillar_roof");
        let iceberg_surface = noise("minecraft:iceberg_surface");
        let clay_bands = generate_bands(&mut noise_random.from_hash_of("minecraft:clay_bands"));
        let first_rule_cache = state.compiler().cache_count();
        let interpret = state.options().interpret;
        let mut system = Self {
            ore_veins: true,
            interpret,
            default_block,
            sea_level,
            clay_bands,
            clay_bands_offset,
            surface,
            surface_secondary,
            badlands_pillar,
            badlands_pillar_roof,
            badlands_surface,
            iceberg_pillar,
            iceberg_pillar_roof,
            iceberg_surface,
            noise_random,
            preliminary_surface,
            rule,
            noises: HashMap::new(),
            densities: HashMap::new(),
            random_factories: HashMap::new(),
            templates: Mutex::new(Vec::new()),
            first_rule_cache,
            eroded_badlands: biome_id("minecraft:eroded_badlands").expect("biome"),
            frozen_ocean: biome_id("minecraft:frozen_ocean").expect("biome"),
            deep_frozen_ocean: biome_id("minecraft:deep_frozen_ocean").expect("biome"),
            pack_biomes: state.options().biomes.clone().filter(|b| !b.is_empty()),
        };
        system.prepare_rule(state, rule);
        system
    }

    /// The temperature of `biome` at a block, pack-added biomes included.
    pub(crate) fn temperature(&self, biome: u16, x: i32, y: i32, z: i32) -> f32 {
        if let Some(added) = self.pack_biomes.as_ref().and_then(|b| b.added(biome)) {
            return temperature::Column::of_data(added.data, x, z, self.sea_level).at(y);
        }
        temperature::at(biome, x, y, z, self.sea_level)
    }

    fn prepare_rule(&mut self, state: &mut RandomState, rule: &'static MaterialRule) {
        match rule {
            MaterialRule::Reference(named) => self.prepare_rule(state, &named.rule),
            MaterialRule::Block(_) | MaterialRule::Bandlands => {}
            MaterialRule::Sequence(rules) => {
                for rule in *rules {
                    self.prepare_rule(state, rule);
                }
            }
            MaterialRule::Condition { if_true, then_run } => {
                self.prepare_condition(state, if_true);
                self.prepare_rule(state, then_run);
            }
            MaterialRule::OreVein {
                density,
                richness,
                filler_gap,
                ..
            } => {
                for function in [*density, *richness, *filler_gap] {
                    let key = function as *const _ as usize;
                    self.densities
                        .entry(key)
                        .or_insert_with(|| state.compile(function));
                }
                self.random_factories
                    .entry("minecraft:ore")
                    .or_insert_with(|| state.random_factory("minecraft:ore"));
            }
        }
    }

    fn prepare_condition(
        &mut self,
        state: &mut RandomState,
        condition: &'static MaterialCondition,
    ) {
        match condition {
            MaterialCondition::Reference(named) => self.prepare_condition(state, &named.condition),
            MaterialCondition::Not(inner) => self.prepare_condition(state, inner),
            MaterialCondition::NoiseThreshold { noise, .. } => {
                if !self.noises.contains_key(noise.name) {
                    let holder = state.loader().noise(noise);
                    let stack = state.compiler().noise(&holder);
                    self.noises.insert(noise.name, stack);
                }
            }
            MaterialCondition::VerticalGradient { random_name, .. } => {
                self.random_factories
                    .entry(random_name)
                    .or_insert_with(|| state.random_factory(random_name));
            }
            _ => {}
        }
    }

    /// The rule compiled for `span`, shared by every chunk of that span.
    pub(crate) fn template(&self, span: GenContext) -> Arc<RuleTemplate> {
        let mut templates = self
            .templates
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((_, template)) = templates.iter().find(|(s, _)| *s == span) {
            return template.clone();
        }
        let template = Arc::new(TemplateBuilder::new(self, span).build(self.rule));
        templates.push((span, template.clone()));
        template
    }

    fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let noise = self.surface.get(x as f64, 0.0, z as f64) as f64;
        (noise * 2.75 + 3.0 + self.noise_random.at(x, 0, z).next_double() * 0.25) as i32
    }

    fn surface_secondary(&self, x: i32, z: i32) -> f64 {
        self.surface_secondary.get(x as f64, 0.0, z as f64) as f64
    }

    fn band(&self, x: i32, y: i32, z: i32) -> BlockState {
        let offset = java_round(self.clay_bands_offset.get(x as f64, 0.0, z as f64) * 4.0);
        let len = self.clay_bands.len() as i32;
        self.clay_bands[((y + offset + len) % len) as usize]
    }

    /// Applies the rules to a chunk whose terrain is filled.
    pub fn build_surface(
        &self,
        ctx: &mut Context,
        span: GenContext,
        chunk: &mut ProtoChunk,
        biomes: &mut dyn BiomeLookup,
        noise_volume: &DensityVolume,
        apply_rules: bool,
    ) {
        let min_y = chunk.shape.min_y;
        let max_y = chunk.shape.min_y + chunk.shape.height - 1;
        let top_section = chunk.highest_filled_section();
        let max_block_y = match top_section {
            Some(index) => chunk.shape.section_base(index) + 15,
            None => chunk.shape.min_y - 1,
        };
        let narrowed = DensityVolume::blocks(
            [
                noise_volume.size_x,
                (max_block_y - noise_volume.min_y + 1).max(1),
                noise_volume.size_z,
            ],
            [noise_volume.min_x, noise_volume.min_y, noise_volume.min_z],
        );
        let template = self.template(span);
        let mut rules = RuleContext::new(self, &template, narrowed);
        let mut compiled = CompiledRule::bind_chunk(ctx, template, &narrowed);
        for x in 0..16 {
            for z in 0..16 {
                let bx = chunk.min_x() + x;
                let bz = chunk.min_z() + z;
                let starting_height = chunk.height_of(HeightmapKind::WorldSurfaceWg, x, z) + 1;
                let surface_biome = biomes.biome(bx, starting_height, bz);
                if surface_biome == self.eroded_badlands {
                    self.eroded_badlands_extension(chunk, x, z, bx, bz, starting_height);
                }
                let height = chunk.height_of(HeightmapKind::WorldSurfaceWg, x, z) + 1;
                let gradient_x = gradient(chunk, x, z, true);
                let gradient_z = gradient(chunk, x, z, false);
                rules.update_xz(bx, bz, gradient_x, gradient_z);
                let mut stone_above = 0;
                let mut water_height = i32::MIN;
                let mut next_ceiling = i32::MAX;
                let end_y = chunk.shape.min_y;
                let mut y = height;
                while y >= end_y {
                    let old = chunk.get(x, y, z);
                    if is_air(old) {
                        stone_above = 0;
                        water_height = i32::MIN;
                    } else if is_fluid(old) {
                        if water_height == i32::MIN {
                            water_height = y + 1;
                        }
                    } else {
                        if next_ceiling >= y {
                            next_ceiling = WAY_BELOW_MIN_Y;
                            let mut look = y - 1;
                            while look >= end_y - 1 {
                                let next = chunk.get(x, look, z);
                                if is_air(next) || is_fluid(next) {
                                    next_ceiling = look + 1;
                                    break;
                                }
                                look -= 1;
                            }
                        }
                        stone_above += 1;
                        let stone_below = y - next_ceiling + 1;
                        rules.update_y(stone_above, stone_below, water_height, y);
                        if apply_rules
                            && y >= min_y
                            && y <= max_y
                            && let Some(state) = rules.apply(ctx, biomes, &mut compiled)
                        {
                            chunk.set(x, y, z, state);
                            if is_fluid(state) {
                                chunk.post_processing.push((bx, y, bz));
                            }
                        }
                    }
                    y -= 1;
                }
                if surface_biome == self.frozen_ocean || surface_biome == self.deep_frozen_ocean {
                    let min_surface = rules.min_surface_level(ctx);
                    self.frozen_ocean_extension(
                        chunk,
                        x,
                        z,
                        bx,
                        bz,
                        starting_height,
                        min_surface,
                        surface_biome,
                    );
                }
            }
        }
        compiled.release(ctx);
        rules.release(ctx);
    }

    /// Rules for picking the top block at single positions.
    pub fn point_rules(&self, span: GenContext) -> PointRules<'_> {
        let template = self.template(span);
        let volume = DensityVolume::blocks([1, 1, 1], [0, 0, 0]);
        PointRules {
            lazy: template.lazy_ore,
            rules: RuleContext::new(self, &template, volume),
            compiled: CompiledRule::unbound(template),
        }
    }

    fn eroded_badlands_extension(
        &self,
        chunk: &mut ProtoChunk,
        x: i32,
        z: i32,
        bx: i32,
        bz: i32,
        height: i32,
    ) {
        let spire = Spire {
            spread: &self.badlands_surface,
            pillar: &self.badlands_pillar,
            roof: &self.badlands_pillar_roof,
            pillar_freq: 0.2,
            roof_freq: 0.75,
        };
        let Some((width, roof)) = spire.sample(bx, bz, 0.0) else {
            return;
        };
        let peak = 64.0 + (width * width * 2.5).min(24.0 + (50.0 * roof).ceil());
        let peak_y = peak.floor() as i32;
        if height > peak_y {
            return;
        }
        let column = (chunk.shape.min_y..=peak_y).rev();
        let first_hit = column
            .clone()
            .map(|y| chunk.get(x, y, z).block())
            .find(|b| *b == self.default_block.block() || *b == Block::Water);
        if first_hit == Some(Block::Water) {
            return;
        }
        for y in column {
            if !is_air(chunk.get(x, y, z)) {
                break;
            }
            chunk.set(x, y, z, self.default_block);
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn frozen_ocean_extension(
        &self,
        chunk: &mut ProtoChunk,
        x: i32,
        z: i32,
        bx: i32,
        bz: i32,
        height: i32,
        min_surface: i32,
        biome: u16,
    ) {
        let spire = Spire {
            spread: &self.iceberg_surface,
            pillar: &self.iceberg_pillar,
            roof: &self.iceberg_pillar_roof,
            pillar_freq: 1.28,
            roof_freq: 1.17,
        };
        let Some((width, roof)) = spire.sample(bx, bz, 1.8) else {
            return;
        };
        let warm = self.temperature(biome, bx, self.sea_level, bz) > 0.1;
        let rise =
            (width * width * 1.2).min(14.0 + (40.0 * roof).ceil()) - if warm { 2.0 } else { 0.0 };
        if rise <= 2.0 {
            return;
        }
        let sea = self.sea_level as f64;
        let ceiling = (sea + rise) as i32;
        let floor = (sea - rise - 7.0) as i32;
        let mut rng = self.noise_random.at(bx, 0, bz);
        let snow_cap = 2 + rng.next_int_bounded(4);
        let snow_line = self.sea_level + 18 + rng.next_int_bounded(10);
        let writable = chunk.shape.min_y..chunk.shape.min_y + chunk.shape.height;
        let mut snow_laid = 0;
        for y in (min_surface..=height.max(ceiling + 1)).rev() {
            let here = chunk.get(x, y, z);
            let skip_chance = if is_air(here) && y < ceiling {
                0.01
            } else if here.block() == Block::Water && y > floor && y < self.sea_level {
                0.15
            } else {
                continue;
            };
            if rng.next_double() <= skip_chance || !writable.contains(&y) {
                continue;
            }
            let snowy = y > snow_line && snow_laid <= snow_cap;
            snow_laid += i32::from(snowy);
            let block = if snowy {
                Block::SnowBlock
            } else {
                Block::PackedIce
            };
            chunk.set(x, y, z, block.default_state());
        }
    }
}

/// A noise-shaped spire over one column: the spread noise, clipped by a
/// pillar mask, sets its width, and a roof noise caps its height.
struct Spire<'a> {
    spread: &'a NoiseStack,
    pillar: &'a NoiseStack,
    roof: &'a NoiseStack,
    pillar_freq: f64,
    roof_freq: f64,
}

impl Spire<'_> {
    /// Width and roof strength, or `None` when the width does not exceed `min_width`.
    #[inline]
    fn sample(&self, bx: i32, bz: i32, min_width: f64) -> Option<(f64, f64)> {
        let (fx, fz) = (f64::from(bx), f64::from(bz));
        let mask = f64::from(
            15.0 * self
                .pillar
                .get(fx * self.pillar_freq, 0.0, fz * self.pillar_freq),
        );
        let width = (8.25 * f64::from(self.spread.get(fx, 0.0, fz)))
            .abs()
            .min(mask);
        if width <= min_width {
            return None;
        }
        let roof = self.roof.get(fx * self.roof_freq, 0.0, fz * self.roof_freq);
        Some((width, (1.5 * f64::from(roof)).abs()))
    }
}

fn java_round(v: f32) -> i32 {
    (v as f64 + 0.5).floor() as i32
}

fn gradient(chunk: &ProtoChunk, x: i32, z: i32, along_x: bool) -> i32 {
    let h = |x, z| chunk.height_of(HeightmapKind::WorldSurfaceWg, x, z);
    if along_x {
        h((x + 1).min(15), z) - h((x - 1).max(0), z)
    } else {
        h(x, (z + 1).min(15)) - h(x, (z - 1).max(0))
    }
}

/// A rule tree compiled for one span into a flat op list: a failed test
/// jumps past the rule it guarded, the first block produced returns, and
/// running off the end produces nothing. Nothing in it depends on a chunk.
pub struct RuleTemplate {
    root: &'static MaterialRule,
    span: GenContext,
    ops: Vec<Op>,
    veins: Vec<VeinTemplate>,
    noise_slots: usize,
    lazy_ore: bool,
}

enum Op {
    Return(BlockState),
    Bandlands,
    Test {
        condition: CompiledCondition,
        negate: bool,
        on_false: usize,
    },
    OreVein(usize),
}

struct VeinTemplate {
    ore: BlockState,
    raw_ore: BlockState,
    filler: BlockState,
    raw_ore_chance: f32,
    density: Arc<Sampler>,
    richness: Arc<Sampler>,
    gap: Arc<Sampler>,
    random: PositionalRandom,
}

/// A set of biome ids as a bitmask.
pub struct BiomeSet(Box<[u64]>);

impl BiomeSet {
    fn new(ids: impl Iterator<Item = u16>) -> Self {
        let mut words = Vec::new();
        for id in ids {
            let word = id as usize / 64;
            if words.len() <= word {
                words.resize(word + 1, 0u64);
            }
            words[word] |= 1 << (id % 64);
        }
        Self(words.into())
    }

    #[inline]
    fn contains(&self, id: u16) -> bool {
        self.0
            .get(id as usize / 64)
            .is_some_and(|word| word >> (id % 64) & 1 == 1)
    }
}

pub enum CompiledCondition {
    Biome(BiomeSet),
    Noise {
        noise: Arc<NoiseStack>,
        min: f64,
        max: f64,
        is_3d: bool,
        slot: usize,
    },
    VerticalGradient {
        random: PositionalRandom,
        true_at_and_below: i32,
        false_at_and_above: i32,
    },
    YAbove {
        anchor_y: i32,
        multiplier: i32,
        add_stone_depth: bool,
    },
    Water {
        offset: i32,
        multiplier: i32,
        add_stone_depth: bool,
    },
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_range: i32,
        ceiling: bool,
    },
    Steep,
    Hole,
    AbovePreliminarySurface,
    Temperature,
}

struct TemplateBuilder<'a> {
    system: &'a MaterialSystem,
    span: GenContext,
    ops: Vec<Op>,
    veins: Vec<VeinTemplate>,
    flat_keys: Vec<usize>,
}

impl<'a> TemplateBuilder<'a> {
    fn new(system: &'a MaterialSystem, span: GenContext) -> Self {
        Self {
            system,
            span,
            ops: Vec::new(),
            veins: Vec::new(),
            flat_keys: Vec::new(),
        }
    }

    fn build(mut self, rule: &'static MaterialRule) -> RuleTemplate {
        self.emit_rule(rule);
        let lazy_ore = self.lazy_ore();
        RuleTemplate {
            root: rule,
            span: self.span,
            ops: self.ops,
            veins: self.veins,
            noise_slots: self.flat_keys.len(),
            lazy_ore,
        }
    }

    /// Whether skipping a vein's volume evaluations cannot change any
    /// later result: the ore programs only touch cache cells numbered
    /// after the rest of the world's functions were compiled (so nothing
    /// else reads them), and the gap samplers share none of them.
    fn lazy_ore(&self) -> bool {
        let mut ore = Vec::new();
        for vein in &self.veins {
            for sampler in [&vein.density, &vein.richness] {
                match sampler.cache_ids() {
                    Some(ids) => ore.extend(ids),
                    None => return false,
                }
            }
        }
        if ore.iter().any(|id| *id < self.system.first_rule_cache) {
            return false;
        }
        self.veins.iter().all(|vein| {
            vein.gap
                .cache_ids()
                .is_some_and(|ids| ids.iter().all(|id| !ore.contains(id)))
        })
    }

    fn emit_rule(&mut self, rule: &'static MaterialRule) {
        match rule {
            MaterialRule::Reference(named) => self.emit_rule(&named.rule),
            MaterialRule::Block(state) => self.ops.push(Op::Return(*state)),
            MaterialRule::Sequence(rules) => {
                for rule in *rules {
                    self.emit_rule(rule);
                }
            }
            MaterialRule::Condition { if_true, then_run } => {
                let (condition, negate) = self.compile_condition(if_true);
                let at = self.ops.len();
                self.ops.push(Op::Test {
                    condition,
                    negate,
                    on_false: 0,
                });
                self.emit_rule(then_run);
                let end = self.ops.len();
                if let Op::Test { on_false, .. } = &mut self.ops[at] {
                    *on_false = end;
                }
            }
            MaterialRule::Bandlands => self.ops.push(Op::Bandlands),
            MaterialRule::OreVein {
                ore_block,
                raw_ore_block,
                filler_block,
                raw_ore_chance,
                density,
                richness,
                filler_gap,
            } => {
                if !self.system.ore_veins {
                    return;
                }
                let sampler = |f: &'static tuff_vanilla_data::worldgen::DensityFunction| {
                    self.system.densities[&(f as *const _ as usize)].clone()
                };
                let vein = VeinTemplate {
                    ore: *ore_block,
                    raw_ore: *raw_ore_block,
                    filler: *filler_block,
                    raw_ore_chance: *raw_ore_chance,
                    density: sampler(density),
                    richness: sampler(richness),
                    gap: sampler(filler_gap),
                    random: self.system.random_factories["minecraft:ore"],
                };
                self.ops.push(Op::OreVein(self.veins.len()));
                self.veins.push(vein);
            }
        }
    }

    fn compile_condition(
        &mut self,
        condition: &'static MaterialCondition,
    ) -> (CompiledCondition, bool) {
        match condition {
            MaterialCondition::Not(inner) => {
                let (condition, negate) = self.compile_condition(inner);
                (condition, !negate)
            }
            MaterialCondition::Reference(named) => self.compile_condition(&named.condition),
            _ => (self.compile_leaf(condition), false),
        }
    }

    fn noise_slot(&mut self, noise: &Arc<NoiseStack>) -> usize {
        let key = Arc::as_ptr(noise) as *const u8 as usize;
        match self.flat_keys.iter().position(|k| *k == key) {
            Some(slot) => slot,
            None => {
                self.flat_keys.push(key);
                self.flat_keys.len() - 1
            }
        }
    }

    fn compile_leaf(&mut self, condition: &'static MaterialCondition) -> CompiledCondition {
        match condition {
            MaterialCondition::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
                is_3d,
            } => {
                let noise = self.system.noises[noise.name].clone();
                let slot = self.noise_slot(&noise);
                CompiledCondition::Noise {
                    noise,
                    min: *min_threshold,
                    max: *max_threshold,
                    is_3d: *is_3d,
                    slot,
                }
            }
            _ => leaf_condition(self.system, self.span, condition)
                .expect("not, references and noise are handled above"),
        }
    }
}

/// A leaf condition other than a noise threshold, bound to `span`.
fn leaf_condition(
    system: &MaterialSystem,
    span: GenContext,
    condition: &'static MaterialCondition,
) -> Option<CompiledCondition> {
    Some(match condition {
        MaterialCondition::Not(_)
        | MaterialCondition::Reference(_)
        | MaterialCondition::NoiseThreshold { .. } => return None,
        MaterialCondition::Biome(names) => CompiledCondition::Biome(BiomeSet::new(
            names.iter().map(|n| biome_id(n).expect("known biome")),
        )),
        MaterialCondition::VerticalGradient {
            random_name,
            true_at_and_below,
            false_at_and_above,
        } => CompiledCondition::VerticalGradient {
            random: system.random_factories[random_name],
            true_at_and_below: span.resolve(*true_at_and_below),
            false_at_and_above: span.resolve(*false_at_and_above),
        },
        MaterialCondition::YAbove {
            anchor,
            surface_depth_multiplier,
            add_stone_depth,
        } => CompiledCondition::YAbove {
            anchor_y: span.resolve(*anchor),
            multiplier: *surface_depth_multiplier,
            add_stone_depth: *add_stone_depth,
        },
        MaterialCondition::Water {
            offset,
            surface_depth_multiplier,
            add_stone_depth,
        } => CompiledCondition::Water {
            offset: *offset,
            multiplier: *surface_depth_multiplier,
            add_stone_depth: *add_stone_depth,
        },
        MaterialCondition::StoneDepth {
            offset,
            add_surface_depth,
            secondary_depth_range,
            surface_type,
        } => CompiledCondition::StoneDepth {
            offset: *offset,
            add_surface_depth: *add_surface_depth,
            secondary_range: *secondary_depth_range,
            ceiling: *surface_type == CaveSurface::Ceiling,
        },
        MaterialCondition::Steep => CompiledCondition::Steep,
        MaterialCondition::Hole => CompiledCondition::Hole,
        MaterialCondition::Temperature => CompiledCondition::Temperature,
        MaterialCondition::AbovePreliminarySurface => CompiledCondition::AbovePreliminarySurface,
    })
}

enum Prefill {
    Buffer(Buf),
    Point(Option<f32>),
}

struct VeinValues {
    density: Prefill,
    richness: Prefill,
}

/// A template bound to the volume being generated: the ore densities of
/// each vein, resolved over the volume up front or on first read.
pub struct CompiledRule {
    template: Arc<RuleTemplate>,
    ore: Vec<VeinValues>,
}

impl CompiledRule {
    fn unbound(template: Arc<RuleTemplate>) -> Self {
        Self {
            template,
            ore: Vec::new(),
        }
    }

    /// Resolves every vein's density and richness over `volume`.
    fn bind_chunk(ctx: &mut Context, template: Arc<RuleTemplate>, volume: &DensityVolume) -> Self {
        let mut rule = Self::unbound(template);
        rule.bind_eager(ctx, volume);
        rule
    }

    fn bind_eager(&mut self, ctx: &mut Context, volume: &DensityVolume) {
        let fill = |sampler: &Sampler, ctx: &mut Context| {
            let mut buffer = ctx.acquire(volume.size());
            sampler.sample_volume(ctx, &mut buffer, volume);
            Prefill::Buffer(buffer)
        };
        for vein in &self.template.veins {
            let density = fill(&vein.density, ctx);
            let richness = fill(&vein.richness, ctx);
            self.ore.push(VeinValues { density, richness });
        }
    }

    /// Rebinds to a one-block volume; the densities are computed when read
    /// if skipping them cannot change any later result.
    fn rebind_point(&mut self, ctx: &mut Context, volume: &DensityVolume, lazy: bool) {
        self.release(ctx);
        if lazy {
            for _ in &self.template.veins {
                self.ore.push(VeinValues {
                    density: Prefill::Point(None),
                    richness: Prefill::Point(None),
                });
            }
        } else {
            self.bind_eager(ctx, volume);
        }
    }

    fn release(&mut self, ctx: &mut Context) {
        for values in self.ore.drain(..) {
            for prefill in [values.density, values.richness] {
                if let Prefill::Buffer(buffer) = prefill {
                    ctx.release(buffer);
                }
            }
        }
    }
}

/// Picks the top block the rules give single exposed positions.
pub struct PointRules<'s> {
    rules: RuleContext<'s>,
    compiled: CompiledRule,
    lazy: bool,
}

impl PointRules<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    pub fn top_material(
        &mut self,
        ctx: &mut Context,
        biomes: &mut dyn BiomeLookup,
        chunk: &ProtoChunk,
        x: i32,
        y: i32,
        z: i32,
        under_fluid: bool,
    ) -> Option<BlockState> {
        let volume = DensityVolume::blocks([1, 1, 1], [x, y, z]);
        self.rules.reposition(volume);
        self.compiled.rebind_point(ctx, &volume, self.lazy);
        let (lx, lz) = (x & 15, z & 15);
        self.rules.update_xz(
            x,
            z,
            gradient(chunk, lx, lz, true),
            gradient(chunk, lx, lz, false),
        );
        self.rules
            .update_y(1, 1, if under_fluid { y + 1 } else { i32::MIN }, y);
        self.rules.apply(ctx, biomes, &mut self.compiled)
    }

    pub fn release(mut self, ctx: &mut Context) {
        self.compiled.release(ctx);
        self.rules.release(ctx);
    }
}

/// The per-chunk state the rules read.
pub struct RuleContext<'s> {
    system: &'s MaterialSystem,
    volume: DensityVolume,
    surface_volume: DensityVolume,
    surface_buffer: Option<Buf>,
    surface_filled: bool,
    x: i32,
    z: i32,
    gradient_x: i32,
    gradient_z: i32,
    surface_depth: i32,
    surface_secondary: Option<f64>,
    min_surface: Option<i32>,
    y: i32,
    water_height: i32,
    stone_below: i32,
    stone_above: i32,
    biome: Option<(i32, u16)>,
    flat_noise: Vec<Option<f64>>,
}

impl<'s> RuleContext<'s> {
    pub(crate) fn new(
        system: &'s MaterialSystem,
        template: &RuleTemplate,
        volume: DensityVolume,
    ) -> Self {
        Self {
            system,
            volume,
            surface_volume: Self::surface_volume_of(&volume),
            surface_buffer: None,
            surface_filled: false,
            x: 0,
            z: 0,
            gradient_x: 0,
            gradient_z: 0,
            surface_depth: 0,
            surface_secondary: None,
            min_surface: None,
            y: 0,
            water_height: 0,
            stone_below: 0,
            stone_above: 0,
            biome: None,
            flat_noise: vec![None; template.noise_slots],
        }
    }

    fn reposition(&mut self, volume: DensityVolume) {
        self.volume = volume;
        self.surface_volume = Self::surface_volume_of(&volume);
        self.surface_filled = false;
    }

    fn release(&mut self, ctx: &mut Context) {
        if let Some(buffer) = self.surface_buffer.take() {
            ctx.release(buffer);
        }
    }

    pub fn update_xz(&mut self, x: i32, z: i32, gradient_x: i32, gradient_z: i32) {
        self.x = x;
        self.z = z;
        self.gradient_x = gradient_x;
        self.gradient_z = gradient_z;
        self.surface_depth = self.system.surface_depth(x, z);
        self.surface_secondary = None;
        self.min_surface = None;
        self.biome = None;
        self.flat_noise.fill(None);
    }

    pub fn update_y(&mut self, stone_above: i32, stone_below: i32, water_height: i32, y: i32) {
        self.y = y;
        self.water_height = water_height;
        self.stone_below = stone_below;
        self.stone_above = stone_above;
    }

    fn secondary(&mut self) -> f64 {
        *self
            .surface_secondary
            .get_or_insert_with(|| self.system.surface_secondary(self.x, self.z))
    }

    pub fn min_surface_level(&mut self, ctx: &mut Context) -> i32 {
        if let Some(level) = self.min_surface {
            return level;
        }
        let system = self.system;
        let level = match self.surface_volume.index_of_block(self.x, 0, self.z) {
            Some(index) => {
                let size = self.surface_volume.size();
                let buffer = self.surface_buffer.get_or_insert_with(|| ctx.acquire(size));
                if !self.surface_filled {
                    system
                        .preliminary_surface
                        .sample_volume(ctx, buffer, &self.surface_volume);
                    self.surface_filled = true;
                }
                buffer[index]
            }
            None => system
                .preliminary_surface
                .sample_value(ctx, self.x, 0, self.z),
        };
        let level = (level as f64).floor() as i32 + self.surface_depth - 8;
        self.min_surface = Some(level);
        level
    }

    pub fn apply(
        &mut self,
        ctx: &mut Context,
        biomes: &mut dyn BiomeLookup,
        rule: &mut CompiledRule,
    ) -> Option<BlockState> {
        if self.system.interpret {
            let CompiledRule { template, ore } = rule;
            let mut veins = 0;
            return self.interpret_rule(ctx, biomes, template, ore, template.root, &mut veins);
        }
        let CompiledRule { template, ore } = rule;
        let ops = &template.ops;
        let mut pc = 0;
        while let Some(op) = ops.get(pc) {
            pc += 1;
            match op {
                Op::Return(state) => return Some(*state),
                Op::Bandlands => return Some(self.system.band(self.x, self.y, self.z)),
                Op::Test {
                    condition,
                    negate,
                    on_false,
                } => {
                    if self.test(ctx, biomes, condition) == *negate {
                        pc = *on_false;
                    }
                }
                Op::OreVein(index) => {
                    let vein = &template.veins[*index];
                    if let Some(state) = self.ore_vein(ctx, vein, &mut ore[*index]) {
                        return Some(state);
                    }
                }
            }
        }
        None
    }

    /// The rule tree walked as it is: the interpreter loaded surface rules
    /// run on when compiling is off. `veins` counts the ore veins met, in
    /// the order the compiled form numbers them.
    fn interpret_rule(
        &mut self,
        ctx: &mut Context,
        biomes: &mut dyn BiomeLookup,
        template: &RuleTemplate,
        ore: &mut [VeinValues],
        rule: &'static MaterialRule,
        veins: &mut usize,
    ) -> Option<BlockState> {
        match rule {
            MaterialRule::Reference(named) => {
                self.interpret_rule(ctx, biomes, template, ore, &named.rule, veins)
            }
            MaterialRule::Block(state) => Some(*state),
            MaterialRule::Bandlands => Some(self.system.band(self.x, self.y, self.z)),
            MaterialRule::Sequence(rules) => {
                for rule in *rules {
                    if let Some(state) =
                        self.interpret_rule(ctx, biomes, template, ore, rule, veins)
                    {
                        return Some(state);
                    }
                }
                None
            }
            MaterialRule::Condition { if_true, then_run } => {
                if self.interpret_condition(ctx, biomes, template.span, if_true) {
                    self.interpret_rule(ctx, biomes, template, ore, then_run, veins)
                } else {
                    *veins += count_veins(then_run, self.system.ore_veins);
                    None
                }
            }
            MaterialRule::OreVein { .. } => {
                if !self.system.ore_veins {
                    return None;
                }
                let index = *veins;
                *veins += 1;
                self.ore_vein(ctx, &template.veins[index], &mut ore[index])
            }
        }
    }

    fn interpret_condition(
        &mut self,
        ctx: &mut Context,
        biomes: &mut dyn BiomeLookup,
        span: GenContext,
        condition: &'static MaterialCondition,
    ) -> bool {
        match condition {
            MaterialCondition::Not(inner) => !self.interpret_condition(ctx, biomes, span, inner),
            MaterialCondition::Reference(named) => {
                self.interpret_condition(ctx, biomes, span, &named.condition)
            }
            MaterialCondition::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
                is_3d,
            } => {
                let noise = &self.system.noises[noise.name];
                let y = if *is_3d { self.y as f64 } else { 0.0 };
                let value = noise.get(self.x as f64, y, self.z as f64) as f64;
                value >= *min_threshold && value <= *max_threshold
            }
            leaf => {
                let compiled = leaf_condition(self.system, span, leaf).expect("a leaf condition");
                self.test(ctx, biomes, &compiled)
            }
        }
    }

    fn ore_vein(
        &mut self,
        ctx: &mut Context,
        vein: &VeinTemplate,
        values: &mut VeinValues,
    ) -> Option<BlockState> {
        let d = self.prefilled(ctx, &vein.density, &mut values.density);
        if d <= 0.0 {
            return None;
        }
        let mut random = vein.random.at(self.x, self.y, self.z);
        if random.next_float() > d {
            return None;
        }
        let r = self.prefilled(ctx, &vein.richness, &mut values.richness);
        if random.next_float() >= r || vein.gap.sample_value(ctx, self.x, self.y, self.z) >= 0.0 {
            return Some(vein.filler);
        }
        Some(if random.next_float() < vein.raw_ore_chance {
            vein.raw_ore
        } else {
            vein.ore
        })
    }

    fn prefilled(&self, ctx: &mut Context, sampler: &Sampler, prefill: &mut Prefill) -> f32 {
        let at = self.volume.index_of_block(self.x, self.y, self.z);
        match (prefill, at) {
            (Prefill::Buffer(buffer), Some(index)) => buffer[index],
            (Prefill::Point(Some(value)), Some(_)) => *value,
            (Prefill::Point(slot), Some(_)) => {
                let mut out = [0.0f32];
                sampler.sample_volume(ctx, &mut out, &self.volume);
                *slot = Some(out[0]);
                out[0]
            }
            (_, None) => sampler.sample_value(ctx, self.x, self.y, self.z),
        }
    }

    fn surface_volume_of(volume: &DensityVolume) -> DensityVolume {
        DensityVolume::blocks(
            [volume.size_x, 1, volume.size_z],
            [volume.min_x, 0, volume.min_z],
        )
    }

    fn current_biome(&mut self, biomes: &mut dyn BiomeLookup) -> u16 {
        let quart_y = self.y;
        match self.biome {
            Some((q, b)) if q == quart_y => b,
            _ => {
                let b = biomes.biome(self.x, self.y, self.z);
                self.biome = Some((quart_y, b));
                b
            }
        }
    }

    pub(crate) fn test(
        &mut self,
        ctx: &mut Context,
        biomes: &mut dyn BiomeLookup,
        condition: &CompiledCondition,
    ) -> bool {
        match condition {
            CompiledCondition::Biome(set) => {
                let biome = self.current_biome(biomes);
                set.contains(biome)
            }
            CompiledCondition::Temperature => {
                let biome = self.current_biome(biomes);
                self.system.temperature(biome, self.x, self.y, self.z) < 0.15
            }
            CompiledCondition::Noise {
                noise,
                min,
                max,
                is_3d,
                slot,
            } => {
                let value = if *is_3d {
                    noise.get(self.x as f64, self.y as f64, self.z as f64) as f64
                } else {
                    match self.flat_noise[*slot] {
                        Some(v) => v,
                        None => {
                            let v = noise.get(self.x as f64, 0.0, self.z as f64) as f64;
                            self.flat_noise[*slot] = Some(v);
                            v
                        }
                    }
                };
                value >= *min && value <= *max
            }
            CompiledCondition::VerticalGradient {
                random,
                true_at_and_below,
                false_at_and_above,
            } => {
                let y = self.y;
                if y <= *true_at_and_below {
                    return true;
                }
                if y >= *false_at_and_above {
                    return false;
                }
                let probability = map(
                    y as f64,
                    *true_at_and_below as f64,
                    *false_at_and_above as f64,
                    1.0,
                    0.0,
                );
                (random.at(self.x, y, self.z).next_float() as f64) < probability
            }
            CompiledCondition::YAbove {
                anchor_y,
                multiplier,
                add_stone_depth,
            } => {
                let stone = if *add_stone_depth {
                    self.stone_above
                } else {
                    0
                };
                self.y + stone >= *anchor_y + self.surface_depth * multiplier
            }
            CompiledCondition::Water {
                offset,
                multiplier,
                add_stone_depth,
            } => {
                if self.water_height == i32::MIN {
                    return true;
                }
                let stone = if *add_stone_depth {
                    self.stone_above
                } else {
                    0
                };
                self.y + stone >= self.water_height + offset + self.surface_depth * multiplier
            }
            CompiledCondition::StoneDepth {
                offset,
                add_surface_depth,
                secondary_range,
                ceiling,
            } => {
                let depth = if *ceiling {
                    self.stone_below
                } else {
                    self.stone_above
                };
                let surface = if *add_surface_depth {
                    self.surface_depth
                } else {
                    0
                };
                let secondary = if *secondary_range == 0 {
                    0
                } else {
                    map(self.secondary(), -1.0, 1.0, 0.0, *secondary_range as f64) as i32
                };
                depth <= 1 + offset + surface + secondary
            }
            CompiledCondition::Steep => self.gradient_x <= -4 || self.gradient_z >= 4,
            CompiledCondition::Hole => self.surface_depth <= 0,
            CompiledCondition::AbovePreliminarySurface => self.y >= self.min_surface_level(ctx),
        }
    }
}

/// The ore veins `rule` holds, as the compiled form numbers them.
fn count_veins(rule: &'static MaterialRule, ore_veins: bool) -> usize {
    match rule {
        MaterialRule::Reference(named) => count_veins(&named.rule, ore_veins),
        MaterialRule::Block(_) | MaterialRule::Bandlands => 0,
        MaterialRule::Sequence(rules) => rules.iter().map(|r| count_veins(r, ore_veins)).sum(),
        MaterialRule::Condition { then_run, .. } => count_veins(then_run, ore_veins),
        MaterialRule::OreVein { .. } => usize::from(ore_veins),
    }
}

fn map(v: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let t = (v - from_min) / (from_max - from_min);
    to_min + t * (to_max - to_min)
}

#[cfg(test)]
mod tests;
