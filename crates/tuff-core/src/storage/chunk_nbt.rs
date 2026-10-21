//! Converts between [`Chunk`]s and vanilla's region chunk NBT
//! (`SerializableChunkData`). Everything this server does not model (block
//! entities, structures, scheduled ticks, post-processing, ...) is kept in
//! [`ChunkMeta::extra`] and written back untouched, so vanilla data survives
//! a round trip through this server.

use tuff_storage::error::ChunkNbtError;
use tuff_vanilla_data::version::DATA_VERSION;
use tuff_vanilla_data::{Block, BlockState};

use crate::world::block_entity::{self, BlockEntity};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::{
    BIOME_VOLUME, Chunk, ChunkShape, LIGHT_BYTES, SECTION_VOLUME, Section, ceil_log2, pack, plains,
    unpack,
};
use tuff_world_model::chunk_meta::ChunkMeta;
use tuff_world_model::light::{ChunkLight, SectionLight};
use tuff_world_model::pos::ChunkPos;

/// Statuses of chunks still in generation, which their generation
/// pipeline resumes from storage itself.
const UNGENERATED_STATUSES: [&str; 12] = [
    "minecraft:empty",
    "minecraft:structure_starts",
    "minecraft:structure_references",
    "minecraft:biomes",
    "minecraft:noise",
    "minecraft:surface",
    "minecraft:carvers",
    "minecraft:terrain",
    "minecraft:features",
    "minecraft:initialize_light",
    "minecraft:light",
    "minecraft:spawn",
];
const MODELED_KEYS: [&str; 8] = [
    "DataVersion",
    "xPos",
    "yPos",
    "zPos",
    "LastUpdate",
    "Status",
    "sections",
    "isLightOn",
];
/// Data of a chunk in generation that the generator carries into the
/// finished chunk itself.
const PROTO_KEYS: [&str; 7] = [
    "Heightmaps",
    "PostProcessing",
    "fluid_ticks",
    "block_ticks",
    "block_entities",
    "entities",
    "carving_mask",
];
const MIN_BLOCK_BITS: u32 = 4;
const BLOCK_ENTITIES: &str = "block_entities";

type Meta = ChunkMeta<BlockEntity>;
type Decoded = tuff_world_model::chunk_meta::Decoded<BlockEntity>;

/// Every block entity as the chunk packet lists it.
pub fn client_block_entities(meta: &Meta) -> Vec<(BlockPos, u32, Compound)> {
    let modeled = meta.block_entities.iter().map(|(pos, entity)| {
        (
            *pos,
            entity.kind.entity_type().id(),
            entity.client_data().unwrap_or_default(),
        )
    });
    let raw = meta
        .raw_block_entities
        .values()
        .filter_map(block_entity::raw::client_data)
        .map(|(pos, kind, data)| (pos, kind.id(), data));
    modeled.chain(raw).collect()
}

/// Every block entity in its saved form, in a stable order.
fn saved_block_entities(meta: &Meta) -> Vec<Tag> {
    let mut saved: Vec<(BlockPos, Compound)> = meta
        .block_entities
        .iter()
        .map(|(pos, entity)| (*pos, entity.encode(*pos)))
        .chain(
            meta.raw_block_entities
                .iter()
                .map(|(pos, nbt)| (*pos, nbt.clone())),
        )
        .collect();
    saved.sort_by_key(|(pos, _)| (pos.y, pos.z, pos.x));
    saved
        .into_iter()
        .map(|(_, nbt)| Tag::Compound(nbt))
        .collect()
}

pub fn decode(
    root: Compound,
    expected: ChunkPos,
    shape: ChunkShape,
) -> Result<Decoded, ChunkNbtError> {
    decode_in(root, expected, shape, Biomes::vanilla())
}

