use std::collections::HashMap;

use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::light::*;
use tuff_world_model::pos::ChunkPos;

use crate::storage::chunk_nbt;
use crate::storage::level::OVERWORLD;
use tuff_nbt::{Compound, NamedTag, NbtLimits, Tag};
use tuff_storage::region_file::{RawChunk, RegionFile};
use tuff_storage::test_dir::TestDir;
use tuff_storage::vanilla::WorldPaths;
use tuff_world_model::chunk_meta::Decoded;

#[derive(Default)]
struct World {
    chunks: HashMap<ChunkPos, (Chunk, SkyLightSources)>,
}

impl ChunkSource for World {
    fn chunk(&self, pos: ChunkPos) -> Option<LightChunk<'_>> {
        self.chunks
            .get(&pos)
            .map(|(blocks, sky_sources)| LightChunk {
                blocks,
                sky_sources,
            })
    }
}

impl World {
    fn lit(chunks: impl IntoIterator<Item = (ChunkPos, Chunk)>) -> (Self, LevelLight) {
        let mut world = Self::default();
        let mut positions = Vec::new();
        for (pos, chunk) in chunks {
            let sources = SkyLightSources::from_chunk(&chunk);
            world.chunks.insert(pos, (chunk, sources));
            positions.push(pos);
        }
        let mut light = LevelLight::new(ChunkShape::OVERWORLD, true);
        for &pos in &positions {
            light.initialize_light(&world, pos, false);
        }
        for &pos in &positions {
            light.light_chunk(&world, pos, false);
        }
        (world, light)
    }
}

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/vanilla-26.3/light.r.0.0.mca"
);
/// The generated area, all in region 0,0.
const AREA: std::ops::RangeInclusive<i32> = 3..=9;
/// Two chunks inside the area, so every input to their light is known.
const CHECKED: std::ops::RangeInclusive<i32> = 5..=7;
const KEPT_KEYS: [&str; 7] = [
    "DataVersion",
    "xPos",
    "yPos",
    "zPos",
    "Status",
    "isLightOn",
    "sections",
];

fn read_root(region: &mut RegionFile, pos: ChunkPos) -> Compound {
    let raw = region.read(pos).unwrap().expect("chunk in region");
    let bytes = raw.decompress(1 << 24).unwrap();
    match NamedTag::read(&mut &bytes[..], NbtLimits::TRUSTED)
        .unwrap()
        .tag
    {
        Tag::Compound(root) => root,
        _ => panic!("chunk root is not a compound"),
    }
}

fn area() -> impl Iterator<Item = ChunkPos> {
    AREA.flat_map(|z| AREA.map(move |x| ChunkPos::new(x, z)))
}

#[tuff_core::watchdog]
#[test]
#[ignore = "run by scripts/light-fixture.sh"]
fn trim_light_fixture() {
    let world = std::env::var("MC_LIGHT_FIXTURE_WORLD").expect("MC_LIGHT_FIXTURE_WORLD");
    let source_dir = WorldPaths::new(world).region_dir(OVERWORLD);
    let mut source = RegionFile::open(&source_dir, 0, 0, false)
        .unwrap()
        .expect("vanilla region 0,0");
    let out = TestDir::new();
    let mut trimmed = RegionFile::open(out.path(), 0, 0, true).unwrap().unwrap();
    for pos in area() {
        let root = read_root(&mut source, pos);
        assert_eq!(
            root.get_str("Status"),
            Some(tuff_world_model::chunk_meta::STATUS_FULL),
            "{pos:?}"
        );
        assert_eq!(root.get_bool("isLightOn"), Some(true), "{pos:?}");
        let mut kept = Compound::new();
        for key in KEPT_KEYS {
            kept.insert(key, root.get(key).unwrap().clone());
        }
        let mut bytes = Vec::new();
        NamedTag::new("", kept).write(&mut bytes).unwrap();
        trimmed
            .write(pos, &RawChunk::compress(bytes).unwrap())
            .unwrap();
    }
    trimmed.sync().unwrap();
    drop(trimmed);
    std::fs::copy(out.path().join("r.0.0.mca"), FIXTURE).unwrap();
}

fn stored(light: Option<&ChunkLight>, layer: LightLayer, index: usize) -> Option<&[u8]> {
    let layers = match layer {
        LightLayer::Sky => &light?.sky,
        LightLayer::Block => &light?.block,
    };
    match &layers[index] {
        SectionLight::Data(data) => Some(&data[..]),
        _ => None,
    }
}

#[tuff_core::watchdog]
#[test]
fn relighting_vanilla_terrain_reproduces_vanilla_light() {
    let dir = TestDir::new();
    std::fs::copy(FIXTURE, dir.path().join("r.0.0.mca")).unwrap();
    let mut region = RegionFile::open(dir.path(), 0, 0, false).unwrap().unwrap();
    let mut chunks = Vec::new();
    let mut expected = HashMap::new();
    for pos in area() {
        let Decoded::Chunk(chunk, _) =
            chunk_nbt::decode(read_root(&mut region, pos), pos, ChunkShape::OVERWORLD).unwrap()
        else {
            panic!("{pos:?} has no terrain");
        };
        expected.insert(pos, chunk.light().cloned());
        chunks.push((pos, *chunk));
    }
    let (_, light) = World::lit(chunks);

    let mut mismatches = Vec::new();
    let mut varied = HashMap::new();
    for z in CHECKED {
        for x in CHECKED {
            let pos = ChunkPos::new(x, z);
            let ours = light.chunk_light(pos);
            for (layer, sections) in [
                (LightLayer::Sky, &ours.sky),
                (LightLayer::Block, &ours.block),
            ] {
                for (index, section) in sections
                    .iter()
                    .enumerate()
                    .take(ChunkShape::OVERWORLD.light_sections())
                {
                    let ours = match section {
                        SectionLight::Data(data) => Some(&data[..]),
                        SectionLight::Empty | SectionLight::Missing => None,
                    };
                    let theirs = stored(expected[&pos].as_ref(), layer, index);
                    if theirs.is_some_and(|data| data.iter().any(|b| *b != 0 && *b != 0xFF)) {
                        *varied.entry(layer as u8).or_insert(0) += 1;
                    }
                    if ours != theirs {
                        mismatches.push((pos, layer, index));
                    }
                }
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "sections differing from vanilla: {mismatches:?}"
    );
    assert!(
        varied
            .get(&(LightLayer::Sky as u8))
            .is_some_and(|n| *n >= 9)
            && varied.contains_key(&(LightLayer::Block as u8)),
        "fixture has too little varied light to be meaningful: {varied:?}"
    );
}
