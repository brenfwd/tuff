//! Chunk generators, one per dimension. Every generator is pure and
//! deterministic, so any thread can generate any chunk. Features supply
//! them through `Registries::generators`; a dimension none of them knows
//! is a void.

use std::sync::Arc;

use tokio::sync::{oneshot, watch};

use crate::storage::codec::ChunkCodec;
use crate::storage::level::DimensionSettings;
use crate::world::chunk_io::{Blob, ChunkIo, EntityWrite, IoOp, IoResult};
use crate::world::dimension::Dimensions;
use crate::world::messages::RegionHandle;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// Whether a locate search passes over structure starts in a chunk.
pub type SkipChunk<'a> = &'a (dyn Fn(ChunkPos) -> bool + Sync);

/// The data-driven registries a generator reads, and its options.
pub struct GeneratorData<'a> {
    pub registries: &'a tuff_item::registries::Registries,
    /// Whether density functions decoded from packs run as op-list
    /// programs rather than tree-walked.
    pub compile_loaded: bool,
    /// The bounds of the dimension the generator fills.
    pub shape: ChunkShape,
    /// Generation stages switched off for stage-by-stage parity diffs.
    pub disabled: DisabledStages,
}

/// Generation stages a debug config switches off, matching the game's
/// developer switches so a world can be diffed against vanilla one stage
/// at a time. All off (every stage runs) by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisabledStages {
    pub surface: bool,
    pub carvers: bool,
    pub structures: bool,
    pub features: bool,
    pub ore_veins: bool,
    pub blending: bool,
    pub fluid_generation: bool,
}

/// The config keys of [`DisabledStages`], in field order.
pub const DISABLE_STAGE_KEYS: [&str; 7] = [
    "tuff.worldgen.debug.disableSurface",
    "tuff.worldgen.debug.disableCarvers",
    "tuff.worldgen.debug.disableStructures",
    "tuff.worldgen.debug.disableFeatures",
    "tuff.worldgen.debug.disableOreVeins",
    "tuff.worldgen.debug.disableBlending",
    "tuff.worldgen.debug.disableFluidGeneration",
];

impl DisabledStages {
    pub fn from_config(settings: &crate::config::ConfigStore) -> Self {
        let [
            surface,
            carvers,
            structures,
            features,
            ore_veins,
            blending,
            fluid_generation,
        ] = DISABLE_STAGE_KEYS.map(|key| settings.get_bool(key).unwrap_or(false));
        Self {
            surface,
            carvers,
            structures,
            features,
            ore_veins,
            blending,
            fluid_generation,
        }
    }
}

/// The config key for [`GeneratorData::compile_loaded`].
pub const COMPILE_LOADED_KEY: &str = "tuff.worldgen.compileLoaded";

pub fn config_keys() -> Vec<crate::config::Key> {
    let stages = [
        "surface rules",
        "carvers",
        "structures",
        "features (decoration)",
        "ore veins",
        "blending with old chunks (no-op: tuff does not blend)",
        "fluid generation (aquifers and sea fill become air)",
    ];
    let mut keys = vec![crate::config::Key::new(
        COMPILE_LOADED_KEY,
        crate::config::Value::Bool(true),
        "Whether density functions and noise settings from data packs compile to op-list programs like the built-in ones; off tree-walks them, a slower fallback (applies on restart)",
    )];
    for (key, stage) in DISABLE_STAGE_KEYS.into_iter().zip(stages) {
        keys.push(crate::config::Key::new(
            key,
            crate::config::Value::Bool(false),
            &format!("Debug: skip worldgen {stage} in newly generated chunks, to diff against a vanilla world with the matching MC_DEBUG_DISABLE_* switch (applies on restart)"),
        ));
    }
    keys
}

/// Hands every dimension's generator the frozen registries.
pub fn bind_all(
    dimensions: &Dimensions,
    registries: &tuff_item::registries::Registries,
    settings: &crate::config::ConfigStore,
) {
    let compile_loaded = settings.get_bool(COMPILE_LOADED_KEY).unwrap_or(true);
    let disabled = DisabledStages::from_config(settings);
    for dimension in dimensions.iter() {
        dimension.generator.bind(&GeneratorData {
            registries,
            compile_loaded,
            shape: dimension.shape,
            disabled,
        });
    }
}