/// [`decode`] naming biomes with `biomes`, which may hold custom ones.
pub fn decode_in(
    mut root: Compound,
    expected: ChunkPos,
    shape: ChunkShape,
    biomes: &Biomes,
) -> Result<Decoded, ChunkNbtError> {
    let (sections_len, light_len, min_section) = (
        shape.sections(),
        shape.light_sections(),
        shape.min_section(),
    );
    let version = root.get_int("DataVersion").unwrap_or(0);
    if version > DATA_VERSION {
        return Err(ChunkNbtError::NewerVersion(version));
    }
    tuff_storage::datafix::chunk_in(&mut root, min_section, shape.overworld)?;
    let pos = ChunkPos::new(
        root.get_int("xPos").unwrap_or(0),
        root.get_int("zPos").unwrap_or(0),
    );
    if pos != expected {
        return Err(ChunkNbtError::WrongPosition(pos));
    }
    let mut status = root.get_str("Status").unwrap_or("").to_owned();
    let retrogen = root.remove("below_zero_retrogen");
    if retrogen.is_some() {
        status = tuff_world_model::chunk_meta::STATUS_FULL.to_owned();
    }
    if status.is_empty() || UNGENERATED_STATUSES.contains(&status.as_str()) {
        strip_modeled(&mut root);
        for key in PROTO_KEYS {
            root.remove(key);
        }
        return Ok(Decoded::Ungenerated(root));
    }
    let light_on = root.get_bool("isLightOn").unwrap_or(false);
    let mut sections: Vec<Section> = (0..sections_len)
        .map(|_| Section::uniform(Block::Air.default_state(), plains()))
        .collect();
    let mut light = ChunkLight {
        sky: vec![SectionLight::Missing; light_len],
        block: vec![SectionLight::Missing; light_len],
    };
    let mut any_light = false;
    for tag in root.get_list("sections").unwrap_or(&[]) {
        let Some(section) = tag.as_compound() else {
            continue;
        };
        let y = section.get_byte("Y").unwrap_or(0) as i32;
        let light_index = y - min_section + 1;
        if (0..light_len as i32).contains(&light_index) {
            for (key, layers) in [
                ("SkyLight", &mut light.sky),
                ("BlockLight", &mut light.block),
            ] {
                if let Some(data) = section.get_byte_array(key)
                    && data.len() == LIGHT_BYTES
                {
                    let mut array = Box::new([0u8; LIGHT_BYTES]);
                    for (out, value) in array.iter_mut().zip(data) {
                        *out = *value as u8;
                    }
                    layers[light_index as usize] = SectionLight::Data(array);
                    any_light = true;
                }
            }
        }
        let index = y - min_section;
        if !(0..sections_len as i32).contains(&index) {
            continue;
        }
        sections[index as usize] = decode_section_in(section, biomes)?;
    }
    strip_modeled(&mut root);
    let (mut block_entities, mut raw_block_entities) = match root.remove(BLOCK_ENTITIES) {
        Some(Tag::List(list)) => {
            block_entity::split(list.into_iter().filter_map(|tag| match tag {
                Tag::Compound(nbt) => Some(nbt),
                _ => None,
            }))
        }
        _ => Default::default(),
    };
    let light = (light_on && any_light).then_some(light);
    let mut chunk = Chunk::from_sections(shape, sections, light);
    let mut retrogen_applied = false;
    if let Some(Tag::Compound(retrogen)) = &retrogen
        && shape.overworld
    {
        below_zero_retrogen(&mut chunk, retrogen, pos);
        retrogen_applied = true;
    }
    block_entity::fill_missing(&chunk, pos, &mut block_entities, &mut raw_block_entities);
    let meta = ChunkMeta {
        status,
        extra: root,
        relight: !light_on,
        block_entities,
        raw_block_entities,
        below_zero_retrogen: retrogen_applied,
    };
    Ok(Decoded::Chunk(Box::new(chunk), meta))
}

fn below_zero_retrogen(chunk: &mut Chunk, retrogen: &Compound, pos: ChunkPos) {
    let missing = retrogen.get_long_array("missing_bedrock").unwrap_or(&[]);
    let deepslate = Block::Deepslate.default_state();
    let bedrock = Block::Bedrock.default_state();
    for z in 0..16i32 {
        for x in 0..16i32 {
            let bit = ((z & 3) << 4) | x;
            let lacks_bedrock = missing
                .get((z >> 2) as usize)
                .is_some_and(|word| (*word >> bit) & 1 != 0);
            for y in -64..0 {
                if chunk.get(x, y, z).block() != Block::Air {
                    continue;
                }
                let depth = (y + 64) as u32;
                let place_bedrock = lacks_bedrock && depth < 5 && {
                    let h = (pos.x as u32)
                        .wrapping_mul(0x9E37_79B1)
                        .wrapping_add((pos.z as u32).wrapping_mul(0x85EB_CA6B))
                        .wrapping_add(((x as u32) << 8) | ((z as u32) << 4) | depth)
                        .wrapping_mul(0xC2B2_AE35);
                    depth == 0 || (h >> 16) % 5 >= depth
                };
                chunk.set(x, y, z, if place_bedrock { bedrock } else { deepslate });
            }
        }
    }
}

