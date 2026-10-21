//! Features: configured features (what to place), placed features (where,
//! through placement modifiers), and decoration, which runs every placed
//! feature of the biomes around a chunk in the game's global order with the
//! game's per-feature seeding.
//!
//! Feature kinds and placement modifier kinds are registries keyed by
//! resource id; the vanilla data (JSON from the data pack, validated at
//! build time) is parsed against them once per generator.

pub mod bonus_chest;
pub mod coral;
pub mod dripstone;
pub mod end;
pub mod geode;
pub mod host;
pub mod huge_mushroom;
pub mod ice;
pub mod kinds;
pub mod level;
pub mod monster_room;
pub mod multiface;
pub mod nether;
pub mod ore;
pub mod placement;
pub mod plants;
pub mod predicate;
pub mod provider;
pub mod rocks;
pub mod sculk;
pub mod template;
pub mod tree;
pub mod value;
pub mod vegetation;
pub mod void_start;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::feature::level::Region;
use crate::feature::placement::Placement;
use crate::feature::provider::StateProvider;
use crate::json::{self, Result};
use crate::loaded::features::FeatureSources;
use crate::structure::{References, Structures};
use tuff_item::random::{Random, WorldgenRandom, XoroshiroRandom};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{biome_id, biome_name};

pub use value::{OwnedFloatProvider, OwnedHeightProvider, OwnedIntProvider};

/// A configured feature.
pub trait Feature: Send + Sync {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool;
}

pub struct PlaceContext<'a, 'g> {
    pub level: &'a mut Region<'g>,
    pub random: &'a mut WorldgenRandom,
}

pub type FeatureFactory = fn(&mut Registry, &Value) -> Result<Arc<dyn Feature>>;

/// A feature with the placement modifiers that choose its positions.
pub struct PlacedFeature {
    pub name: Option<String>,
    pub feature: Arc<dyn Feature>,
    pub placement: Vec<Placement>,
}

impl PlacedFeature {
    /// Runs the placement modifiers depth first, as the game does, and
    /// places the feature at every position they yield. `top` is the
    /// placed feature the biome filter checks against.
    pub fn place(&self, ctx: &mut PlaceContext, origin: BlockPos, top: Option<&str>) -> bool {
        if self.placement.is_empty() {
            return self.feature.place(ctx, origin);
        }
        let mut placed = false;
        let mut stack = vec![(origin, 0usize)];
        let mut produced = Vec::new();
        while let Some((pos, index)) = stack.pop() {
            produced.clear();
            self.placement[index].modify(ctx, top, pos, &mut produced);
            let next = index + 1;
            if next < self.placement.len() {
                for p in produced.iter().rev() {
                    stack.push((*p, next));
                }
            } else {
                for &p in &produced {
                    placed |= self.feature.place(ctx, p);
                }
            }
        }
        placed
    }
}

/// A feature kind this server does not implement yet; placing it does
/// nothing.
struct Missing;

impl Feature for Missing {
    fn place(&self, _: &mut PlaceContext, _: BlockPos) -> bool {
        false
    }
}

/// Feature kinds, placement kinds, and the parsed vanilla definitions.
pub struct Registry {
    kinds: HashMap<String, FeatureFactory>,
    configured: HashMap<String, Arc<dyn Feature>>,
    placed: HashMap<String, Arc<PlacedFeature>>,
    providers: HashMap<String, Arc<StateProvider>>,
    sources: Option<Arc<FeatureSources>>,
    resolving: BTreeSet<String>,
    /// Kinds met in the data that no factory handles.
    pub missing: BTreeSet<String>,
    /// Definitions that failed to parse, with the error.
    pub errors: Vec<(String, String)>,
}

impl Registry {
    pub fn builtin() -> Self {
        Self::with_sources(None)
    }

