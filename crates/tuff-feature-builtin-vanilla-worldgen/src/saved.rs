//! Chunks in generation as the game saves them: the region chunk compound
//! with the status the chunk reached, so a restart (of this server or of
//! vanilla) resumes them instead of generating them again.

use std::collections::HashMap;

use tuff_core::storage::chunk_nbt;
use tuff_core::world::generator::Finished;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::Registry;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{BIOME_VOLUME, ChunkShape, ceil_log2, pack, unpack};
use tuff_world_model::chunk_meta::ChunkMeta;
use tuff_world_model::pos::ChunkPos;

use crate::proto::{HeightmapKind, ProtoChunk, ScheduledTick};

/// Terrain done: structures, biomes, noise, surface and carvers.
pub const STATUS_TERRAIN: &str = "minecraft:terrain";
/// The chunk's own features are placed.
pub const STATUS_FEATURES: &str = "minecraft:features";

/// The game's statuses in order, with the older steps the terrain status
/// replaced where they fall.
const STATUSES: [&str; 13] = [
    "minecraft:empty",
    "minecraft:structure_starts",
    "minecraft:structure_references",
    "minecraft:biomes",
    "minecraft:noise",
    "minecraft:surface",
    "minecraft:carvers",
    STATUS_TERRAIN,
    STATUS_FEATURES,
    "minecraft:initialize_light",
    "minecraft:light",
    "minecraft:spawn",
    tuff_world_model::chunk_meta::STATUS_FULL,
];

const HEIGHTMAPS: [(HeightmapKind, &str); 6] = [
    (HeightmapKind::WorldSurfaceWg, "WORLD_SURFACE_WG"),
    (HeightmapKind::WorldSurface, "WORLD_SURFACE"),
    (HeightmapKind::OceanFloorWg, "OCEAN_FLOOR_WG"),
    (HeightmapKind::OceanFloor, "OCEAN_FLOOR"),
    (HeightmapKind::MotionBlocking, "MOTION_BLOCKING"),
    (
        HeightmapKind::MotionBlockingNoLeaves,
        "MOTION_BLOCKING_NO_LEAVES",
    ),
];

/// What storage holds for a chunk the pipeline needs.
pub enum Saved {
    /// A chunk in generation past its terrain, and whether its own
    /// features are placed.
    Proto(Box<ProtoChunk>, bool),
    /// A finished chunk, which only its region may touch.
    Full,
    /// Nothing usable: the terrain is generated afresh.
    Nothing,
}

fn status_rank(status: &str) -> Option<usize> {
    STATUSES.iter().position(|s| *s == status)
}

/// The compound a chunk in generation is saved as.
pub fn to_nbt(proto: &ProtoChunk, decorated: bool) -> Compound {
    let shape = proto.shape;
    let mut extra = Compound::new();
    let mut heightmaps = Compound::new();
    let bits = ceil_log2(shape.height as usize + 1);
    for (kind, name) in HEIGHTMAPS {
        if let Some(heights) = proto.heightmap(kind) {
            let values = heights.iter().map(|h| (h - shape.min_y) as u64);
            let words: Vec<i64> = pack(bits, values).into_iter().map(|w| w as i64).collect();
            heightmaps.insert(name, Tag::LongArray(words));
        }
    }
    extra.insert("Heightmaps", heightmaps);
    let min_section = shape.min_y >> 4;
    let mut sections = vec![Vec::new(); shape.sections()];
    for &(x, y, z) in &proto.post_processing {
        if let Some(list) = usize::try_from((y >> 4) - min_section)
            .ok()
            .and_then(|i| sections.get_mut(i))
        {
            list.push(Tag::Short(
                ((x & 15) | (y & 15) << 4 | (z & 15) << 8) as i16,
            ));
        }
    }
    extra.insert(
        "PostProcessing",
        Tag::List(sections.into_iter().map(Tag::List).collect()),
    );
    for (key, ticks) in [
        ("fluid_ticks", &proto.fluid_ticks),
        ("block_ticks", &proto.block_ticks),
    ] {
        let list = ticks.iter().map(|t| Tag::Compound(t.to_nbt())).collect();
        extra.insert(key, Tag::List(list));
    }
    extra.insert(
        "entities",
        Tag::List(proto.entities.iter().cloned().map(Tag::Compound).collect()),
    );
    extra.insert(
        "structures",
        crate::structure::structures_nbt(&proto.structure_starts, &proto.structure_references),
    );
    let raw_block_entities = proto
        .block_entities
        .iter()
        .map(|(&(x, y, z), data)| (BlockPos::new(x, y, z), data.clone()))
        .collect();
    let meta = ChunkMeta {
        status: if decorated {
            STATUS_FEATURES
        } else {
            STATUS_TERRAIN
        }
        .into(),
        extra,
        relight: false,
        block_entities: HashMap::new(),
        raw_block_entities,
        below_zero_retrogen: false,
    };
    chunk_nbt::encode(&proto.clone().into_chunk(), &meta, proto.pos, 0)
}