fn strip_modeled(root: &mut Compound) {
    for key in MODELED_KEYS {
        root.remove(key);
    }
}

pub fn decode_section(section: &Compound) -> Result<Section, ChunkNbtError> {
    decode_section_in(section, Biomes::vanilla())
}

fn decode_section_in(
    section: &Compound,
    biomes_by_name: &Biomes,
) -> Result<Section, ChunkNbtError> {
    let mut blocks = Box::new([Block::Air.default_state(); SECTION_VOLUME]);
    if let Some(states) = section.get_compound("block_states") {
        let palette: Vec<BlockState> = states
            .get_list("palette")
            .ok_or(ChunkNbtError::Malformed("block palette missing"))?
            .iter()
            .map(decode_block_state)
            .collect();
        fill(
            &mut blocks[..],
            &palette,
            states.get_long_array("data"),
            ceil_log2(palette.len()).max(MIN_BLOCK_BITS),
            Block::Air.default_state(),
        )?;
    }
    let mut biomes = Box::new([plains(); BIOME_VOLUME]);
    if let Some(container) = section.get_compound("biomes") {
        let palette: Vec<u16> = container
            .get_list("palette")
            .ok_or(ChunkNbtError::Malformed("biome palette missing"))?
            .iter()
            .map(|tag| {
                tag.as_str()
                    .and_then(|name| biomes_by_name.id(name))
                    .unwrap_or_else(plains)
            })
            .collect();
        fill(
            &mut biomes[..],
            &palette,
            container.get_long_array("data"),
            ceil_log2(palette.len()),
            plains(),
        )?;
    }
    Ok(Section::new(blocks, biomes))
}

/// Expands a paletted container into `out`. Indices past the palette fall
/// back to `default`, as vanilla does.
fn fill<T: Copy>(
    out: &mut [T],
    palette: &[T],
    data: Option<&[i64]>,
    bits: u32,
    default: T,
) -> Result<(), ChunkNbtError> {
    match palette {
        [] => return Err(ChunkNbtError::Malformed("empty palette")),
        [single] => out.fill(*single),
        _ => {
            let data = data.ok_or(ChunkNbtError::Malformed("palette data missing"))?;
            let indices = unpack(bits, out.len(), data).ok_or(ChunkNbtError::Malformed(
                "palette data has the wrong length",
            ))?;
            for (slot, index) in out.iter_mut().zip(indices) {
                *slot = palette.get(index as usize).copied().unwrap_or(default);
            }
        }
    }
    Ok(())
}

/// Accepts the 26.x forms (a bare id for a default state, or
/// `{id, properties}`) and the older `{Name, Properties}` form. Unknown
/// blocks become air; unknown or missing properties keep their defaults.
/// Extra entry keys are ignored.
pub fn decode_block_state(tag: &Tag) -> BlockState {
    let (name, properties) = match tag {
        Tag::String(name) => (name.as_str(), None),
        Tag::Compound(entry) => (
            entry
                .get_str("id")
                .or_else(|| entry.get_str("Name"))
                .unwrap_or(""),
            entry
                .get_compound("properties")
                .or_else(|| entry.get_compound("Properties")),
        ),
        _ => ("", None),
    };
    let Some(block) = Block::from_name(name) else {
        return Block::Air.default_state();
    };
    let mut state = block.default_state();
    for (key, value) in properties.into_iter().flat_map(Compound::iter) {
        if let Some(value) = value.as_str()
            && let Some(next) = state.with(key, value)
        {
            state = next;
        }
    }
    state
}

/// The saved form of `state`: a bare id for a default vanilla state, else
/// `{id, properties}`.
pub fn encode_block_state(state: BlockState) -> Tag {
    let id = state.block().name();
    if state.is_default() {
        return Tag::from(id);
    }
    let mut entry = Compound::new().with("id", id);
    if state.properties().len() > 0 {
        entry.insert("properties", state.properties().collect::<Compound>());
    }
    Tag::Compound(entry)
}

