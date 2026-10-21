//! The tuffer example plugin loaded as WebAssembly: its dimension in the
//! registries and commands, deterministic floating islands and pale oak
//! trees, glowstone portals lit with a water bucket, travel at 1:128 and the
//! dimension on the world map.

use super::*;
use crate::auth::GameProfile;
use crate::config::ConfigStore;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::Serverbound;
use crate::world::custom_dimension::{DimensionProvider, ProviderContext};
use crate::world::messages::{PlayerHandle, RegionRequest};
use crate::world::session::PlayerSession;
use crate::world::session::hooks::{PacketInterceptor, Verdict};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_plugin_wasm::worldgen::WorldgenProvider;
use tuff_plugin_wasm::{Limits, WasmHost, WasmPluginsFeature, bridge};
use tuff_storage::admin::PermissionLevel;
use tuff_storage::sqlite::SqliteBackend;
use tuff_vanilla_data::data_registry::{DIMENSION_TYPE, WORLDGEN_BIOME};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::world::BedCondition;
use tuff_vanilla_data::{Block, Item};
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::ChunkShape;
use tuff_world_model::pos::{ChunkPos, RegionPos};
use tuff_world_model::{BlockPos, Face};

const TUFFER: &str = "tuffer:tuffer";
const BUILD: &str = "examples/rust-tuffer/target/wasm32-unknown-unknown/release/tuffer.wasm";
const GRANT: &str = "dimension = tuffer:tuffer\nworld-write = all\nplayer-write = all";
const SHAPE: ChunkShape = ChunkShape::new(0, 256);

fn built() -> Vec<u8> {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES
        .get_or_init(|| {
            let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            assert!(
                super::worldedit_wasm::build_example(&root.join("examples/rust-tuffer")),
                "building tuffer.wasm failed (is the wasm32-unknown-unknown target installed?)"
            );
            let path = root.join(BUILD);
            std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        })
        .clone()
}

struct Setup {
    dir: TestDir,
    backend: Arc<SqliteBackend>,
}

fn setup(grant: bool) -> Setup {
    let dir = TestDir::new();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(plugins.join("tuffer.wasm"), built()).unwrap();
    let backend = Arc::new(SqliteBackend::open(dir.path().join("world.mcserver")).unwrap());
    if grant {
        backend
            .config()
            .expect("sqlite keeps the config")
            .write_plugin_grant("tuffer", Some(GRANT))
            .unwrap();
    }
    Setup { dir, backend }
}

async fn start_with(setup: &Setup, extra: Vec<Box<dyn crate::feature::Feature>>) -> TestWorld {
    let mut plugins: Vec<Box<dyn crate::feature::Feature>> = vec![Box::new(
        WasmPluginsFeature::at(setup.dir.path().join("plugins")),
    )];
    plugins.extend(extra);
    let world = TestWorld::start_with_features_on(setup.backend.clone(), plugins);
    world
        .shared
        .services
        .get::<WasmHost>()
        .unwrap()
        .sync()
        .await;
    world
}

async fn start() -> (TestWorld, Setup) {
    let setup = setup(true);
    let world = start_with(&setup, Vec::new()).await;
    (world, setup)
}

fn compound(nbt: &[u8]) -> Compound {
    let mut input = nbt;
    Tag::read_network(&mut input, tuff_nbt::NbtLimits::NETWORK)
        .unwrap()
        .and_then(|tag| tag.as_compound().cloned())
        .expect("a compound")
}

fn generator(
    setup: &Setup,
    biomes: &Arc<Biomes>,
    seed: i64,
) -> Arc<dyn crate::world::generator::ChunkGenerator> {
    let settings = ConfigStore::new();
    let provider = WorldgenProvider::new(Some(setup.dir.path().join("plugins")), Limits::default());
    provider.attach(&ProviderContext {
        config_store: setup.backend.config().unwrap(),
        settings,
        properties: Default::default(),
    });
    provider.generator(TUFFER, seed, biomes)
}

