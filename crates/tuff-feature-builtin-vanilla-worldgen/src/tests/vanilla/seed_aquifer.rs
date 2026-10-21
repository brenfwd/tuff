//! Aquifer output for a seed and spot where a water-filled blob once
//! appeared inside a vanilla cave: the fluid level search skipped surface
//! samples over air, so a cell with no submerged surface took its level
//! from the spread noise alone. Regenerate with
//! `scripts/worldgen-aquifer-fixture.sh`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tuff_core::storage::chunk_nbt;
use tuff_nbt::{Compound, Compression, NamedTag, NbtLimits, Tag};
use tuff_vanilla_data::worldgen::NoiseSettings;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::chunk_meta::Decoded;
use tuff_world_model::pos::ChunkPos;

use super::fixtures::{KEPT_KEYS, read_root};
use super::*;
use crate::biome_source::MultiNoise;
use crate::generator::{NoiseGenerator, Stages};

const SEED: i64 = -5713689679960382707;
const CHUNK: [i32; 2] = [-43, 169];

fn fixture_file() -> PathBuf {
    Path::new(FIXTURE_DIR)
        .parent()
        .unwrap()
        .join("seed_aquifer.nbt")
}

#[tuff_core::watchdog]
#[test]
#[ignore = "run by scripts/worldgen-aquifer-fixture.sh"]
fn trim_seed_aquifer_fixture() {
    let region = std::env::var("MC_AQUIFER_REGION_DIR").expect("MC_AQUIFER_REGION_DIR");
    let pos = ChunkPos::new(CHUNK[0], CHUNK[1]);
    let root = read_root(Path::new(&region), pos).expect("vanilla did not save the chunk");
    let mut kept = Compound::new();
    for key in KEPT_KEYS {
        if let Some(value) = root.get(key) {
            kept.insert(key, value.clone());
        }
    }
    let mut bytes = Vec::new();
    NamedTag::new("", kept).write(&mut bytes).unwrap();
    std::fs::write(fixture_file(), Compression::Gzip.compress(bytes).unwrap()).unwrap();
}

#[tuff_core::watchdog]
#[test]
fn aquifer_matches_vanilla_for_seed_with_dry_cave_at_high_altitude() {
    let pos = ChunkPos::new(CHUNK[0], CHUNK[1]);
    let data = std::fs::read(fixture_file()).expect("seed aquifer fixture");
    let raw = Compression::Gzip.decompress(&data, 1 << 26).unwrap();
    let Tag::Compound(root) = NamedTag::read(&mut &raw[..], NbtLimits::TRUSTED)
        .unwrap()
        .tag
    else {
        panic!("fixture root is not a compound");
    };
    let Decoded::Chunk(theirs, _) = chunk_nbt::decode(root, pos, ChunkShape::OVERWORLD).unwrap()
    else {
        panic!("fixture holds no terrain");
    };
    let generator = NoiseGenerator::new(
        NoiseSettings::get("minecraft:overworld").unwrap(),
        Arc::new(MultiNoise::preset("minecraft:overworld").unwrap()),
        SEED,
    );
    let stages = Stages {
        surface: false,
        carvers: false,
    };
    let ours = generator.generate(pos, ChunkShape::OVERWORLD, stages);
    let (blocks, _, lines) = diff(&ours, &theirs, 8);
    assert_eq!(
        blocks,
        0,
        "blocks differ from vanilla:\n  {}",
        lines.join("\n  ")
    );
}