/// Produces the terrain of one dimension.
pub trait ChunkGenerator: Send + Sync {
    /// A whole chunk on its own, without anything its neighbours place.
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk;
    fn sea_level(&self) -> i32;
    /// Replaces the placeholder fill below y 0 of an upgraded chunk with
    /// this generator's terrain there; false when it cannot.
    fn retrogen_below_zero(&self, _pos: ChunkPos, _shape: ChunkShape, _chunk: &mut Chunk) -> bool {
        false
    }
    /// Whether this is a debug world, which clients render with its own sky.
    fn is_debug(&self) -> bool {
        false
    }
    /// Whether this is a flat world, which clients render with a low horizon.
    fn is_flat(&self) -> bool {
        false
    }
    /// Where players appear when the world has no spawn point yet.
    fn spawn(&self) -> BlockPos;
    /// Generators whose features cross chunk borders run their own
    /// pipeline, started once per world here; regions then ask it for new
    /// chunks instead of calling `generate`.
    fn pipeline(self: Arc<Self>, _cx: PipelineContext) -> Option<Arc<dyn ChunkPipeline>> {
        None
    }

    /// The nearest start of any of `structures` (resource ids) from
    /// `from`, as `/locate` finds it: where it is and which one it is.
    /// The search is long; run it off the actors.
    fn locate_structure(
        &self,
        _structures: &[&str],
        _from: BlockPos,
        _shape: ChunkShape,
    ) -> Option<(BlockPos, &'static str)> {
        None
    }

    /// Like [`Self::locate_structure`], searching `radius` placement
    /// cells out, as explorer maps do. Starts whose chunk `skip` accepts
    /// are passed over.
    fn locate_structure_within(
        &self,
        structures: &[&str],
        from: BlockPos,
        _radius: i32,
        shape: ChunkShape,
        _skip: Option<SkipChunk<'_>>,
    ) -> Option<(BlockPos, &'static str)> {
        self.locate_structure(structures, from, shape)
    }

    /// A start of `structure` in `chunk`, built regardless of the chunk's
    /// biomes and the structure's placement rules, as `/place structure`
    /// does. Long; run it off the actors.
    fn plan_structure(
        &self,
        _structure: &str,
        _chunk: ChunkPos,
        _shape: ChunkShape,
    ) -> Option<Arc<dyn crate::world::features::PlannedStructure>> {
        None
    }

    /// A jigsaw assembly from `pool` anchored at `pos`, as `/place jigsaw`
    /// builds one. Long; run it off the actors.
    fn plan_jigsaw(
        &self,
        _pool: &str,
        _target: &str,
        _max_depth: i32,
        _pos: BlockPos,
        _shape: ChunkShape,
        _keep_jigsaws: bool,
    ) -> Option<Arc<dyn crate::world::features::PlannedStructure>> {
        None
    }

    /// The biome the generator places at each of `positions`, `None`
    /// where it cannot tell.
    fn biomes_at(&self, positions: &[BlockPos], _shape: ChunkShape) -> Vec<Option<&'static str>> {
        vec![None; positions.len()]
    }

    /// The nearest sampled position holding any of `biomes` (resource ids)
    /// from `from`, and its biome. Also long.
    fn locate_biome(
        &self,
        _biomes: &[&str],
        _from: BlockPos,
        _shape: ChunkShape,
    ) -> Option<(BlockPos, &'static str)> {
        None
    }

    /// Whether freshly generated chunks start with their biome's creatures
    /// (the game's spawn step of world generation); the region spawns them
    /// once such a chunk is installed.
    fn spawns_creatures(&self) -> bool {
        false
    }

    /// The locate position of the nearest of `structures` that concentric
    /// rings place (strongholds), as an eye of ender finds it from `from`.
    /// Computing the rings the first time is slow, so callers run this off
    /// their actor.
    /// Called once at startup, after data packs load and before chunks
    /// generate, with the frozen data-driven registries.
    fn bind(&self, _data: &GeneratorData<'_>) {}
    /// A throwaway copy of this generator, bound like it, with `seed` and a
    /// single `biome` everywhere in place of its own where given. `None`
    /// when it cannot make one or `biome` is unknown.
    fn variant(&self, _seed: Option<i64>, _biome: Option<&str>) -> Option<Arc<dyn ChunkGenerator>> {
        None
    }
    fn nearest_ring_structure(&self, _structures: &[&str], _from: BlockPos) -> Option<BlockPos> {
        None
    }
}