    /// The registry with data pack features in front of the built-in ones.
    pub fn with_sources(sources: Option<Arc<FeatureSources>>) -> Self {
        let mut registry = Self {
            sources,
            resolving: BTreeSet::new(),
            kinds: HashMap::new(),
            configured: HashMap::new(),
            placed: HashMap::new(),
            providers: HashMap::new(),
            missing: BTreeSet::new(),
            errors: Vec::new(),
        };
        kinds::register(&mut registry);
        ore::register(&mut registry);
        tree::register(&mut registry);
        huge_mushroom::register(&mut registry);
        multiface::register(&mut registry);
        vegetation::register(&mut registry);
        rocks::register(&mut registry);
        monster_room::register(&mut registry);
        bonus_chest::register(&mut registry);
        void_start::register(&mut registry);
        plants::register(&mut registry);
        geode::register(&mut registry);
        ice::register(&mut registry);
        dripstone::register(&mut registry);
        coral::register(&mut registry);
        sculk::register(&mut registry);
        nether::register(&mut registry);
        end::register(&mut registry);
        template::register(&mut registry);
        registry
    }

    pub fn register(&mut self, kind: &str, factory: FeatureFactory) {
        self.kinds.insert(kind.to_string(), factory);
    }

    /// Parses a configured feature: a name or an inline definition.
    pub fn feature(&mut self, value: &Value) -> Result<Arc<dyn Feature>> {
        if let Some(name) = value.as_str() {
            let name = json::qualify(name);
            if let Some(feature) = self.configured.get(&name) {
                return Ok(feature.clone());
            }
            if let Some(json) = self.pack(&name, |s| &s.configured) {
                let feature = self
                    .guarded(format!("configured feature {name}"), |r| r.feature(&json))
                    .map_err(|e| format!("configured feature {name}: {e}"))?;
                self.configured.insert(name, feature.clone());
                return Ok(feature);
            }
            let text = lookup(tuff_vanilla_data::worldgen::CONFIGURED_FEATURES, &name)
                .ok_or_else(|| format!("unknown configured feature {name}"))?;
            let parsed: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let feature = match self.feature(&parsed) {
                Ok(feature) => feature,
                Err(err) => {
                    self.errors.push((name.clone(), err));
                    Arc::new(Missing)
                }
            };
            self.configured.insert(name, feature.clone());
            return Ok(feature);
        }
        let kind = json::qualify(json::kind(value)?);
        match self.kinds.get(&kind).copied() {
            Some(factory) => factory(self, value),
            None => {
                self.missing.insert(kind);
                Ok(Arc::new(Missing))
            }
        }
    }

    /// Parses a placed feature: a name or an inline definition.
    pub fn placed(&mut self, value: &Value) -> Result<Arc<PlacedFeature>> {
        if let Some(name) = value.as_str() {
            let name = json::qualify(name);
            if let Some(placed) = self.placed.get(&name) {
                return Ok(placed.clone());
            }
            if let Some(json) = self.pack(&name, |s| &s.placed) {
                let mut placed = self
                    .guarded(format!("placed feature {name}"), |r| r.parse_placed(&json))
                    .map_err(|e| format!("placed feature {name}: {e}"))?;
                placed.name = Some(name.clone());
                let placed = Arc::new(placed);
                self.placed.insert(name, placed.clone());
                return Ok(placed);
            }
            let text = lookup(tuff_vanilla_data::worldgen::PLACED_FEATURES, &name)
                .ok_or_else(|| format!("unknown placed feature {name}"))?;
            let parsed: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let mut placed = match self.parse_placed(&parsed) {
                Ok(placed) => placed,
                Err(err) => {
                    self.errors.push((name.clone(), err));
                    PlacedFeature {
                        name: None,
                        feature: Arc::new(Missing),
                        placement: Vec::new(),
                    }
                }
            };
            placed.name = Some(name.clone());
            let placed = Arc::new(placed);
            self.placed.insert(name, placed.clone());
            return Ok(placed);
        }
        Ok(Arc::new(self.parse_placed(value)?))
    }

    fn pack(
        &self,
        name: &str,
        table: fn(&FeatureSources) -> &HashMap<String, Arc<Value>>,
    ) -> Option<Arc<Value>> {
        table(self.sources.as_deref()?).get(name).cloned()
    }

