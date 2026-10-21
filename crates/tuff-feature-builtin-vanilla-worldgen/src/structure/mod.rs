//! Structures: where they start (structure sets and their placements),
//! what they are made of (pieces), which chunks they reach (references),
//! and placing them during decoration.
//!
//! Structure types are a registry keyed by resource id, like feature kinds;
//! a structure whose type is not registered never starts.

pub mod end_city;
pub mod fortress;
pub mod geometry;
pub mod jigsaw;
pub mod mansion;
pub mod mineshaft;
pub mod monument;
pub mod nether_fossil;
pub mod piece;
pub mod placement;
pub mod pool;
pub mod processor;
pub mod ruined_portal;
pub mod scattered;
pub mod stronghold;
pub mod template;
pub mod templated;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::{Structure, TerrainAdaptation};

use crate::density::Context;
use crate::feature::level::Region;
use crate::generator::NoiseGenerator;
use crate::java_set::long_set_order;
use crate::json::Result;
use crate::proto::HeightmapKind;
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{ChunkShape, biome_id};
use tuff_world_model::pos::ChunkPos;

use geometry::{BoundingBox, data_value_2d, encapsulating_boxes};

type FoundStarts = Vec<(usize, Vec<(i64, Arc<StructureStart>)>)>;

/// How far (in chunks) a structure start can reach the chunks that
/// reference it.
pub const REFERENCE_RADIUS: i32 = 8;

/// A structure piece: a part of a started structure with a box.
pub trait Piece: Send + Sync {
    fn kind(&self) -> PieceType;
    fn bounding_box(&self) -> BoundingBox;
    fn orientation(&self) -> Option<Facing> {
        None
    }
    fn gen_depth(&self) -> i32 {
        0
    }
    /// How terrain adaptation sees the piece: `Some(ground level delta)`
    /// when it is rigid.
    fn rigid_ground_delta(&self) -> Option<i32> {
        Some(0)
    }
    /// Where the piece joins its neighbours, for terrain adaptation.
    fn junctions(&self) -> &[crate::beardifier::Junction] {
        &[]
    }
    /// The fields beside id, box, orientation and depth.
    fn save_extra(&self, tag: &mut Compound);
    /// Takes back the progress a saved copy of this piece carries.
    fn load_progress(&self, _tag: &Compound) {}
    /// Places the part of the piece inside `chunk_box` (the chunk being
    /// decorated, which a piece may widen for later pieces).
    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        chunk: ChunkPos,
        reference: BlockPos,
    );

    /// Keeps the jigsaw blocks of a template piece instead of replacing
    /// them with their final state.
    fn keep_jigsaws(&mut self) {}

    /// Runs after every piece of the start had its chance to place in
    /// `chunk_box`, whether or not this piece reaches it.
    fn after_place(&self, _ctx: &mut PieceContext, _chunk_box: &BoundingBox, _chunk: ChunkPos) {}

    fn to_nbt(&self) -> Compound {
        let mut tag = Compound::new()
            .with("id", self.kind().name())
            .with("BB", self.bounding_box().to_nbt())
            .with("O", self.orientation().map_or(-1, data_value_2d))
            .with("GD", self.gen_depth());
        self.save_extra(&mut tag);
        tag
    }
}

pub struct PieceContext<'a, 'g> {
    pub level: &'a mut Region<'g>,
    pub random: &'a mut WorldgenRandom,
}

/// A structure that started in a chunk.
pub struct StructureStart {
    pub structure: &'static Structure,
    pub chunk: ChunkPos,
    pub references: i32,
    pub pieces: Vec<Box<dyn Piece>>,
    bounding_box: BoundingBox,
    kind: Option<Arc<dyn StructureKind>>,
}

impl std::fmt::Debug for StructureStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StructureStart")
            .field("structure", &self.structure.name)
            .field("chunk", &self.chunk)
            .field("pieces", &self.pieces.len())
            .field("bounding_box", &self.bounding_box)
            .finish()
    }
}

impl StructureStart {
    /// `None` for no pieces, which is no start at all.
    pub fn new(
        structure: &'static Structure,
        chunk: ChunkPos,
        pieces: Vec<Box<dyn Piece>>,
    ) -> Option<Self> {
        let boxes: Vec<BoundingBox> = pieces.iter().map(|p| p.bounding_box()).collect();
        let mut bounding_box = encapsulating_boxes(&boxes)?;
        if structure.terrain_adaptation != TerrainAdaptation::None {
            bounding_box = bounding_box.inflated(12);
        }
        Some(Self {
            structure,
            chunk,
            references: 0,
            pieces,
            bounding_box,
            kind: None,
        })
    }