/// Where regions ask a staged generator for finished chunks.
pub trait ChunkPipeline: Send + Sync {
    /// Asks for the finished chunk at `pos`, delivered to `reply` as
    /// `RegionInternal::Generated`. Lower `priority` is served first (see
    /// [`URGENT`]); the pipeline orders requests of equal priority itself.
    /// False if the pipeline has stopped.
    fn want(&self, pos: ChunkPos, reply: RegionHandle, priority: u32) -> bool;

    /// Withdraws `region`'s request for `pos`: work nobody else waits on
    /// may be dropped. A delivery already under way still arrives.
    fn unwant(&self, _pos: ChunkPos, _region: RegionPos) {}

    /// Saves every chunk still in progress, answering `done` once they
    /// are stored. The world asks once its regions have closed.
    fn flush(&self, done: oneshot::Sender<()>) {
        let _ = done.send(());
    }

    /// No region is loaded: drops what storage or generation can give
    /// back, saving chunks in progress first, and answers `done` once that
    /// is stored.
    fn idle(&self, done: oneshot::Sender<()>) {
        let _ = done.send(());
    }

    /// Forgets the finished and idle chunks in progress that `area`
    /// matches, whose storage was just cleared, so they are generated and
    /// decorated afresh when next wanted. Answers `done` once it has.
    fn forget(
        &self,
        _area: Arc<dyn Fn(ChunkPos) -> bool + Send + Sync>,
        done: oneshot::Sender<()>,
    ) {
        let _ = done.send(());
    }
}

/// The priority of a chunk something waits on outside any player's view
/// (a command, a forced chunk, a teleport): served before the rest.
pub const URGENT: u32 = 0;
/// The priority of a chunk players view.
pub const VIEWED: u32 = 1;
/// The priority of a chunk only pre-generation waits on: served after
/// every chunk players view.
pub const BACKGROUND: u32 = 2;

/// What a pipeline starts with.
pub struct PipelineContext {
    pub shape: ChunkShape,
    /// The game tick.
    pub clock: watch::Receiver<u64>,
    /// The dimension's chunk storage, for chunks in progress; `None` keeps
    /// them in memory only.
    pub storage: Option<ProtoStorage>,
    /// The data pack tags in effect, for generation jobs to run under.
    pub tags: tuff_datapack::tags::TagFeed,
}

/// A dimension's chunk storage as a pipeline uses it: whole chunk
/// compounds read and written beside the regions' own chunk data, through
/// the same storage actor.
#[derive(Clone)]
pub struct ProtoStorage {
    io: ChunkIo,
    dimension: Arc<str>,
    codec: Arc<dyn ChunkCodec>,
}

impl ProtoStorage {
    pub fn new(io: ChunkIo, dimension: &str, codec: Arc<dyn ChunkCodec>) -> Self {
        Self {
            io,
            dimension: dimension.into(),
            codec,
        }
    }

    pub fn codec(&self) -> &Arc<dyn ChunkCodec> {
        &self.codec
    }

    /// The stored chunk blobs at `positions`, in order; dropped if storage
    /// failed.
    pub fn read(&self, positions: &[ChunkPos]) -> oneshot::Receiver<Vec<IoResult>> {
        let ops = positions.iter().map(|pos| IoOp::Read(*pos)).collect();
        self.io.submit(&self.dimension, ops)
    }

    /// Stores chunk blobs, leaving the chunks' entity records alone.
    pub fn write(&self, blobs: Vec<(ChunkPos, Blob)>) -> oneshot::Receiver<Vec<IoResult>> {
        let ops = blobs
            .into_iter()
            .map(|(pos, blob)| IoOp::Write {
                pos,
                blocks: Some(blob),
                entities: EntityWrite::Keep,
                pois: EntityWrite::Keep,
            })
            .collect();
        self.io.submit(&self.dimension, ops)
    }
}