    fn guarded<T>(&mut self, key: String, parse: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.resolving.contains(&key) {
            return Err(format!("{key} references itself"));
        }
        self.resolving.insert(key.clone());
        let result = parse(self);
        self.resolving.remove(&key);
        result
    }

    fn parse_placed(&mut self, value: &Value) -> Result<PlacedFeature> {
        let o = json::obj(value, &["feature", "placement"], "placed feature")?;
        let feature = self.feature(o.req("feature")?)?;
        let placement = o
            .list("placement")?
            .iter()
            .map(Placement::parse)
            .collect::<Result<Vec<_>>>()?;
        Ok(PlacedFeature {
            name: None,
            feature,
            placement,
        })
    }

    pub fn state_provider(&mut self, name: &str) -> Result<Arc<StateProvider>> {
        if let Some(provider) = self.providers.get(name) {
            return Ok(provider.clone());
        }
        let text = lookup(tuff_vanilla_data::worldgen::BLOCK_STATE_PROVIDERS, name)
            .ok_or_else(|| format!("unknown block state provider {name}"))?;
        let parsed: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let provider = StateProvider::parse(self, &parsed)?;
        self.providers.insert(name.to_string(), provider.clone());
        Ok(provider)
    }
}

/// The feature registry for features grown in live worlds, which have no
/// noise generator to hold one: the world's data pack features (from the
/// registries of the region in scope) over the built-in ones.
pub fn live_registry() -> Arc<Mutex<Registry>> {
    type Packed = Vec<(
        std::sync::Weak<tuff_item::registries::Registries>,
        Arc<Mutex<Registry>>,
    )>;
    static BUILTIN: std::sync::OnceLock<Arc<Mutex<Registry>>> = std::sync::OnceLock::new();
    static PACKED: std::sync::OnceLock<Mutex<Packed>> = std::sync::OnceLock::new();
    let builtin = || {
        BUILTIN
            .get_or_init(|| Arc::new(Mutex::new(Registry::builtin())))
            .clone()
    };
    let registries = tuff_item::registries::current();
    if !registries.modified() {
        return builtin();
    }
    let mut packed = PACKED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    packed.retain(|(owner, _)| owner.strong_count() > 0);
    if let Some((_, registry)) = packed
        .iter()
        .find(|(owner, _)| std::ptr::eq(owner.as_ptr(), Arc::as_ptr(&registries)))
    {
        return registry.clone();
    }
    let registry = match FeatureSources::from_registries(&registries) {
        Ok(sources) if !sources.is_empty() => {
            Arc::new(Mutex::new(Registry::with_sources(Some(Arc::new(sources)))))
        }
        _ => builtin(),
    };
    packed.push((Arc::downgrade(&registries), registry.clone()));
    registry
}

fn lookup(table: &'static [(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    table
        .binary_search_by(|(key, _)| key.cmp(&name))
        .ok()
        .map(|i| table[i].1)
}

/// One generation step's features, in the global order.
struct Step {
    features: Vec<Arc<PlacedFeature>>,
}

/// Every biome's features, sorted into the game's global order, ready to
/// decorate chunks.
pub struct Decorator {
    steps: Vec<Step>,
    /// Per biome and step, a bitset over the step's global indices.
    biome_steps: HashMap<u16, Vec<Box<[u64]>>>,
    /// Per biome, the names of its placed features (for the biome filter).
    biome_features: HashMap<u16, BTreeSet<String>>,
    pub registry: Mutex<Registry>,
}

impl Decorator {
    /// Builds the order from the biome source's possible biomes, in its
    /// order.
    pub fn new(biomes: &[u16], sources: Option<Arc<FeatureSources>>) -> Self {
        let lists = biomes
            .iter()
            .map(|&biome| {
                if let Some(steps) = sources.as_ref().and_then(|s| s.biomes.get(&biome)) {
                    return (biome, steps.clone());
                }
                let data =
                    tuff_vanilla_data::biome::Biome::get(biome_name(biome)).expect("known biome");
                let steps = data
                    .features
                    .iter()
                    .map(|step| step.iter().map(|n| json::qualify(n)).collect())
                    .collect();
                (biome, steps)
            })
            .collect();
        Self::from_lists(lists, Vec::new(), sources)
    }

