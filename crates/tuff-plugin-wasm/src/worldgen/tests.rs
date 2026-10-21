use std::path::PathBuf;
use std::sync::Arc;

use tuff_core::config::{ConfigStore, Value as Setting};
use tuff_storage::backend::WorldBackend;
use tuff_storage::sqlite::SqliteBackend;

use super::*;
use crate::wit::bindings::types::{BiomeDecl, BlockPos as WitPos};
use crate::wit::canon::Value;
use crate::wit::testing::{self, Data, export};
use crate::wit::value::ToValue;

const SHAPE: ChunkShape = ChunkShape::new(0, 16);

fn decl() -> DimensionDecl {
    DimensionDecl {
        key: "mine:sky".into(),
        dimension_type: "minecraft:overworld".into(),
        type_json: String::new(),
        biomes: Vec::<BiomeDecl>::new(),
        natural_spawning: false,
        sea_level: 3,
        spawn: WitPos { x: 1, y: 2, z: 3 },
    }
}

fn layers(floor: usize) -> GeneratedChunk {
    let mut blocks = vec![1u8; 4096];
    blocks[..floor * 256].fill(0);
    GeneratedChunk {
        palette: vec!["minecraft:stone".into(), "minecraft:air".into()],
        blocks,
        biome: "minecraft:plains".into(),
    }
}

fn module(manifest: &str) -> Vec<u8> {
    let mut data = Data::new();
    let dims = data.export_result("declare-dimensions", &vec![decl()].to_value());
    let odd: Result<GeneratedChunk, String> = Ok(layers(1));
    let even: Result<GeneratedChunk, String> = Ok(layers(2));
    let odd = data.export_result("generate-chunk", &odd.to_value());
    let even = data.export_result("generate-chunk", &even.to_value());
    let log = data.call("log", &[Value::Variant(0, None), Value::String("x".into())]);
    let body = format!(
        "(func {declare} (i32.const {dims}))
         (func {generate}
           (if (i64.eq (local.get 2) (i64.const 99)) (then {log}))
           (if (i64.ne (i64.and (local.get 2) (i64.const 1)) (i64.const 0))
             (then (return (i32.const {odd}))))
           (i32.const {even}))
         {data}",
        declare = export("declare-dimensions"),
        generate = export("generate-chunk"),
        data = data.wat(),
    );
    let mut wasm = wat::parse_str(testing::module(&body)).unwrap();
    wasm.extend(manifest::custom_section(
        manifest::SECTION,
        manifest.as_bytes(),
    ));
    wasm
}

struct Setup {
    dir: PathBuf,
    _backend: SqliteBackend,
    ctx: ProviderContext,
}

fn setup(name: &str, directory: Option<&str>, grant: Option<&str>) -> Setup {
    setup_with(name, directory, grant, "")
}