/// The generators features provide.
pub trait WorldGenerators: Send + Sync {
    /// The generator for a dimension's settings, if this knows it.
    fn generator(&self, settings: &DimensionSettings, seed: i64)
    -> Option<Arc<dyn ChunkGenerator>>;

    /// Where a new world's players first appear.
    fn new_world_spawn(&self, _seed: i64) -> BlockPos {
        BlockPos::new(0, 64, 0)
    }
}

/// No generators: every dimension is a void.
pub struct NoGenerators;

impl WorldGenerators for NoGenerators {
    fn generator(
        &self,
        _settings: &DimensionSettings,
        _seed: i64,
    ) -> Option<Arc<dyn ChunkGenerator>> {
        None
    }
}

/// Starts the pipeline of every dimension whose generator has one, keeping
/// chunks in progress in `storage` when given.
pub fn start_pipelines(
    dimensions: &Dimensions,
    clock: &watch::Receiver<u64>,
    storage: Option<(&ChunkIo, &Arc<dyn ChunkCodec>)>,
    tags: &tuff_datapack::tags::TagFeed,
) -> Vec<Option<Arc<dyn ChunkPipeline>>> {
    dimensions
        .iter()
        .map(|dimension| {
            dimension.generator.clone().pipeline(PipelineContext {
                shape: dimension.shape,
                clock: clock.clone(),
                storage: storage.map(|(io, codec)| {
                    ProtoStorage::new(io.clone(), &dimension.key, codec.clone())
                }),
                tags: tags.clone(),
            })
        })
        .collect()
}

/// A tick scheduled during generation: what ticks (a fluid or block id),
/// where, and how many ticks after the chunk goes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledTick {
    pub kind: &'static str,
    pub pos: (i32, i32, i32),
    pub delay: i32,
}

impl ScheduledTick {
    /// Schedules `tick` unless one of its kind already waits there.
    pub fn schedule(list: &mut Vec<ScheduledTick>, tick: ScheduledTick) {
        if !list
            .iter()
            .any(|t| t.pos == tick.pos && t.kind == tick.kind)
        {
            list.push(tick);
        }
    }

    /// The tick as chunk data stores it.
    pub fn to_nbt(self) -> Compound {
        Compound::new()
            .with("i", self.kind)
            .with("x", self.pos.0)
            .with("y", self.pos.1)
            .with("z", self.pos.2)
            .with("t", self.delay)
            .with("p", 0)
    }
}

/// A chunk ready for its region.
#[derive(Clone)]
pub struct Finished {
    pub chunk: Chunk,
    /// Block entity data features and structures left.
    pub block_entities: Vec<Compound>,
    /// The chunk's structure starts and references as saved.
    pub structures: Option<Compound>,
    /// Positions the live chunk post-processes: their first fluid tick
    /// and a shape update from their neighbours.
    pub post_processing: Vec<BlockPos>,
    /// Ticks generation scheduled for the live chunk.
    pub fluid_ticks: Vec<ScheduledTick>,
    pub block_ticks: Vec<ScheduledTick>,
    /// Entities structures placed, as saved entity data.
    pub entities: Vec<Compound>,
}

impl std::fmt::Debug for Finished {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Finished")
            .field("block_entities", &self.block_entities.len())
            .field("post_processing", &self.post_processing.len())
            .finish_non_exhaustive()
    }
}

impl Finished {
    /// A chunk with nothing beside its blocks.
    pub fn new(chunk: Chunk) -> Self {
        Self {
            chunk,
            block_entities: Vec::new(),
            structures: None,
            post_processing: Vec::new(),
            fluid_ticks: Vec::new(),
            block_ticks: Vec::new(),
            entities: Vec::new(),
        }
    }
}

/// Nothing at all, for dimensions whose generator this server does not know.
#[derive(Debug, Clone, Copy)]
pub struct Void;

impl ChunkGenerator for Void {
    fn generate(&self, _pos: ChunkPos, shape: ChunkShape) -> Chunk {
        Chunk::empty(shape)
    }

    fn sea_level(&self) -> i32 {
        0
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 64, 0)
    }
}