    /// The decorator of a flat world's biome: the biome's features when
    /// `features` is set (minus the structure steps, and its own lakes when
    /// `lakes` replaces them), plus the lava lakes when `lakes` is set.
    pub fn flat(
        biome: u16,
        data: &'static tuff_vanilla_data::biome::Biome,
        features: bool,
        lakes: bool,
        top_layers: &[(i32, tuff_vanilla_data::BlockState)],
        sources: Option<Arc<FeatureSources>>,
    ) -> Self {
        const LAKES: usize = 1;
        const TOP_LAYER_MODIFICATION: usize = 10;
        const UNDERGROUND_STRUCTURES: usize = 3;
        const SURFACE_STRUCTURES: usize = 4;
        let mut steps: Vec<Vec<String>> = Vec::new();
        if features {
            let lists: Vec<Vec<String>> = match sources.as_ref().and_then(|s| s.biomes.get(&biome))
            {
                Some(steps) => steps.clone(),
                None => data
                    .features
                    .iter()
                    .map(|step| step.iter().map(|n| json::qualify(n)).collect())
                    .collect(),
            };
            steps = lists
                .into_iter()
                .enumerate()
                .map(|(i, step)| {
                    if i == UNDERGROUND_STRUCTURES
                        || i == SURFACE_STRUCTURES
                        || (lakes && i == LAKES)
                    {
                        Vec::new()
                    } else {
                        step
                    }
                })
                .collect();
        }
        if lakes {
            steps.resize(steps.len().max(LAKES + 1), Vec::new());
            steps[LAKES] = vec![
                "minecraft:lake_lava_underground".to_owned(),
                "minecraft:lake_lava_surface".to_owned(),
            ];
        }
        let mut inline = Vec::new();
        if !top_layers.is_empty() {
            steps.resize(steps.len().max(TOP_LAYER_MODIFICATION + 1), Vec::new());
            for &(height, state) in top_layers {
                let name = format!("tuff:flat_fill_layer_{height}");
                inline.push((
                    name.clone(),
                    Arc::new(PlacedFeature {
                        name: Some(name.clone()),
                        feature: Arc::new(kinds::FillLayer { height, state }),
                        placement: Vec::new(),
                    }),
                ));
                steps[TOP_LAYER_MODIFICATION].push(name);
            }
        }
        Self::from_lists(vec![(biome, steps)], inline, sources)
    }