    pub fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    pub fn place_in_chunk(
        &self,
        level: &mut Region,
        random: &mut WorldgenRandom,
        mut chunk_box: BoundingBox,
        chunk: ChunkPos,
    ) {
        let Some(first) = self.pieces.first() else {
            return;
        };
        let first_box = first.bounding_box();
        let center = first_box.center();
        let reference = BlockPos::new(center.x, first_box.min_y, center.z);
        let mut ctx = PieceContext { level, random };
        for piece in &self.pieces {
            if piece.bounding_box().intersects(&chunk_box) {
                piece.place(&mut ctx, &mut chunk_box, chunk, reference);
            }
        }
        for piece in &self.pieces {
            piece.after_place(&mut ctx, &chunk_box, chunk);
        }
        if let Some(kind) = &self.kind {
            kind.after_place(self, &mut ctx, &chunk_box);
        }
    }

    /// Takes back the piece progress of a saved start of the same structure.
    pub fn load_progress(&self, saved: &Compound) {
        let Some(children) = saved.get_list("Children") else {
            return;
        };
        if children.len() != self.pieces.len() {
            return;
        }
        for (piece, tag) in self.pieces.iter().zip(children) {
            if let Some(tag) = tag.as_compound()
                && tag.get_str("id") == Some(piece.kind().name())
            {
                piece.load_progress(tag);
            }
        }
    }

    pub fn to_nbt(&self) -> Compound {
        Compound::new()
            .with("id", self.structure.name)
            .with("ChunkX", self.chunk.x)
            .with("ChunkZ", self.chunk.z)
            .with("references", self.references)
            .with(
                "Children",
                Tag::List(
                    self.pieces
                        .iter()
                        .map(|p| Tag::Compound(p.to_nbt()))
                        .collect(),
                ),
            )
    }
}

/// The structures of one structure type that reference a chunk, in the
/// order the game visits them.
#[derive(Clone, Debug)]
pub struct References {
    /// Index of the structure in [`tuff_vanilla_data::structure::STRUCTURES`].
    pub structure: usize,
    pub starts: Vec<Arc<StructureStart>>,
}

/// Everything a structure type sees while choosing where it starts.
pub struct GenerationContext<'g> {
    pub generator: &'g NoiseGenerator,
    pub random: WorldgenRandom,
    pub seed: i64,
    pub chunk: ChunkPos,
    pub shape: ChunkShape,
    pub entry: &'g Entry,
    last_biome: Option<usize>,
}