fn setup_with(name: &str, directory: Option<&str>, grant: Option<&str>, extra: &str) -> Setup {
    let dir = std::env::temp_dir().join(format!("tuff-worldgen-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let plugins = dir.join(directory.unwrap_or("plugins"));
    std::fs::create_dir_all(&plugins).unwrap();
    let manifest = format!(
        "id = skygen\nversion = 1.0.0\napi = 0\ncap.dimension = mine:sky -- adds a sky\n{extra}"
    );
    std::fs::write(plugins.join("skygen.wasm"), module(&manifest)).unwrap();
    let backend = SqliteBackend::open(dir.join("server.db")).unwrap();
    let store = backend.config().expect("sqlite keeps the config");
    if let Some(grant) = grant {
        store.write_plugin_grant("skygen", Some(grant)).unwrap();
    }
    let settings = ConfigStore::new();
    for key in crate::config_keys() {
        settings.register(key).unwrap();
    }
    if directory.is_some() {
        let path = plugins.to_str().unwrap().to_owned();
        settings
            .set(crate::DIRECTORY_KEY, Setting::String(path))
            .unwrap();
    }
    let ctx = ProviderContext {
        config_store: store,
        settings,
        properties: Default::default(),
    };
    Setup {
        dir,
        _backend: backend,
        ctx,
    }
}

fn provider(setup: &Setup, directory: Option<PathBuf>) -> WorldgenProvider {
    let provider = WorldgenProvider::new(directory, Limits::default());
    provider.attach(&setup.ctx);
    provider
}

fn generator(provider: &WorldgenProvider, seed: i64) -> Arc<dyn ChunkGenerator> {
    provider.generator("mine:sky", seed, &Arc::new(Biomes::default()))
}

fn stone() -> BlockState {
    crate::world::parse_state("minecraft:stone").unwrap()
}

fn air() -> BlockState {
    tuff_vanilla_data::Block::Air.default_state()
}

#[tuff_core::watchdog]
#[test]
fn a_granted_dimension_generates_chunks_from_the_seed() {
    let s = setup("granted", None, Some("dimension = mine:sky"));
    let provider = provider(&s, Some(s.dir.join("plugins")));
    let dims = provider.dimensions();
    assert_eq!(dims.len(), 1);
    assert_eq!(dims[0].key, "mine:sky");
    assert!(!dims[0].claims_spawn);
    let odd = generator(&provider, 7);
    assert_eq!((odd.sea_level(), odd.spawn()), (3, BlockPos::new(1, 2, 3)));
    let chunk = odd.generate(ChunkPos { x: 4, z: -2 }, SHAPE);
    assert_eq!((chunk.get(0, 0, 0), chunk.get(0, 1, 0)), (stone(), air()));
    let even = generator(&provider, 8).generate(ChunkPos { x: 4, z: -2 }, SHAPE);
    assert_eq!((even.get(5, 1, 9), even.get(5, 2, 9)), (stone(), air()));
    let again = odd.generate(ChunkPos { x: 4, z: -2 }, SHAPE);
    assert!((0..16).all(|y| chunk.get(3, y, 3) == again.get(3, y, 3)));
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[tuff_core::watchdog]
#[test]
fn a_manifest_claims_the_spawn_only_for_its_own_granted_dimension() {
    let s = setup_with(
        "claim",
        None,
        Some("dimension = mine:sky"),
        "spawn_dimension = mine:sky\n",
    );
    let dims = provider(&s, Some(s.dir.join("plugins"))).dimensions();
    assert_eq!(dims.len(), 1);
    assert!(dims[0].claims_spawn);
    let _ = std::fs::remove_dir_all(&s.dir);

    let s = setup_with(
        "claim-other",
        None,
        Some("dimension = mine:sky"),
        "spawn_dimension = minecraft:the_end\n",
    );
    let dims = provider(&s, Some(s.dir.join("plugins"))).dimensions();
    assert!(!dims[0].claims_spawn);
    let _ = std::fs::remove_dir_all(&s.dir);

    let s = setup_with(
        "claim-ungranted",
        None,
        None,
        "spawn_dimension = mine:sky\n",
    );
    assert!(
        provider(&s, Some(s.dir.join("plugins")))
            .dimensions()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[tuff_core::watchdog]
#[test]
fn a_trap_in_the_generator_yields_an_empty_chunk_and_the_next_call_works() {
    let s = setup("trap", None, Some("dimension = mine:sky"));
    let provider = provider(&s, Some(s.dir.join("plugins")));
    let trapped = generator(&provider, 99).generate(ChunkPos { x: 0, z: 0 }, SHAPE);
    assert_eq!(trapped.get(0, 0, 0), air());
    let fine = generator(&provider, 1).generate(ChunkPos { x: 0, z: 0 }, SHAPE);
    assert_eq!(fine.get(0, 0, 0), stone());
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[tuff_core::watchdog]
#[test]
fn an_ungranted_dimension_is_not_registered() {
    let s = setup("ungranted", None, None);
    let provider = provider(&s, Some(s.dir.join("plugins")));
    assert!(provider.dimensions().is_empty());
    let other = setup("wrong-grant", None, Some("dimension = mine:other"));
    let provider = super::WorldgenProvider::new(Some(other.dir.join("plugins")), Limits::default());
    provider.attach(&other.ctx);
    assert!(provider.dimensions().is_empty());
    let _ = std::fs::remove_dir_all(&s.dir);
    let _ = std::fs::remove_dir_all(&other.dir);
}

#[tuff_core::watchdog]
#[test]
fn the_configured_plugin_directory_is_honored() {
    let s = setup(
        "custom-dir",
        Some("elsewhere"),
        Some("dimension = mine:sky"),
    );
    let provider = provider(&s, None);
    assert_eq!(provider.dimensions().len(), 1);
    let _ = std::fs::remove_dir_all(&s.dir);
}