    fn from_lists(
        biome_lists: Vec<(u16, Vec<Vec<String>>)>,
        inline: Vec<(String, Arc<PlacedFeature>)>,
        sources: Option<Arc<FeatureSources>>,
    ) -> Self {
        let biome_lists: Vec<(u16, Vec<Vec<String>>)> = biome_lists
            .into_iter()
            .map(|(biome, steps)| {
                let steps = steps
                    .iter()
                    .map(|names| {
                        names
                            .iter()
                            .flat_map(|name| crate::loaded::features::expand(name))
                            .collect()
                    })
                    .collect();
                (biome, steps)
            })
            .collect();
        let mut registry = Registry::with_sources(sources);
        registry.placed.extend(inline);
        let mut index_of: HashMap<String, usize> = HashMap::new();
        let mut placed: Vec<Arc<PlacedFeature>> = Vec::new();
        let mut edges: BTreeMap<(usize, usize), BTreeSet<(usize, usize)>> = BTreeMap::new();
        let mut max_step = 0;
        let mut biome_features = HashMap::new();
        for (biome, steps) in &biome_lists {
            let biome = *biome;
            max_step = max_step.max(steps.len());
            let mut list = Vec::new();
            for (step, names) in steps.iter().enumerate() {
                for name in names {
                    let next = index_of.len();
                    let index = *index_of.entry(name.clone()).or_insert_with(|| {
                        let feature = registry
                            .placed(&Value::String(name.clone()))
                            .expect("placed features resolve (validated at load)");
                        placed.push(feature);
                        next
                    });
                    list.push((step, index));
                }
            }
            for i in 0..list.len() {
                let node = (list[i].0, list[i].1);
                let entry = edges.entry(node).or_default();
                if i + 1 < list.len() {
                    entry.insert(list[i + 1]);
                }
            }
            biome_features.insert(
                biome,
                steps.iter().flatten().cloned().collect::<BTreeSet<_>>(),
            );
        }
        let mut discovered = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let mut sorted = Vec::new();
        fn dfs(
            node: (usize, usize),
            edges: &BTreeMap<(usize, usize), BTreeSet<(usize, usize)>>,
            discovered: &mut BTreeSet<(usize, usize)>,
            visiting: &mut BTreeSet<(usize, usize)>,
            out: &mut Vec<(usize, usize)>,
        ) {
            if discovered.contains(&node) || visiting.contains(&node) {
                return;
            }
            visiting.insert(node);
            if let Some(next) = edges.get(&node) {
                for n in next {
                    dfs(*n, edges, discovered, visiting, out);
                }
            }
            visiting.remove(&node);
            discovered.insert(node);
            out.push(node);
        }
        for node in edges.keys() {
            dfs(*node, &edges, &mut discovered, &mut visiting, &mut sorted);
        }
        sorted.reverse();
        let mut steps = Vec::new();
        let mut global: HashMap<(usize, usize), usize> = HashMap::new();
        for step in 0..max_step {
            let mut features = Vec::new();
            for &(s, index) in &sorted {
                if s == step {
                    global.insert((s, index), features.len());
                    features.push(placed[index].clone());
                }
            }
            steps.push(Step { features });
        }
        let mut biome_steps = HashMap::new();
        for (biome, lists) in biome_lists {
            let per_step = lists
                .iter()
                .enumerate()
                .map(|(step, names)| {
                    let count = steps.get(step).map_or(0, |s: &Step| s.features.len());
                    let mut bits = vec![0u64; count.div_ceil(64)];
                    for n in names {
                        let index = global[&(step, index_of[n])];
                        bits[index >> 6] |= 1 << (index & 63);
                    }
                    bits.into_boxed_slice()
                })
                .collect();
            biome_steps.insert(biome, per_step);
        }
        Self {
            steps,
            biome_steps,
            biome_features,
            registry: Mutex::new(registry),
        }
    }

    pub fn has_feature(&self, biome: u16, name: &str) -> bool {
        self.biome_features
            .get(&biome)
            .is_some_and(|set| set.contains(name))
    }

    /// Places the features of the chunk at the centre of `level`.
    /// Decorates the window's centre chunk: per step, the structures that
    /// reach it, then the features of the biomes around it.
    pub fn decorate(
        &self,
        level: &mut Region,
        seed: i64,
        biomes_present: &[u16],
        structures: Option<(&Structures, &[References])>,
    ) {
        let center = level.center;
        let origin = BlockPos::new(center.x << 4, level.min_y(), center.z << 4);
        let mut random = WorldgenRandom::new(Random::Xoroshiro(XoroshiroRandom::new(0)));
        let decoration_seed = random.set_decoration_seed(seed, origin.x, origin.z);
        let mut union: Vec<u64> = Vec::new();
        let step_count = self
            .steps
            .len()
            .max(tuff_vanilla_data::world::DECORATION_STEPS.len());
        for step_index in 0..step_count {
            if let Some((structures, references)) = structures {
                structures.place_step(step_index, level, references, &mut random, decoration_seed);
            }
            let Some(step) = self.steps.get(step_index) else {
                continue;
            };
            union.clear();
            union.resize(step.features.len().div_ceil(64), 0);
            for biome in biomes_present {
                if let Some(steps) = self.biome_steps.get(biome)
                    && let Some(bits) = steps.get(step_index)
                {
                    or_into(&mut union, bits);
                }
            }
            for index in ascending(&union) {
                let feature = &step.features[index];
                random.set_feature_seed(decoration_seed, index as i32, step_index as i32);
                let mut ctx = PlaceContext {
                    level,
                    random: &mut random,
                };
                feature.place(&mut ctx, origin, feature.name.as_deref());
            }
        }
    }
}

