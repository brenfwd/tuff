//! A C++ plugin on the SDK's `Dimensions` and `ChunkBuilder` hooks,
//! compiled with the SDK's build script and run through the worldgen
//! pool. Skipped without a wasm32 clang and wasm-ld unless
//! `TUFF_EXAMPLE_PLUGINS=1` requires them.

use std::path::{Path, PathBuf};
use std::process::Command;

use tuff_plugin_wasm::sandbox::Limits;
use tuff_plugin_wasm::worldgen::Pool;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::ChunkPos;

fn build() -> Option<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out: PathBuf =
        std::env::temp_dir().join(format!("tuff-cpp-worldgen-{}.wasm", std::process::id()));
    let result = Command::new(root.join("../tuff-plugin-sdk/c/build.sh"))
        .arg(&out)
        .arg(root.join("tests/guests/cpp-worldgen/worldgen.cpp"))
        .output();
    match result {
        Ok(done) if done.status.success() => {
            let wasm = std::fs::read(&out).ok();
            let _ = std::fs::remove_file(&out);
            wasm
        }
        other => {
            let why = match other {
                Ok(done) => String::from_utf8_lossy(&done.stderr).into_owned(),
                Err(err) => err.to_string(),
            };
            if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") {
                panic!("cpp-worldgen did not build:\n{why}");
            }
            eprintln!("skipping: cpp-worldgen did not build:\n{why}");
            None
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn cpp_plugin_declares_dimensions_and_generates_chunks() {
    let Some(wasm) = build() else { return };
    let pool = Pool::new(wasm, Limits::default());

    let decls = pool.declare().unwrap();
    assert_eq!(decls.len(), 1);
    let sky = &decls[0];
    assert_eq!(sky.key, "example:sky");
    assert_eq!(sky.dimension_type, "minecraft:overworld");
    assert_eq!(sky.sea_level, 5);
    assert!(!sky.natural_spawning);
    assert_eq!((sky.spawn.x, sky.spawn.y, sky.spawn.z), (1, 70, 2));
    assert_eq!(sky.biomes.len(), 1);
    assert_eq!(sky.biomes[0].key, "example:cloud");

    let shape = ChunkShape::new(0, 16);
    let odd = pool
        .generate("example:sky", 1, ChunkPos::new(0, 0), shape)
        .unwrap();
    assert_eq!(odd.biome, "example:cloud");
    assert_eq!(odd.blocks.len(), 16 * 256);
    let name = |index: usize| odd.palette[odd.blocks[index] as usize].as_str();
    assert_eq!(name(0), "minecraft:bedrock");
    assert_eq!(name(256 + 4 * 16 + 3), "minecraft:stone");
    assert_eq!(name(512), "minecraft:dirt");
    assert_eq!(name(3000), "minecraft:air");

    let even = pool
        .generate("example:sky", 2, ChunkPos::new(0, 0), shape)
        .unwrap();
    assert_eq!(even.palette[even.blocks[512] as usize], "minecraft:sand");

    let refused = pool.generate("example:sky", 1, ChunkPos::new(99, 0), shape);
    assert_eq!(refused.unwrap_err(), "refused");
}