impl GenerationContext<'_> {
    pub fn sea_level(&self) -> i32 {
        self.generator.settings.sea_level
    }

    /// The biome at a quart position, sampled on its own.
    pub fn biome(&mut self, qx: i32, qy: i32, qz: i32) -> u16 {
        let mut ctx = Context::uncached();
        self.generator.biome_source.biome_at(
            &self.generator.state.router,
            &mut ctx,
            &mut self.last_biome,
            [qx, qy, qz],
        )
    }

    /// Whether `accept` holds for every noise biome within `radius` blocks
    /// of a position (the cube of quart positions around it). The slices
    /// are sampled in parallel: a structure start sits on the pipeline's
    /// critical path.
    pub fn all_biomes_within(
        &self,
        (x, y, z): (i32, i32, i32),
        radius: i32,
        accept: impl Fn(u16) -> bool + Sync,
    ) -> bool {
        use rayon::prelude::*;
        let generator = self.generator;
        let tags = tuff_datapack::tags::current();
        ((z - radius) >> 2..=(z + radius) >> 2)
            .into_par_iter()
            .all(|qz| {
                tuff_datapack::tags::sync_scope(tags.clone(), || {
                    let mut ctx = Context::uncached();
                    let mut last = None;
                    ((x - radius) >> 2..=(x + radius) >> 2).all(|qx| {
                        ((y - radius) >> 2..=(y + radius) >> 2).all(|qy| {
                            accept(generator.biome_source.biome_at(
                                &generator.state.router,
                                &mut ctx,
                                &mut last,
                                [qx, qy, qz],
                            ))
                        })
                    })
                })
            })
    }

    pub fn is_valid_biome_at(&mut self, pos: BlockPos) -> bool {
        let biome = self.biome(pos.x >> 2, pos.y >> 2, pos.z >> 2);
        self.entry.allows(biome)
    }

    pub fn base_height(&self, x: i32, z: i32, kind: HeightmapKind) -> i32 {
        self.generator.base_height(x, z, kind, self.shape)
    }

    pub fn first_occupied_height(&self, x: i32, z: i32, kind: HeightmapKind) -> i32 {
        self.base_height(x, z, kind) - 1
    }

    /// Whether the structure's biomes hold anywhere in the column at
    /// `(x, z)`, from `min_y` to `max_y`.
    pub fn could_exist_in_column(&mut self, x: i32, z: i32, min_y: i32, max_y: i32) -> bool {
        (min_y >> 2..=max_y >> 2).any(|qy| {
            let biome = self.biome(x >> 2, qy, z >> 2);
            self.entry.allows(biome)
        })
    }

    /// [`Self::could_exist_in_column`] over the whole terrain column.
    pub fn could_exist_in_terrain_column(&mut self, x: i32, z: i32) -> bool {
        let (min_y, max_y) = (self.shape.min_y, self.shape.min_y + self.shape.height - 1);
        self.could_exist_in_column(x, z, min_y - 1, max_y)
    }

    /// The top of the chunk's middle column by `kind`, if the structure's
    /// biomes could be there.
    pub fn on_top_of_chunk_center(&mut self, kind: HeightmapKind) -> Option<BlockPos> {
        let (x, z) = ((self.chunk.x << 4) + 8, (self.chunk.z << 4) + 8);
        if !self.could_exist_in_terrain_column(x, z) {
            return None;
        }
        Some(BlockPos::new(x, self.first_occupied_height(x, z, kind), z))
    }

    /// The first occupied heights (world surface) at the corners of a box
    /// from `(x, z)` spanning `size_x` and `size_z`.
    pub fn corner_heights(&self, x: i32, z: i32, size_x: i32, size_z: i32) -> [i32; 4] {
        let kind = HeightmapKind::WorldSurfaceWg;
        [
            self.first_occupied_height(x, z, kind),
            self.first_occupied_height(x, z + size_z, kind),
            self.first_occupied_height(x + size_x, z, kind),
            self.first_occupied_height(x + size_x, z + size_z, kind),
        ]
    }

    pub fn lowest_y(&self, x: i32, z: i32, size_x: i32, size_z: i32) -> i32 {
        let h = self.corner_heights(x, z, size_x, size_z);
        h[0].min(h[1]).min(h[2].min(h[3]))
    }

    pub fn mean_first_occupied_height(&self, x: i32, z: i32, size_x: i32, size_z: i32) -> i32 {
        self.corner_heights(x, z, size_x, size_z)
            .iter()
            .sum::<i32>()
            / 4
    }

    pub fn base_column(&self, x: i32, z: i32) -> (i32, Vec<tuff_vanilla_data::BlockState>) {
        self.generator.base_column(x, z, self.shape)
    }
}

pub type PieceBuilder = Box<dyn FnOnce(&mut GenerationContext) -> Vec<Box<dyn Piece>>>;

/// Where a structure would start, and how to build it there.
pub struct GenerationStub {
    pub position: BlockPos,
    pub pieces: PieceBuilder,
}

/// A structure type (`minecraft:ruined_portal`, ...).
pub trait StructureKind: Send + Sync {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub>;

    /// Runs after the start's pieces placed in `chunk_box`.
    fn after_place(
        &self,
        _start: &StructureStart,
        _ctx: &mut PieceContext,
        _chunk_box: &BoundingBox,
    ) {
    }
}