/// The biomes of a chunk's 3x3 neighbourhood that the source can produce,
/// as decoration iterates them.
pub fn biomes_in(level: &Region, possible: &[u16]) -> Vec<u16> {
    let chunks = (-1..=1).flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)));
    present_in(
        possible,
        chunks.filter_map(|(dx, dz)| {
            level
                .chunk(level.center.x + dx, level.center.z + dz)
                .map(|c| c.biomes())
        }),
    )
}

/// The `possible` biomes (in their order) that occur in any of `columns`.
fn present_in<'a>(possible: &[u16], columns: impl Iterator<Item = &'a [u16]>) -> Vec<u16> {
    let bound = possible.iter().max().map_or(0, |m| *m as usize + 1);
    let mut seen = vec![0u64; bound.div_ceil(64)];
    for biomes in columns {
        let mut last = None;
        for &b in biomes {
            if last != Some(b) {
                last = Some(b);
                if (b as usize) < bound {
                    seen[b as usize >> 6] |= 1 << (b & 63);
                }
            }
        }
    }
    possible
        .iter()
        .copied()
        .filter(|&b| seen[b as usize >> 6] >> (b & 63) & 1 != 0)
        .collect()
}

fn or_into(into: &mut [u64], bits: &[u64]) {
    for (word, other) in into.iter_mut().zip(bits) {
        *word |= *other;
    }
}

/// The set bits of `words` as ascending indices.
fn ascending(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(w, &word)| {
        let mut rest = word;
        std::iter::from_fn(move || {
            (rest != 0).then(|| {
                let bit = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                w * 64 + bit
            })
        })
    })
}

pub fn biome(name: &str) -> Option<u16> {
    biome_id(name)
}

#[cfg(test)]
mod bookkeeping_tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    fn reference_present(possible: &[u16], columns: &[Vec<u16>]) -> Vec<u16> {
        let mut seen = BTreeSet::new();
        for column in columns {
            seen.extend(column.iter().copied());
        }
        possible
            .iter()
            .copied()
            .filter(|b| seen.contains(b))
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn present_biomes_match_the_set_based_reference() {
        let mut rng = Lcg(7);
        for round in 0..400 {
            let span = [1u64, 3, 70, 200, 1000][round % 5];
            let possible: Vec<u16> = (0..(rng.next() % 90))
                .map(|_| (rng.next() % span) as u16)
                .collect();
            let columns: Vec<Vec<u16>> = (0..(rng.next() % 10))
                .map(|_| {
                    let runs = rng.next() % 40;
                    let mut column = Vec::new();
                    for _ in 0..runs {
                        let id = (rng.next() % (span + 40)) as u16;
                        column.extend(std::iter::repeat_n(id, (rng.next() % 9) as usize));
                    }
                    column
                })
                .collect();
            let fast = present_in(&possible, columns.iter().map(|c| c.as_slice()));
            assert_eq!(fast, reference_present(&possible, &columns));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn bitset_union_iterates_like_the_ordered_set() {
        let mut rng = Lcg(11);
        for _ in 0..400 {
            let count = (rng.next() % 300) as usize;
            let words = count.div_ceil(64);
            let mut union = vec![0u64; words];
            let mut expected = BTreeSet::new();
            for _ in 0..(rng.next() % 6) {
                let mut bits = vec![0u64; words];
                if count > 0 {
                    for _ in 0..(rng.next() % 40) {
                        let index = (rng.next() % count as u64) as usize;
                        bits[index >> 6] |= 1 << (index & 63);
                        expected.insert(index);
                    }
                }
                or_into(&mut union, &bits);
            }
            assert_eq!(
                ascending(&union).collect::<Vec<_>>(),
                expected.into_iter().collect::<Vec<_>>()
            );
        }
    }
}