/// What a saved compound holds for the pipeline at `pos`.
pub fn from_nbt(root: &Compound, pos: ChunkPos, shape: ChunkShape) -> Saved {
    if root.get_int("xPos") != Some(pos.x) || root.get_int("zPos") != Some(pos.z) {
        return Saved::Nothing;
    }
    if root
        .get_int("DataVersion")
        .is_some_and(|v| v > tuff_vanilla_data::version::DATA_VERSION)
    {
        return Saved::Nothing;
    }
    let Some(rank) = root.get_str("Status").and_then(status_rank) else {
        return Saved::Nothing;
    };
    if rank == STATUSES.len() - 1 {
        return Saved::Full;
    }
    if rank < status_rank("minecraft:carvers").expect("carvers is a status") {
        return Saved::Nothing;
    }
    let decorated = rank >= status_rank(STATUS_FEATURES).expect("features is a status");
    match proto(root, pos, shape) {
        Some(proto) => Saved::Proto(Box::new(proto), decorated),
        None => Saved::Nothing,
    }
}

fn proto(root: &Compound, pos: ChunkPos, shape: ChunkShape) -> Option<ProtoChunk> {
    let min_section = shape.min_y >> 4;
    let mut sections = Vec::with_capacity(shape.sections());
    let mut biomes = vec![0u16; shape.sections() * BIOME_VOLUME];
    let mut found = vec![None; shape.sections()];
    for tag in root.get_list("sections").unwrap_or(&[]) {
        let section = tag.as_compound()?;
        let index = section.get_byte("Y")? as i32 - min_section;
        if let Some(slot) = usize::try_from(index).ok().and_then(|i| found.get_mut(i))
            && section.contains_key("block_states")
        {
            *slot = Some(chunk_nbt::decode_section(section).ok()?);
        }
    }
    for (index, section) in found.into_iter().enumerate() {
        let section = section?;
        for (i, biome) in biomes[index * BIOME_VOLUME..(index + 1) * BIOME_VOLUME]
            .iter_mut()
            .enumerate()
        {
            *biome = section.biome(i);
        }
        sections.push(section);
    }
    let mut proto = ProtoChunk::resumed(pos, shape, sections, biomes);
    let heights = root.get_compound("Heightmaps");
    let bits = ceil_log2(shape.height as usize + 1);
    for (kind, name) in HEIGHTMAPS {
        let Some(words) = heights.and_then(|h| h.get_long_array(name)) else {
            continue;
        };
        let Some(values) = unpack(bits, 256, words) else {
            continue;
        };
        let mut heightmap = [0i32; 256];
        for (out, value) in heightmap.iter_mut().zip(values) {
            *out = value as i32 + shape.min_y;
        }
        proto.set_heightmap(kind, heightmap);
    }
    for (index, list) in root
        .get_list("PostProcessing")
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let section_y = min_section + index as i32;
        for packed in list.as_list().unwrap_or(&[]) {
            let Some(packed) = packed.as_short() else {
                continue;
            };
            let packed = packed as i32;
            proto.post_processing.push((
                (pos.x << 4) + (packed & 15),
                (section_y << 4) + ((packed >> 4) & 15),
                (pos.z << 4) + ((packed >> 8) & 15),
            ));
        }
    }
    proto.fluid_ticks = ticks(root, "fluid_ticks", |name| {
        tuff_vanilla_data::fluid::Fluid::from_name(name).map(Registry::name)
    });
    proto.block_ticks = ticks(root, "block_ticks", |name| {
        tuff_vanilla_data::Block::from_name(name).map(Registry::name)
    });
    for tag in root.get_list("block_entities").unwrap_or(&[]) {
        let Some(data) = tag.as_compound() else {
            continue;
        };
        if let (Some(x), Some(y), Some(z)) =
            (data.get_int("x"), data.get_int("y"), data.get_int("z"))
        {
            proto.block_entities.insert((x, y, z), data.clone());
        }
    }
    proto.entities = root
        .get_list("entities")
        .unwrap_or(&[])
        .iter()
        .filter_map(|tag| tag.as_compound().cloned())
        .collect();
    proto.saved_starts = root
        .get_compound("structures")
        .and_then(|s| s.get_compound("starts"))
        .cloned();
    Some(proto)
}

fn ticks(
    root: &Compound,
    key: &str,
    kind: impl Fn(&str) -> Option<&'static str>,
) -> Vec<ScheduledTick> {
    root.get_list(key)
        .unwrap_or(&[])
        .iter()
        .filter_map(|tag| {
            let tick = tag.as_compound()?;
            Some(ScheduledTick {
                kind: kind(tick.get_str("i")?)?,
                pos: (tick.get_int("x")?, tick.get_int("y")?, tick.get_int("z")?),
                delay: tick.get_int("t").unwrap_or(0),
            })
        })
        .collect()
}

/// A finished chunk taken back into generation, as it was before it was
/// finished: its region never got it.
pub fn unfinished(pos: ChunkPos, finished: Finished) -> ProtoChunk {
    let mut proto = ProtoChunk::from_chunk(pos, &finished.chunk);
    for data in finished.block_entities {
        if let (Some(x), Some(y), Some(z)) =
            (data.get_int("x"), data.get_int("y"), data.get_int("z"))
        {
            proto.block_entities.insert((x, y, z), data);
        }
    }
    proto.post_processing = finished
        .post_processing
        .iter()
        .map(|p| (p.x, p.y, p.z))
        .collect();
    proto.fluid_ticks = finished.fluid_ticks;
    proto.block_ticks = finished.block_ticks;
    proto.entities = finished.entities;
    proto
}