pub type KindFactory = fn(&'static Structure, &Value) -> Result<Arc<dyn StructureKind>>;

/// Structure types by resource id.
pub fn kinds() -> &'static HashMap<&'static str, KindFactory> {
    static KINDS: OnceLock<HashMap<&'static str, KindFactory>> = OnceLock::new();
    KINDS.get_or_init(|| {
        let mut map: HashMap<&'static str, KindFactory> = HashMap::new();
        map.insert("minecraft:ruined_portal", ruined_portal::factory);
        map.insert("minecraft:nether_fossil", nether_fossil::factory);
        map.insert("minecraft:mineshaft", mineshaft::factory);
        map.insert("minecraft:jigsaw", jigsaw::factory);
        map.insert("minecraft:fortress", fortress::factory);
        map.insert("minecraft:stronghold", stronghold::factory);
        map.insert("minecraft:desert_pyramid", scattered::desert_pyramid);
        map.insert("minecraft:jungle_temple", scattered::jungle_temple);
        map.insert("minecraft:swamp_hut", scattered::swamp_hut);
        map.insert("minecraft:shipwreck", templated::shipwreck);
        map.insert("minecraft:igloo", templated::igloo);
        map.insert("minecraft:ocean_ruin", templated::ocean_ruin);
        map.insert("minecraft:buried_treasure", templated::buried_treasure);
        map.insert("minecraft:end_city", end_city::factory);
        map.insert("minecraft:woodland_mansion", mansion::factory);
        map.insert("minecraft:ocean_monument", monument::factory);
        map
    })
}

/// One structure as a generator uses it.
pub struct Entry {
    pub data: &'static Structure,
    pub kind: Option<Arc<dyn StructureKind>>,
    /// Indexed by biome id, as built in.
    pub biomes: Vec<bool>,
    /// The tag the biomes come from, read from the tags in effect.
    pub biome_tag: Option<tuff_vanilla_data::tag::Key<&'static str>>,
    /// A tag only data packs define, read by name.
    pub pack_tag: Option<&'static str>,
    /// The world's pack-added biomes, for their names.
    pub pack_biomes: Option<Arc<crate::loaded::biomes::PackBiomes>>,
}

impl Entry {
    /// The same structure accepting every biome.
    pub fn unrestricted(&self) -> Entry {
        Entry {
            data: self.data,
            kind: self.kind.clone(),
            biomes: vec![true; 4096],
            biome_tag: None,
            pack_tag: None,
            pack_biomes: None,
        }
    }

    /// Whether the structure may start in `biome`.
    pub fn allows(&self, biome: u16) -> bool {
        let added = self.pack_biomes.as_ref().and_then(|pack| pack.added(biome));
        match (self.biome_tag, self.pack_tag, added) {
            (Some(key), _, None) => crate::tags::biome_is(key, biome),
            (Some(_), _, Some(added)) => self
                .data
                .biome_tag
                .is_some_and(|tag| biome_in_tag(tag, &added.name)),
            (None, Some(tag), _) => {
                let name = self.pack_biomes.as_ref().map_or_else(
                    || tuff_world_model::chunk::biome_name(biome),
                    |p| p.name(biome),
                );
                biome_in_tag(tag, name)
            }
            (None, None, Some(added)) => self.data.biomes.contains(&added.name.as_str()),
            (None, None, None) => self.biomes.get(biome as usize).copied().unwrap_or(false),
        }
    }
}

fn biome_in_tag(tag: &str, biome: &str) -> bool {
    tuff_datapack::tags::with(|tags| tags.contains("minecraft:worldgen/biome", tag, biome))
}

/// The structures of one generator: every structure (the registry, which
/// decoration counts through), and the structure sets its biome source can
/// produce.
pub struct Structures {
    pub entries: Vec<Entry>,
    pub sets: Vec<placement::Set>,
    /// Structure indices per decoration step, in registry order.
    pub by_step: Vec<Vec<usize>>,
    /// Structure types the data names that are not implemented.
    pub missing: Vec<&'static str>,
    pub errors: Vec<String>,
    possible: Vec<bool>,
    seed: i64,
    sources: Option<Arc<crate::loaded::structures::StructureSources>>,
}

impl Structures {
    /// Keeps only the structure sets named in `names`.
    pub fn restricted_to(mut self, names: &[String]) -> Self {
        self.sets
            .retain(|set| names.iter().any(|n| n == set.data.name));
        self
    }

    pub fn reachable_sets(&self) -> impl Iterator<Item = &placement::Set> {
        self.sets
            .iter()
            .filter(|set| set.is_reachable(&self.entries, &self.possible))
    }

    pub fn new(possible_biomes: &[u16], seed: i64) -> Self {
        Self::with_sources(possible_biomes, seed, None, None)
    }

    /// The built-in structures with the pack ones of `sources` replacing
    /// or joining them.
    pub fn with_sources(
        possible_biomes: &[u16],
        seed: i64,
        sources: Option<Arc<crate::loaded::structures::StructureSources>>,
        pack_biomes: Option<Arc<crate::loaded::biomes::PackBiomes>>,
    ) -> Self {
        crate::loaded::structures::scope(sources.as_ref(), || {
            Self::build(possible_biomes, seed, sources.clone(), pack_biomes)
        })
    }

    fn build(
        possible_biomes: &[u16],
        seed: i64,
        sources: Option<Arc<crate::loaded::structures::StructureSources>>,
        pack_biomes: Option<Arc<crate::loaded::biomes::PackBiomes>>,
    ) -> Self {
        let mut all: Vec<&'static tuff_vanilla_data::structure::Structure> =
            tuff_vanilla_data::structure::STRUCTURES
                .iter()
                .map(|(name, builtin)| {
                    sources
                        .as_ref()
                        .and_then(|s| s.structure(name))
                        .unwrap_or(builtin)
                })
                .collect();
        if let Some(sources) = &sources {
            for structure in &sources.structures {
                if tuff_vanilla_data::structure::Structure::get(structure.name).is_none() {
                    all.push(structure);
                }
            }
        }
        let biome_count = all
            .iter()
            .flat_map(|s| s.biomes.iter().filter_map(|b| biome_id(b)))
            .chain(possible_biomes.iter().copied())
            .map(|b| b as usize + 1)
            .max()
            .unwrap_or(0);
        let mut missing = Vec::new();
        let mut errors = Vec::new();
        let mut entries = Vec::new();
        let mut by_step = vec![Vec::new(); tuff_vanilla_data::world::DECORATION_STEPS.len()];
        for (index, data) in all.into_iter().enumerate() {
            let mut biomes = vec![false; biome_count];
            for name in data.biomes {
                if let Some(id) = biome_id(name) {
                    biomes[id as usize] = true;
                }
            }
            let kind = match kinds().get(data.kind) {
                Some(factory) => {
                    let config: Value =
                        serde_json::from_str(data.config).expect("structure config is JSON");
                    match factory(data, &config) {
                        Ok(kind) => Some(kind),
                        Err(e) => {
                            errors.push(format!("{}: {e}", data.name));
                            None
                        }
                    }
                }
                None => {
                    if !missing.contains(&data.kind) {
                        missing.push(data.kind);
                    }
                    None
                }
            };
            by_step[data.step].push(index);
            let biome_tag = data.biome_tag.and_then(|name| {
                tuff_datapack::tags::key(
                    "minecraft:worldgen/biome",
                    tuff_vanilla_data::tag::worldgen::biome::ALL,
                    name,
                )
            });
            let pack_tag = data.biome_tag.filter(|_| biome_tag.is_none());
            entries.push(Entry {
                data,
                kind,
                biomes,
                biome_tag,
                pack_tag,
                pack_biomes: pack_biomes.clone(),
            });
        }
        let possible: Vec<bool> = {
            let count = possible_biomes
                .iter()
                .map(|b| *b as usize + 1)
                .max()
                .unwrap_or(0)
                .max(biome_count);
            let mut v = vec![false; count];
            for b in possible_biomes {
                v[*b as usize] = true;
            }
            v
        };
        let mut set_data: Vec<&'static tuff_vanilla_data::structure::StructureSet> =
            tuff_vanilla_data::structure::STRUCTURE_SETS
                .iter()
                .map(|(name, builtin)| {
                    sources
                        .as_ref()
                        .and_then(|s| s.set(name))
                        .unwrap_or(builtin)
                })
                .collect();
        if let Some(sources) = &sources {
            for set in &sources.sets {
                if tuff_vanilla_data::structure::StructureSet::get(set.name).is_none() {
                    set_data.push(set);
                }
            }
        }
        let sets = set_data
            .into_iter()
            .map(|set| placement::Set::new(set, &entries))
            .collect();
        Self {
            entries,
            sets,
            by_step,
            possible,
            missing,
            errors,
            seed,
            sources,
        }
    }

    /// Runs `f` with this generator's pack template pools and processor
    /// lists in effect.
    pub fn scoped<R>(&self, f: impl FnOnce() -> R) -> R {
        crate::loaded::structures::scope(self.sources.as_ref(), f)
    }

    /// The chunks the concentric rings structure set `set` starts in (the
    /// strongholds), computed once per generator.
    pub fn ring_starts(&self, generator: &NoiseGenerator, set: &str) -> Option<&[ChunkPos]> {
        let set = self.reachable_sets().find(|s| s.data.name == set)?;
        matches!(
            set.data.placement,
            tuff_vanilla_data::structure::StructurePlacement::ConcentricRings { .. }
        )
        .then(|| set.ring_positions(generator, self.seed))
    }

    /// The locate position of the ring start of `set` nearest to `from`
    /// (what eyes of ender fly toward), measured from each chunk's middle
    /// at y 32; ring chunks whose biome refuses the structure are skipped.
    pub fn nearest_ring_start(
        &self,
        generator: &NoiseGenerator,
        set: &str,
        from: BlockPos,
    ) -> Option<BlockPos> {
        let data = self.reachable_sets().find(|s| s.data.name == set)?;
        let positions = self.ring_starts(generator, set)?;
        let offset = data.data.placement.spread().locate_offset;
        let mut best: Option<(f64, ChunkPos)> = None;
        for pos in positions {
            let (dx, dy, dz) = (
                ((pos.x << 4) + 8 - from.x) as f64,
                (32 - from.y) as f64,
                ((pos.z << 4) + 8 - from.z) as f64,
            );
            let distance = dx * dx + dy * dy + dz * dz;
            if best.is_some_and(|(d, _)| distance >= d) {
                continue;
            }
            if data
                .structures
                .iter()
                .any(|(index, _)| self.starts_in_biome(generator, *index, *pos))
            {
                best = Some((distance, *pos));
            }
        }
        best.map(|(_, pos)| {
            BlockPos::new(
                (pos.x << 4) + offset[0],
                offset[1],
                (pos.z << 4) + offset[2],
            )
        })
    }

    /// The nearest locate position among every concentric rings set that
    /// places one of `structures` (what eyes of ender fly toward).
    pub fn nearest_ring_structure(
        &self,
        generator: &NoiseGenerator,
        structures: &[&str],
        from: BlockPos,
    ) -> Option<BlockPos> {
        let mut best: Option<(i64, BlockPos)> = None;
        for set in self.reachable_sets() {
            let wanted = set
                .structures
                .iter()
                .any(|(index, _)| structures.contains(&self.entries[*index].data.name));
            if !wanted {
                continue;
            }
            let Some(found) = self.nearest_ring_start(generator, set.data.name, from) else {
                continue;
            };
            let (dx, dy, dz) = (
                i64::from(found.x - from.x),
                i64::from(found.y - from.y),
                i64::from(found.z - from.z),
            );
            let distance = dx * dx + dy * dy + dz * dz;
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, found));
            }
        }
        best.map(|(_, pos)| pos)
    }

    fn starts_in_biome(&self, generator: &NoiseGenerator, index: usize, pos: ChunkPos) -> bool {
        let entry = &self.entries[index];
        if entry.kind.is_none() {
            return false;
        }
        let mut ctx = Context::uncached();
        let biome = generator.biome_source.biome_at(
            &generator.state.router,
            &mut ctx,
            &mut None,
            [pos.x << 2, 0, pos.z << 2],
        );
        entry.allows(biome)
    }

    pub fn seed(&self) -> i64 {
        self.seed
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.data.name == name)
    }

    /// The structures that start in chunk `pos`.
    pub fn starts(
        &self,
        generator: &NoiseGenerator,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Vec<Arc<StructureStart>> {
        self.scoped(|| {
            self.reachable_sets()
                .filter_map(|set| self.set_start(generator, set, pos, shape))
                .map(Arc::new)
                .collect()
        })
    }

    /// The structure of `set` that starts in chunk `pos`, if any.
    pub fn set_start(
        &self,
        generator: &NoiseGenerator,
        set: &placement::Set,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Option<StructureStart> {
        if !set.is_structure_chunk(self, generator, pos) {
            return None;
        }
        if let [(index, _)] = set.structures.as_slice() {
            return self.try_generate(generator, *index, pos, shape);
        }
        let mut options = set.structures.clone();
        let mut random = WorldgenRandom::legacy(0);
        random.set_large_feature_seed(self.seed, pos.x, pos.z);
        let mut total: i32 = options.iter().map(|(_, w)| *w).sum();
        while !options.is_empty() {
            let mut choice = random.next_int_bounded(total);
            let mut picked = 0;
            for (i, (_, weight)) in options.iter().enumerate() {
                choice -= weight;
                picked = i;
                if choice < 0 {
                    break;
                }
            }
            let (index, weight) = options[picked];
            if let Some(start) = self.try_generate(generator, index, pos, shape) {
                return Some(start);
            }
            options.remove(picked);
            total -= weight;
        }
        None
    }

    /// Whether structure `index` finds a place to start in chunk `pos`
    /// whose biome it accepts, without building its pieces.
    pub fn can_start(
        &self,
        generator: &NoiseGenerator,
        index: usize,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> bool {
        let entry = &self.entries[index];
        let Some(kind) = entry.kind.as_ref() else {
            return false;
        };
        let mut ctx = self.generation_context(generator, entry, pos, shape);
        self.scoped(|| {
            kind.find_generation_point(&mut ctx)
                .is_some_and(|stub| ctx.is_valid_biome_at(stub.position))
        })
    }

    fn generation_context<'g>(
        &self,
        generator: &'g NoiseGenerator,
        entry: &'g Entry,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> GenerationContext<'g> {
        let mut random = WorldgenRandom::legacy(0);
        random.set_large_feature_seed(self.seed, pos.x, pos.z);
        GenerationContext {
            generator,
            random,
            seed: self.seed,
            chunk: pos,
            shape,
            entry,
            last_biome: None,
        }
    }

    fn try_generate(
        &self,
        generator: &NoiseGenerator,
        index: usize,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Option<StructureStart> {
        let entry = &self.entries[index];
        let kind = entry.kind.as_ref()?;
        let mut ctx = self.generation_context(generator, entry, pos, shape);
        let stub = self.scoped(|| kind.find_generation_point(&mut ctx))?;
        if !ctx.is_valid_biome_at(stub.position) {
            return None;
        }
        let pieces = self.scoped(|| (stub.pieces)(&mut ctx));
        let mut start = StructureStart::new(entry.data, pos, pieces)?;
        start.kind = Some(kind.clone());
        Some(start)
    }

    /// The start of structure `index` in chunk `pos` whatever its biomes,
    /// without the placement rules of its sets.
    pub fn forced_start(
        &self,
        generator: &NoiseGenerator,
        index: usize,
        pos: ChunkPos,
        shape: ChunkShape,
    ) -> Option<StructureStart> {
        let entry = self.entries[index].unrestricted();
        let kind = entry.kind.as_ref()?;
        let mut ctx = self.generation_context(generator, &entry, pos, shape);
        let stub = self.scoped(|| kind.find_generation_point(&mut ctx))?;
        let pieces = self.scoped(|| (stub.pieces)(&mut ctx));
        let mut start = StructureStart::new(entry.data, pos, pieces)?;
        start.kind = Some(kind.clone());
        Some(start)
    }

    /// The pieces of a jigsaw assembly anchored at `at`, in a context
    /// that accepts every biome.
    pub fn assemble_jigsaw(
        &self,
        generator: &NoiseGenerator,
        (pool, target, max_depth): (&str, &str, i32),
        at: BlockPos,
        shape: ChunkShape,
        keep_jigsaws: bool,
    ) -> Option<Vec<Box<dyn Piece>>> {
        let (_, data) = tuff_vanilla_data::structure::STRUCTURES.first()?;
        let entry = Entry {
            data,
            kind: None,
            biomes: vec![true; 4096],
            biome_tag: None,
            pack_tag: None,
            pack_biomes: None,
        };
        let mut ctx = self.generation_context(generator, &entry, ChunkPos::of_block(at), shape);
        self.scoped(|| jigsaw::assemble(&mut ctx, pool, target, max_depth, at, keep_jigsaws))
    }

    /// The starts within [`REFERENCE_RADIUS`] of `pos` whose box reaches
    /// its column, grouped per structure in the game's order. `starts`
    /// gives the starts of a chunk.
    pub fn references<'s>(
        &self,
        pos: ChunkPos,
        starts: impl Fn(ChunkPos) -> &'s [Arc<StructureStart>],
    ) -> Vec<References> {
        let (min_x, min_z) = (pos.x << 4, pos.z << 4);
        let mut found: FoundStarts = Vec::new();
        for sx in pos.x - REFERENCE_RADIUS..=pos.x + REFERENCE_RADIUS {
            for sz in pos.z - REFERENCE_RADIUS..=pos.z + REFERENCE_RADIUS {
                for start in starts(ChunkPos::new(sx, sz)) {
                    if !start
                        .bounding_box
                        .intersects_xz(min_x, min_z, min_x + 15, min_z + 15)
                    {
                        continue;
                    }
                    let Some(index) = self.index_of(start.structure.name) else {
                        continue;
                    };
                    let key = chunk_key(ChunkPos::new(sx, sz));
                    match found.iter_mut().find(|(i, _)| *i == index) {
                        Some((_, list)) => list.push((key, start.clone())),
                        None => found.push((index, vec![(key, start.clone())])),
                    }
                }
            }
        }
        found.sort_by_key(|(index, _)| *index);
        found
            .into_iter()
            .map(|(structure, list)| {
                let keys: Vec<i64> = list.iter().map(|(k, _)| *k).collect();
                let starts = long_set_order(&keys)
                    .into_iter()
                    .filter_map(|k| list.iter().find(|(key, _)| *key == k))
                    .map(|(_, s)| s.clone())
                    .collect();
                References { structure, starts }
            })
            .collect()
    }

    /// Places the structures of decoration step `step` that reference the
    /// window's centre chunk, as decoration does before that step's
    /// features.
    pub fn place_step(
        &self,
        step: usize,
        level: &mut Region,
        references: &[References],
        random: &mut WorldgenRandom,
        decoration_seed: i64,
    ) {
        let Some(structures) = self.by_step.get(step) else {
            return;
        };
        let center = level.center;
        let writable = BoundingBox::chunk(center.x, center.z, level.min_y() + 1, level.max_y());
        for (index, structure) in structures.iter().enumerate() {
            random.set_feature_seed(decoration_seed, index as i32, step as i32);
            let Some(refs) = references.iter().find(|r| r.structure == *structure) else {
                continue;
            };
            for start in &refs.starts {
                self.scoped(|| start.place_in_chunk(level, random, writable, center));
            }
        }
    }
}

/// A mob a structure places on the bottom centre of `pos`, as saved
/// entity data. A `fresh` one is kept for good and set up as a new
/// structure spawn when its chunk goes live.
pub fn structure_mob(id: &str, pos: BlockPos, fresh: bool) -> Compound {
    let at = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
    let mob = Compound::new()
        .with("id", id)
        .with("Pos", Tag::List(at.map(Tag::Double).to_vec()))
        .with("Motion", Tag::List(vec![Tag::Double(0.0); 3]))
        .with(
            "Rotation",
            Tag::List(vec![Tag::Float(0.0), Tag::Float(0.0)]),
        );
    if fresh {
        mob.with("PersistenceRequired", true)
            .with(template::FINALIZE_SPAWN, true)
    } else {
        mob
    }
}

/// A chunk position packed the way the game keys chunks.
pub fn chunk_key(pos: ChunkPos) -> i64 {
    (pos.x as u32 as i64) | ((pos.z as i64) << 32)
}

/// A chunk's structure data as the game saves it: its own starts and the
/// chunks holding the starts that reach it.
pub fn structures_nbt(starts: &[Arc<StructureStart>], references: &[References]) -> Compound {
    let mut starts_tag = Compound::new();
    for start in starts {
        starts_tag.insert(start.structure.name, start.to_nbt());
    }
    let mut refs_tag = Compound::new();
    for refs in references {
        let Some(name) = refs.starts.first().map(|s| s.structure.name) else {
            continue;
        };
        let keys: Vec<i64> = refs.starts.iter().map(|s| chunk_key(s.chunk)).collect();
        refs_tag.insert(name, Tag::LongArray(keys));
    }
    Compound::new()
        .with("starts", starts_tag)
        .with("References", refs_tag)
}

#[cfg(test)]
mod pack_tag_tests {
    use super::*;

    fn monument_reachable(structures: &Structures) -> bool {
        structures
            .reachable_sets()
            .any(|set| set.data.name == "minecraft:ocean_monuments")
    }

    #[tuff_core::watchdog]
    #[test]
    fn pack_biome_tag_makes_a_structure_set_reachable() {
        let plains = biome_id("minecraft:plains").expect("plains");
        let pack = tuff_datapack::tags::vanilla_with(&[(
            "minecraft:worldgen/biome",
            "minecraft:has_structure/ocean_monument",
            "minecraft:plains",
        )]);
        let before = Structures::new(&[plains], 0);
        assert!(!tuff_datapack::tags::sync_scope(
            tuff_datapack::tags::vanilla_with(&[]),
            || monument_reachable(&before)
        ));
        let after = Structures::new(&[plains], 0);
        assert!(tuff_datapack::tags::sync_scope(
            pack,
            || monument_reachable(&after)
        ));
    }
}