fn named(state: tuff_vanilla_data::BlockState, name: &str) -> bool {
    state.to_string().starts_with(name)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dimension_is_registered_with_its_type_biome_and_rules() {
    let (world, _setup) = start().await;
    let dimensions = &world.shared.dimensions;
    assert!(dimensions.keys().contains(&TUFFER));
    let tuffer = dimensions.by_key(TUFFER).unwrap();
    assert_eq!(tuffer.type_id(), DIMENSION_TYPE.entries.len() as i32);
    assert_eq!(tuffer.shape, SHAPE);
    assert!(!tuffer.natural_spawning && !tuffer.special_spawners());
    let environment = tuffer.environment();
    assert!(environment.has_fixed_time);
    assert!(!environment.water_evaporates);
    assert_eq!(environment.bed_rule.can_sleep, BedCondition::Never);
    assert_eq!(environment.bed_rule.can_set_spawn, BedCondition::Never);
    assert!(!environment.bed_rule.destroy_on_use);
    assert!(
        dimensions.by_key(OVERWORLD).unwrap().natural_spawning,
        "the overworld still spawns mobs"
    );

    let custom = dimensions.custom();
    let types = custom.registry_entries(DIMENSION_TYPE.key);
    assert_eq!(types.len(), 1);
    assert_eq!(types[0].0, "tuffer:tuffer_type");
    let data = compound(types[0].1);
    assert_eq!(data.get_bool("has_fixed_time"), Some(true));
    assert_eq!(data.get_str("skybox"), Some("none"));
    let attributes = data.get_compound("attributes").unwrap();
    assert!(attributes.get_str("minecraft:visual/sky_color").is_some());
    let biomes = custom.registry_entries(WORLDGEN_BIOME.key);
    assert_eq!(biomes.len(), 1);
    assert_eq!(biomes[0].0, "tuffer:islands");
    assert_eq!(
        dimensions.biomes().id("tuffer:islands"),
        Some(WORLDGEN_BIOME.entries.len() as u16)
    );

    let region = world.region_in(TUFFER, RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    world.ticks(3).await;
    let said = crate::world::commands::console::run(
        world.shared.clone(),
        "execute in tuffer:tuffer run setblock 0 200 0 minecraft:gold_block",
    )
    .await;
    assert_eq!(said[0].to_value()["translate"], "commands.setblock.success");
    let gold = block_at(&region, BlockPos::new(0, 200, 0)).await.unwrap();
    assert!(named(gold, "minecraft:gold_block"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_grant_the_dimension_is_not_registered() {
    let setup = setup(false);
    let world = start_with(&setup, Vec::new()).await;
    assert!(world.shared.dimensions.by_key(TUFFER).is_none());
    assert_eq!(world.shared.dimensions.keys().len(), 3);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn islands_and_pale_oak_trees_are_deterministic_and_float() {
    let (world, setup) = start().await;
    let biomes = world.shared.dimensions.biomes().clone();
    let make = generator(&setup, &biomes, 1);
    assert_eq!(make.sea_level(), 0);
    let spawn = make.spawn();
    assert_eq!((spawn.x, spawn.z), (0, 0));

    let origin = make.generate(ChunkPos { x: 0, z: 0 }, SHAPE);
    assert!(
        named(origin.get(0, spawn.y - 2, 0), "minecraft:grass_block")
            || named(origin.get(0, spawn.y - 1, 0), "minecraft:grass_block")
    );
    assert!(named(origin.get(0, spawn.y, 0), "minecraft:air"));
    assert!(
        named(origin.get(0, 10, 0), "minecraft:air"),
        "nothing below"
    );
    assert!(
        named(origin.get(0, 200, 0), "minecraft:air"),
        "nothing above"
    );

    let again = generator(&setup, &biomes, 1).generate(ChunkPos { x: 0, z: 0 }, SHAPE);
    let other = generator(&setup, &biomes, 2);
    let (mut logs, mut leaves, mut differs, mut voids) = (0, 0, false, 0);
    for cx in -6..6 {
        for cz in -6..6 {
            let pos = ChunkPos { x: cx, z: cz };
            let chunk = make.generate(pos, SHAPE);
            let rerun = make.generate(pos, SHAPE);
            let foreign = other.generate(pos, SHAPE);
            for y in 90..180 {
                for (x, z) in (0..16).flat_map(|x| (0..16).map(move |z| (x, z))) {
                    let state = chunk.get(x, y, z);
                    assert_eq!(state, rerun.get(x, y, z));
                    differs |= state != foreign.get(x, y, z);
                    if named(state, "minecraft:pale_oak_log") {
                        logs += 1;
                        let below = chunk.get(x, y - 1, z);
                        assert!(
                            named(below, "minecraft:pale_oak_log")
                                || named(below, "minecraft:grass_block"),
                            "a trunk stands on grass: {below}"
                        );
                    }
                    leaves += i32::from(named(state, "minecraft:pale_oak_leaves"));
                }
            }
            voids += i32::from((60..200).all(|y| named(chunk.get(3, y, 3), "minecraft:air")));
        }
    }
    assert!(logs > 0 && leaves > 0, "{logs} logs, {leaves} leaves");
    assert!(voids > 10, "islands float with gaps between them");
    assert!(differs, "the seed changes the islands");
    assert!((0..256).all(|y| origin.get(5, y, 5) == again.get(5, y, 5)));
    world.shutdown().await;
}

fn session(world: &TestWorld) -> PlayerSession {
    let (outbound, _frames) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(7);
    let (_packets, packets) = mpsc::channel(8);
    PlayerSession::new(
        world.shared.clone(),
        7,
        GameProfile {
            id: 77,
            name: "Steve".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        PermissionLevel::All,
    )
}

async fn run(world: &TestWorld, line: &str) {
    crate::world::commands::console::run(world.shared.clone(), line).await;
}

async fn put(
    world: &TestWorld,
    dimension: &str,
    blocks: Vec<([i32; 3], tuff_vanilla_data::BlockState)>,
) {
    let mut by_region: std::collections::HashMap<RegionPos, Vec<_>> = Default::default();
    for (p, state) in blocks {
        let pos = BlockPos::new(p[0], p[1], p[2]);
        by_region
            .entry(ChunkPos::of_block(pos).region())
            .or_default()
            .push((pos, state));
    }
    for (region, changes) in by_region {
        let handle = world.region_in(dimension, region).await;
        let (reply, done) = oneshot::channel();
        handle
            .request(RegionRequest::Blocks {
                op: crate::world::messages::BlockOp::Set {
                    changes,
                    destroy: false,
                    keep: false,
                    strict: false,
                    quiet: false,
                    data: None,
                },
                reply,
            })
            .await;
        done.await.unwrap();
    }
}

/// A glowstone frame around a `w` by `h` opening whose lowest corner is
/// `(x, y, z)`, along X.
async fn frame(world: &TestWorld, dimension: &str, at: [i32; 3], w: i32, h: i32) {
    let [x, y, z] = at;
    let mut blocks = Vec::new();
    for i in -1..=w {
        for j in -1..=h {
            let edge = i == -1 || i == w || j == -1 || j == h;
            let block = if edge { Block::Glowstone } else { Block::Air };
            blocks.push(([x + i, y + j, z], block.default_state()));
        }
    }
    put(world, dimension, blocks).await;
}

async fn light(world: &TestWorld, client: &mut PlayerSession, at: [i32; 3]) -> Verdict {
    let slot = crate::world::inventory::Inventory::hotbar_slot(client.inventory.selected());
    *client.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::WaterBucket, 1, MintReason::Test));
    let verdict = bridge::Clicks
        .intercept(
            client,
            &Serverbound::UseItemOn {
                hand: 0,
                position: BlockPos::new(at[0], at[1], at[2]),
                face: Face::South,
                cursor: [0.5; 3],
                sequence: 1,
            },
        )
        .await;
    world.ticks(6).await;
    verdict
}

async fn state_at(
    world: &TestWorld,
    dimension: &str,
    p: [i32; 3],
) -> tuff_vanilla_data::BlockState {
    let pos = BlockPos::new(p[0], p[1], p[2]);
    let region = world
        .region_in(dimension, ChunkPos::of_block(pos).region())
        .await;
    block_at(&region, pos).await.expect("inside the world")
}

async fn items(world: &TestWorld, dimension: &str, region: RegionPos) -> Vec<[f64; 3]> {
    let region = world.region_in(dimension, region).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntityType::Item)
        .map(|e| e.position)
        .collect()
}

async fn wait_items(world: &TestWorld, dimension: &str, region: RegionPos, count: usize) {
    for _ in 0..40 {
        world.ticks(12).await;
        if items(world, dimension, region).await.len() >= count {
            return;
        }
    }
}

async fn drop_item(world: &TestWorld, dimension: &str, at: [f64; 3]) {
    let chunk = ChunkPos::of_position(at);
    let region = world.region_in(dimension, chunk.region()).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
}

async fn gateway_in_column(world: &TestWorld, dimension: &str, x: i32, z: i32) -> Option<i32> {
    for y in (0..250).rev() {
        if named(
            state_at(world, dimension, [x, y, z]).await,
            "minecraft:end_gateway",
        ) {
            return Some(y);
        }
    }
    None
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_water_bucket_lights_only_a_complete_glowstone_frame() {
    let (world, _setup) = start().await;
    let mut steve = session(&world);

    frame(&world, OVERWORLD, [5, 80, 5], 2, 3).await;
    put(
        &world,
        OVERWORLD,
        vec![([6, 83, 5], Block::Stone.default_state())],
    )
    .await;
    let verdict = light(&world, &mut steve, [4, 81, 5]).await;
    assert_eq!(verdict, Verdict::Cancel);
    for y in 80..83 {
        let inside = state_at(&world, OVERWORLD, [5, y, 5]).await;
        assert!(
            named(inside, "minecraft:air"),
            "the top is broken: {inside}"
        );
    }

    put(
        &world,
        OVERWORLD,
        vec![([6, 83, 5], Block::Glowstone.default_state())],
    )
    .await;
    light(&world, &mut steve, [4, 81, 5]).await;
    for (x, y) in [(5, 80), (6, 82), (5, 82), (6, 80)] {
        let inside = state_at(&world, OVERWORLD, [x, y, 5]).await;
        assert!(named(inside, "minecraft:end_gateway"), "{x} {y}: {inside}");
    }
    assert!(named(
        state_at(&world, OVERWORLD, [4, 81, 5]).await,
        "minecraft:glowstone"
    ));

    frame(&world, OVERWORLD, [20, 80, 20], 2, 2).await;
    light(&world, &mut steve, [19, 80, 20]).await;
    assert!(
        named(
            state_at(&world, OVERWORLD, [20, 80, 20]).await,
            "minecraft:air"
        ),
        "two by two is too small"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn portals_travel_at_one_to_one_twenty_eight_and_find_or_make_their_match() {
    let (world, _setup) = start().await;
    let mut steve = session(&world);

    frame(&world, OVERWORLD, [1288, 70, -250], 2, 3).await;
    light(&world, &mut steve, [1287, 71, -250]).await;
    assert!(named(
        state_at(&world, OVERWORLD, [1288, 70, -250]).await,
        "minecraft:end_gateway"
    ));

    drop_item(&world, OVERWORLD, [1288.5, 70.5, -249.5]).await;
    wait_items(&world, TUFFER, RegionPos::new(0, -1), 1).await;
    assert!(
        items(&world, OVERWORLD, RegionPos::new(2, -1))
            .await
            .is_empty(),
        "the item left the overworld"
    );
    let arrived = items(&world, TUFFER, RegionPos::new(0, -1)).await;
    assert_eq!(arrived.len(), 1, "{arrived:?}");
    let [x, y, z] = arrived[0];
    assert!((9.0..=12.0).contains(&x), "x {x}");
    assert!((-3.0..=-1.0).contains(&z), "z {z}");
    let gate_y = gateway_in_column(&world, TUFFER, 10, -2)
        .await
        .expect("a portal was built in the tuffer");
    assert!((gate_y as f64 - y).abs() <= 3.0, "{gate_y} {y}");

    drop_item(&world, TUFFER, [x, y + 0.1, z]).await;
    wait_items(&world, OVERWORLD, RegionPos::new(2, -1), 1).await;
    let back = items(&world, OVERWORLD, RegionPos::new(2, -1)).await;
    assert_eq!(back.len(), 1, "{back:?}");
    assert!(
        (back[0][0] - 1289.0).abs() <= 2.0 && (back[0][2] + 249.5).abs() <= 2.0,
        "back through the existing portal: {:?}",
        back[0]
    );
    assert!(
        items(&world, TUFFER, RegionPos::new(0, -1)).await.len() <= 1,
        "only the first item is still in the tuffer"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_escape_portal_built_in_the_tuffer_leads_to_the_scaled_overworld_spot() {
    let (world, _setup) = start().await;
    let mut steve = session(&world);

    steve.dimension = world.dimension(TUFFER);
    frame(&world, TUFFER, [-9, 129, 3], 2, 3).await;
    light(&world, &mut steve, [-10, 130, 3]).await;
    assert!(named(
        state_at(&world, TUFFER, [-9, 129, 3]).await,
        "minecraft:end_gateway"
    ));
    drop_item(&world, TUFFER, [-8.5, 129.5, 3.5]).await;
    world.ticks(12).await;
    let arrived = items(&world, OVERWORLD, RegionPos::new(-3, 0)).await;
    assert_eq!(arrived.len(), 1, "{arrived:?}");
    assert!((arrived[0][0] + 1088.0).abs() <= 8.0, "{:?}", arrived[0]);
    assert!((arrived[0][2] - 448.0).abs() <= 8.0, "{:?}", arrived[0]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_world_map_lists_and_draws_the_dimension() {
    let setup = setup(true);
    let world = start_with(
        &setup,
        vec![Box::new(tuff_feature_builtin_world_map::WorldMapFeature)],
    )
    .await;
    let index = world
        .shared
        .dimensions
        .iter()
        .position(|d| d.key == TUFFER)
        .expect("the map's dimension list has the tuffer");
    run(&world, "execute in tuffer:tuffer run forceload add 0 0").await;
    world.ticks(10).await;
    let map = world
        .shared
        .services
        .get::<tuff_feature_builtin_world_map::service::MapService>()
        .unwrap();
    let handle = map.handle().expect("the map runs");
    let key = tuff_feature_builtin_world_map::tile::TileKey::new(index as u16, 0, 0, 0);
    handle.tile(key, "test", None).await;
    handle.idle().await;
    let pixels = handle.pixels(key).await.expect("the tile exists");
    assert!(
        pixels.chunks(4).any(|p| p[3] != 0),
        "the origin island is drawn"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_end_gateways_keep_ticking_with_the_feature_loaded() {
    let (world, _setup) = start().await;
    let pos = BlockPos::new(40, 80, 40);
    let region = world
        .region_in(
            crate::storage::level::THE_END,
            ChunkPos::of_block(pos).region(),
        )
        .await;
    let (reply, done) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Set {
                changes: vec![(pos, Block::EndGateway.default_state())],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: Some(Compound::new().with("id", "minecraft:end_gateway")),
            },
            reply,
        })
        .await;
    done.await.unwrap();
    world.ticks(60).await;
    let (reply, nbt) = oneshot::channel();
    region
        .request(RegionRequest::BlockWithEntity { pos, reply })
        .await;
    let (_, nbt) = nbt.await.unwrap().expect("loaded");
    let age = nbt.expect("block entity").get("Age").and_then(|t| match t {
        Tag::Long(v) => Some(*v),
        Tag::Int(v) => Some(i64::from(*v)),
        _ => None,
    });
    assert!(age.is_some_and(|a| a > 0), "age {age:?}");
    world.shutdown().await;
}