fn container(palette: Vec<Tag>, indices: &[u16], bits: u32) -> Compound {
    let mut out = Compound::new().with("palette", Tag::List(palette));
    if !indices.is_empty() {
        let words = pack(bits, indices.iter().map(|i| *i as u64));
        out.insert(
            "data",
            words.into_iter().map(|w| w as i64).collect::<Vec<_>>(),
        );
    }
    out
}

/// Builds the region NBT for a chunk. `last_update` is the game time.
pub fn encode(chunk: &Chunk, meta: &Meta, pos: ChunkPos, last_update: i64) -> Compound {
    encode_in(chunk, meta, pos, last_update, Biomes::vanilla())
}

/// [`encode`] naming biomes with `biomes_by_id`, which may hold custom ones.
pub fn encode_in(
    chunk: &Chunk,
    meta: &Meta,
    pos: ChunkPos,
    last_update: i64,
    biomes_by_id: &Biomes,
) -> Compound {
    let shape = chunk.shape();
    let min_section = shape.min_section();
    let mut root = Compound::new()
        .with("DataVersion", DATA_VERSION)
        .with("xPos", pos.x)
        .with("yPos", min_section)
        .with("zPos", pos.z)
        .with("LastUpdate", last_update)
        .with("Status", meta.status.as_str());
    let light = chunk.light();
    let blocks: Vec<&Section> = chunk.sections().collect();
    let mut sections = Vec::with_capacity(shape.light_sections());
    for light_index in 0..shape.light_sections() {
        let y = min_section - 1 + light_index as i32;
        let mut out = Compound::new();
        if let Some(section) = (y - min_section)
            .try_into()
            .ok()
            .and_then(|i: usize| blocks.get(i))
        {
            let (states, indices) = section.block_palette();
            let bits = ceil_log2(states.len()).max(MIN_BLOCK_BITS);
            let palette = states.into_iter().map(encode_block_state).collect();
            out.insert("block_states", container(palette, &indices, bits));
            let (biomes, indices) = section.biome_palette();
            let bits = ceil_log2(biomes.len());
            let palette = biomes
                .into_iter()
                .map(|b| Tag::from(biomes_by_id.name(b)))
                .collect();
            out.insert("biomes", container(palette, &indices, bits));
        }
        if let Some(light) = light {
            for (key, layers) in [("BlockLight", &light.block), ("SkyLight", &light.sky)] {
                if let SectionLight::Data(data) = &layers[light_index] {
                    out.insert(key, data.iter().map(|b| *b as i8).collect::<Vec<_>>());
                }
            }
        }
        if !out.is_empty() {
            out.insert("Y", y as i8);
            sections.push(out);
        }
    }
    root.insert("sections", sections);
    if light.is_some() {
        root.insert("isLightOn", true);
    }
    for (key, value) in meta.extra.iter() {
        if meta.relight && key == "Heightmaps" {
            continue;
        }
        root.insert(key, value.clone());
    }
    root.insert(BLOCK_ENTITIES, Tag::List(saved_block_entities(meta)));
    for (key, empty) in [
        ("block_ticks", Tag::List(Vec::new())),
        ("fluid_ticks", Tag::List(Vec::new())),
        (
            "PostProcessing",
            Tag::List(
                (0..shape.sections())
                    .map(|_| Tag::List(Vec::new()))
                    .collect(),
            ),
        ),
        (
            "structures",
            Tag::Compound(
                Compound::new()
                    .with("References", Compound::new())
                    .with("starts", Compound::new()),
            ),
        ),
    ] {
        if !root.contains_key(key) {
            root.insert(key, empty);
        }
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_nbt::{NamedTag, NbtLimits};
    use tuff_storage::region_file::RegionFile;
    use tuff_storage::test_dir::TestDir;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/vanilla-26.3"
    );

    fn fixture_chunk(pos: ChunkPos) -> Compound {
        let dir = TestDir::new();
        std::fs::copy(
            std::path::Path::new(FIXTURES).join("r.0.0.deflate.mca"),
            dir.path().join("r.0.0.mca"),
        )
        .unwrap();
        let mut region = RegionFile::open(dir.path(), 0, 0, false).unwrap().unwrap();
        let raw = region.read(pos).unwrap().unwrap();
        let bytes = raw.decompress(1 << 24).unwrap();
        let named = NamedTag::read(&mut &bytes[..], NbtLimits::TRUSTED).unwrap();
        match named.tag {
            Tag::Compound(root) => root,
            _ => panic!("not a compound"),
        }
    }

    fn decode_full(root: Compound, pos: ChunkPos) -> (Chunk, Meta) {
        match decode(root, pos, ChunkShape::OVERWORLD).unwrap() {
            Decoded::Chunk(chunk, meta) => (*chunk, meta),
            Decoded::Ungenerated(_) => panic!("expected terrain"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_upgraded_chunk_awaiting_below_zero_retrogen_keeps_its_blocks() {
        let pos = ChunkPos::new(10, 10);
        let mut root = fixture_chunk(pos);
        root.insert("Status", "empty");
        root.insert(
            "below_zero_retrogen",
            Compound::new().with("target_status", "heightmaps"),
        );
        let (chunk, meta) = decode_full(root, pos);
        assert_eq!(meta.status, tuff_world_model::chunk_meta::STATUS_FULL);
        assert!(!meta.extra.contains_key("below_zero_retrogen"));
        assert_eq!(chunk.get(0, -64, 0).block(), Block::Bedrock);
    }

    #[tuff_core::watchdog]
    #[test]
    fn below_zero_retrogen_fills_air_below_zero_with_deepslate_and_bedrock() {
        let pos = ChunkPos::new(10, 10);
        let mut root = fixture_chunk(pos);
        root.insert(
            "below_zero_retrogen",
            Compound::new()
                .with("target_status", "heightmaps")
                .with("missing_bedrock", vec![1i64]),
        );
        let (chunk, _) = decode_full(root, pos);
        assert_eq!(chunk.get(0, -64, 0).block(), Block::Bedrock);
        for y in -64..0 {
            assert_ne!(chunk.get(5, y, 5).block(), Block::Air);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn decodes_vanilla_terrain() {
        let pos = ChunkPos::new(10, 10);
        let (chunk, meta) = decode_full(fixture_chunk(pos), pos);
        assert_eq!(meta.status, tuff_world_model::chunk_meta::STATUS_FULL);
        assert!(!meta.relight);
        assert!(chunk.light().is_some());
        assert_eq!(chunk.get(0, -64, 0).block(), Block::Bedrock);
        let surface = (-64..320)
            .rev()
            .map(|y| chunk.get(8, y, 8))
            .find(|s| !tuff_world_model::chunk::is_air(*s))
            .unwrap();
        assert_ne!(surface.block(), Block::Stone);
        assert!(meta.extra.contains_key("Heightmaps"));
        assert!(meta.extra.contains_key("block_ticks"));
        assert!(!meta.extra.contains_key("sections"));
        let leaves = chunk
            .sections()
            .flat_map(|s| s.block_palette().0)
            .find(|s| s.block() == Block::OakLeaves);
        assert!(leaves.is_some_and(|s| !s.is_default()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn proto_chunks_are_ungenerated_and_keep_their_data() {
        let pos = ChunkPos::new(0, 0);
        match decode(fixture_chunk(pos), pos, ChunkShape::OVERWORLD).unwrap() {
            Decoded::Ungenerated(extra) => {
                assert!(extra.contains_key("structures"));
                assert!(!extra.contains_key("Status"));
                assert!(!extra.contains_key("Heightmaps"));
            }
            Decoded::Chunk(..) => panic!("structure_starts has no terrain"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_chunk_round_trips_exactly() {
        let pos = ChunkPos::new(10, 10);
        let original = fixture_chunk(pos);
        let (chunk, meta) = decode_full(original.clone(), pos);
        let encoded = encode(&chunk, &meta, pos, original.get_long("LastUpdate").unwrap());
        let (again, again_meta) = decode_full(encoded.clone(), pos);
        assert_eq!(again, chunk);
        assert_eq!(again_meta, meta);
        let mut original_keys: Vec<&str> = original.keys().collect();
        let mut encoded_keys: Vec<&str> = encoded.keys().collect();
        original_keys.sort();
        encoded_keys.sort();
        assert_eq!(original_keys, encoded_keys);
        let sections = |root: &Compound| root.get_list("sections").unwrap().to_vec();
        for (ours, theirs) in sections(&encoded).iter().zip(sections(&original)) {
            let (ours, theirs) = (ours.as_compound().unwrap(), theirs.as_compound().unwrap());
            assert_eq!(ours.get_byte("Y"), theirs.get_byte("Y"));
            for key in ["BlockLight", "SkyLight"] {
                assert_eq!(ours.get(key), theirs.get(key), "{key}");
            }
            let states = |c: &Compound| c.get_compound("block_states").cloned();
            if let (Some(ours), Some(theirs)) = (states(ours), states(theirs)) {
                let set = |c: &Compound| {
                    let mut palette: Vec<BlockState> = c
                        .get_list("palette")
                        .unwrap()
                        .iter()
                        .map(decode_block_state)
                        .collect();
                    palette.sort();
                    palette
                };
                assert_eq!(set(&ours), set(&theirs));
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn edits_drop_derived_data() {
        let pos = ChunkPos::new(10, 10);
        let (mut chunk, mut meta) = decode_full(fixture_chunk(pos), pos);
        chunk.set(0, 70, 0, Block::Air.default_state());
        meta.block_changed();
        assert!(encode(&chunk, &meta, pos, 5).get_bool("isLightOn") == Some(true));
        chunk.set_light(None);
        let encoded = encode(&chunk, &meta, pos, 5);
        assert!(!encoded.contains_key("Heightmaps"));
        assert!(!encoded.contains_key("isLightOn"));
        assert!(
            encoded
                .get_list("sections")
                .unwrap()
                .iter()
                .all(|s| !s.as_compound().unwrap().contains_key("SkyLight"))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_entities_round_trip_and_stale_ones_are_dropped_on_load() {
        use crate::world::block_entity::{ContainerKind, raw};
        use tuff_item::item::ItemStack;
        use tuff_vanilla_data::Item;
        use tuff_vanilla_data::registry::BlockEntityType;

        let pos = ChunkPos::new(10, 10);
        let (mut chunk, mut meta) = decode_full(fixture_chunk(pos), pos);
        let sign_at = BlockPos::new(162, 70, 160);
        let chest_at = BlockPos::new(160, 70, 161);
        let stale_at = BlockPos::new(163, 70, 160);
        chunk.set(2, 70, 0, Block::OakSign.default_state());
        chunk.set(0, 70, 1, Block::Chest.default_state());
        chunk.set(3, 70, 0, Block::Stone.default_state());
        let sign = raw::new(BlockEntityType::Sign, sign_at).with("is_waxed", true);
        let mut chest = BlockEntity::new(ContainerKind::Chest);
        chest.items[4] = Some(ItemStack::new(Item::Apple, 7));
        meta.raw_block_entities.insert(sign_at, sign.clone());
        meta.raw_block_entities
            .insert(stale_at, raw::new(BlockEntityType::Bell, stale_at));
        meta.block_entities.insert(chest_at, chest.clone());
        let encoded = encode(&chunk, &meta, pos, 5);
        assert_eq!(encoded.get_list(BLOCK_ENTITIES).unwrap().len(), 3);
        let Decoded::Chunk(_, reloaded) = decode(encoded, pos, ChunkShape::OVERWORLD).unwrap()
        else {
            panic!("full chunk");
        };
        assert_eq!(reloaded.block_entities.get(&chest_at), Some(&chest));
        assert_eq!(
            reloaded.raw_block_entities,
            std::collections::HashMap::from([(sign_at, sign)])
        );
        assert!(!reloaded.extra.contains_key(BLOCK_ENTITIES));
    }

    #[tuff_core::watchdog]
    #[test]
    fn blocks_missing_their_block_entity_get_one_on_load() {
        use tuff_vanilla_data::registry::BlockEntityType;

        let pos = ChunkPos::new(10, 10);
        let (mut chunk, meta) = decode_full(fixture_chunk(pos), pos);
        chunk.set(5, 80, 5, Block::Bell.default_state());
        chunk.set(6, 80, 5, Block::Barrel.default_state());
        chunk.set(7, 80, 5, Block::MovingPiston.default_state());
        let encoded = encode(&chunk, &meta, pos, 5);
        let (_, reloaded) = decode_full(encoded, pos);
        let bell = BlockPos::new(165, 80, 165);
        assert_eq!(
            reloaded
                .raw_block_entities
                .get(&bell)
                .and_then(crate::world::block_entity::raw::kind),
            Some(BlockEntityType::Bell)
        );
        assert!(
            reloaded
                .block_entities
                .contains_key(&BlockPos::new(166, 80, 165))
        );
        assert!(
            !reloaded
                .raw_block_entities
                .contains_key(&BlockPos::new(167, 80, 165))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_block_entity_is_listed_for_clients() {
        use crate::world::block_entity::{ContainerKind, raw};
        use tuff_vanilla_data::registry::BlockEntityType;

        let chest = BlockPos::new(1, 64, 2);
        let sign = BlockPos::new(3, 64, 4);
        let ender_chest = BlockPos::new(5, 64, 6);
        let sand = BlockPos::new(9, 64, 10);
        let mut meta = ChunkMeta::generated();
        meta.block_entities
            .insert(chest, BlockEntity::new(ContainerKind::Chest));
        for (at, nbt) in [
            (
                sign,
                raw::new(BlockEntityType::Sign, sign).with("is_waxed", true),
            ),
            (
                ender_chest,
                raw::new(BlockEntityType::EnderChest, ender_chest),
            ),
            (
                sand,
                raw::new(BlockEntityType::BrushableBlock, sand)
                    .with("hit_direction", 1)
                    .with("LootTable", "minecraft:archaeology/desert_well"),
            ),
        ] {
            meta.raw_block_entities.insert(at, nbt);
        }
        let mut listed = client_block_entities(&meta);
        listed.sort_by_key(|(pos, ..)| pos.x);
        let blank = Compound::new()
            .with("color", "black")
            .with("has_glowing_text", false)
            .with(
                "messages",
                vec![Tag::from(""), "".into(), "".into(), "".into()],
            );
        assert_eq!(
            listed,
            vec![
                (chest, BlockEntityType::Chest.id(), Compound::new()),
                (
                    sign,
                    BlockEntityType::Sign.id(),
                    Compound::new()
                        .with("back_text", blank.clone())
                        .with("front_text", blank)
                        .with("is_waxed", true)
                ),
                (
                    ender_chest,
                    BlockEntityType::EnderChest.id(),
                    Compound::new()
                ),
                (
                    sand,
                    BlockEntityType::BrushableBlock.id(),
                    Compound::new().with("hit_direction", 1)
                ),
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn generated_chunks_encode_vanilla_shapes() {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(1, 0, 1, Block::Stone.default_state());
        chunk.set(
            2,
            0,
            1,
            Block::OakLeaves
                .default_state()
                .with("distance", "2")
                .unwrap(),
        );
        let encoded = encode(&chunk, &ChunkMeta::generated(), ChunkPos::new(-3, 4), 0);
        assert_eq!(encoded.get_int("DataVersion"), Some(DATA_VERSION));
        assert_eq!(
            encoded.get_str("Status"),
            Some(tuff_world_model::chunk_meta::STATUS_FULL)
        );
        let sections = encoded.get_list("sections").unwrap();
        assert_eq!(sections.len(), ChunkShape::OVERWORLD.sections());
        let section = sections[4].as_compound().unwrap();
        assert_eq!(section.get_byte("Y"), Some(0));
        let states = section.get_compound("block_states").unwrap();
        let palette = states.get_list("palette").unwrap();
        assert_eq!(palette[0], Tag::from("minecraft:air"));
        let leaves = palette[2].as_compound().unwrap();
        assert_eq!(leaves.get_str("id"), Some("minecraft:oak_leaves"));
        assert_eq!(
            leaves
                .get_compound("properties")
                .unwrap()
                .get_str("distance"),
            Some("2")
        );
        assert_eq!(states.get_long_array("data").unwrap().len(), 256);
        let empty = sections[20].as_compound().unwrap();
        assert!(
            !empty
                .get_compound("block_states")
                .unwrap()
                .contains_key("data")
        );
        assert_eq!(
            encoded.get_list("PostProcessing").unwrap().len(),
            ChunkShape::OVERWORLD.sections()
        );
        let (decoded, _) = decode_full(encoded, ChunkPos::new(-3, 4));
        assert_eq!(decoded, chunk);
    }

    #[tuff_core::watchdog]
    #[test]
    fn chunks_round_trip_in_a_zero_based_shape() {
        let shape = ChunkShape::new(0, 256);
        let pos = ChunkPos::new(2, -9);
        let mut chunk = Chunk::empty(shape);
        chunk.set(3, 0, 3, Block::Bedrock.default_state());
        chunk.set(3, 255, 3, Block::Netherrack.default_state());
        chunk.set_light(Some(ChunkLight {
            sky: vec![SectionLight::Missing; shape.light_sections()],
            block: (0..shape.light_sections())
                .map(|i| {
                    if i == 1 {
                        SectionLight::Data(Box::new([0x33; LIGHT_BYTES]))
                    } else {
                        SectionLight::Missing
                    }
                })
                .collect(),
        }));
        let encoded = encode(&chunk, &ChunkMeta::generated(), pos, 1);
        assert_eq!(encoded.get_int("yPos"), Some(0));
        let sections = encoded.get_list("sections").unwrap();
        assert_eq!(sections.len(), shape.sections());
        let ys: Vec<i8> = sections
            .iter()
            .map(|s| s.as_compound().unwrap().get_byte("Y").unwrap())
            .collect();
        assert_eq!(ys.first(), Some(&0));
        assert_eq!(ys.last(), Some(&15));
        assert!(
            sections[0]
                .as_compound()
                .unwrap()
                .contains_key("BlockLight")
        );
        assert!(
            !sections[1]
                .as_compound()
                .unwrap()
                .contains_key("BlockLight")
        );
        assert_eq!(
            encoded.get_list("PostProcessing").unwrap().len(),
            shape.sections()
        );
        let Decoded::Chunk(decoded, meta) = decode(encoded, pos, shape).unwrap() else {
            panic!("expected terrain");
        };
        assert_eq!(*decoded, chunk);
        assert_eq!(decoded.shape(), shape);
        assert!(!meta.relight);
        assert_eq!(decoded.get(3, 255, 3).block(), Block::Netherrack);
        assert_eq!(decoded.get(3, 0, 3).block(), Block::Bedrock);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_misplaced_newer_and_malformed_chunks() {
        let base = || {
            Compound::new()
                .with("DataVersion", DATA_VERSION)
                .with("xPos", 1)
                .with("zPos", 2)
                .with("Status", tuff_world_model::chunk_meta::STATUS_FULL)
        };
        let pos = ChunkPos::new(1, 2);
        let shape = ChunkShape::OVERWORLD;
        assert_eq!(
            decode(base(), ChunkPos::new(0, 0), shape).unwrap_err(),
            ChunkNbtError::WrongPosition(pos)
        );
        assert_eq!(
            decode(base().with("DataVersion", DATA_VERSION + 1), pos, shape).unwrap_err(),
            ChunkNbtError::NewerVersion(DATA_VERSION + 1)
        );
        let bad_data = base().with(
            "sections",
            vec![
                Compound::new().with("Y", 0i8).with(
                    "block_states",
                    Compound::new()
                        .with(
                            "palette",
                            vec![Tag::from("minecraft:stone"), Tag::from("minecraft:dirt")],
                        )
                        .with("data", vec![0i64; 3]),
                ),
            ],
        );
        assert!(matches!(
            decode(bad_data, pos, shape),
            Err(ChunkNbtError::Malformed(_))
        ));
        let unknown = base().with(
            "sections",
            vec![Compound::new().with("Y", 0i8).with(
                "block_states",
                Compound::new().with("palette", vec![Tag::from("modded:thing")]),
            )],
        );
        let (chunk, _) = decode_full(unknown, pos);
        assert_eq!(chunk.get(0, 0, 0), Block::Air.default_state());
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_legacy_palette_entries() {
        let legacy = Tag::Compound(
            Compound::new()
                .with("Name", "minecraft:oak_leaves")
                .with("Properties", Compound::new().with("distance", "3")),
        );
        assert_eq!(decode_block_state(&legacy).get("distance"), Some("3"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_saved_custom_block_identity_loads_and_saves_as_its_stand_in() {
        let look = Block::NoteBlock.default_state().with("note", "7").unwrap();
        let entry = Tag::Compound(
            Compound::new()
                .with("id", "minecraft:note_block")
                .with("properties", Compound::new().with("note", "7"))
                .with("tuff:block", "example:ore"),
        );
        assert_eq!(decode_block_state(&entry), look);
        let Tag::Compound(saved) = encode_block_state(look) else {
            panic!("a non-default state saves as a compound");
        };
        assert!(!saved.contains_key("tuff:block"));
    }
}
