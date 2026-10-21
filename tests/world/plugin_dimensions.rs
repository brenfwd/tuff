//! Dimensions a plugin adds at startup: registry data sent to clients,
//! commands and storage reaching them, their environment (no sleeping,
//! water that stays) and the natural spawning gate.

use super::*;
use crate::feature::{Feature, LoadContext};
use crate::storage::codec::WorldBackendExt as _;
use crate::world::custom_dimension::{BiomeDefinition, DimensionDefinition, DimensionProvider};
use crate::world::generator::ChunkGenerator;
use crate::world::messages::{BedUse, HandEffect};
use tuff_item::load::{LoadError, Phase};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::data_registry::{DIMENSION_TYPE, WORLDGEN_BIOME};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::world::BedCondition;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::Chunk;

const SKY: &str = "test:sky";
const MEADOW: &str = "test:meadow";
const BARREN: &str = "test:barren";
const SKY_BIOME: &str = "test:sky_meadow";
const GROUND: i32 = 63;

const SKY_TYPE: &str = r##"{
    "ambient_light": 0.5,
    "attributes": {
        "minecraft:gameplay/bed_rule": {
            "can_set_spawn": "never",
            "can_sleep": "never",
            "error_message": {"translate": "test.sky.no_sleep"}
        },
        "minecraft:gameplay/respawn_anchor_works": false,
        "minecraft:visual/ambient_light_color": "#3f5f7f",
        "minecraft:visual/fog_color": "#a0c0e0",
        "minecraft:visual/sky_color": "#80a0c0",
        "minecraft:visual/ambient_particles": [
            {"particle": {"type": "minecraft:white_ash"}, "probability": 0.01}
        ]
    },
    "coordinate_scale": 128.0,
    "has_ceiling": false,
    "has_ender_dragon_fight": false,
    "has_fixed_time": true,
    "has_skylight": true,
    "height": 256,
    "infiniburn": "#minecraft:infiniburn_overworld",
    "logical_height": 256,
    "min_y": 0,
    "monster_spawn_block_light_limit": 0,
    "monster_spawn_light_level": 0,
    "skybox": "none"
}"##;

const SKY_BIOME_JSON: &str = r##"{
    "has_precipitation": false,
    "temperature": 0.5,
    "downfall": 0.0,
    "effects": {"water_color": "#3d57d6"},
    "carvers": [],
    "features": []
}"##;

/// Flat grass at [`GROUND`] over stone, in one biome.
struct Meadow {
    biome: u16,
}

impl ChunkGenerator for Meadow {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let mut chunk = placeholder::Layered {
            layers: vec![
                (GROUND - 1, Block::Stone.default_state()),
                (GROUND, Block::GrassBlock.default_state()),
            ],
            biome: "minecraft:plains",
            sea_level: 0,
        }
        .generate(pos, shape);
        chunk.fill_biomes(|_, _, _, _| Some(self.biome));
        chunk
    }

    fn sea_level(&self) -> i32 {
        0
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, GROUND + 1, 0)
    }
}

/// `test:sky` (custom type and biome, no natural spawning), and
/// `test:meadow` and `test:barren` (vanilla overworld type and plains, with
/// and without natural spawning).
struct Provider;

impl DimensionProvider for Provider {
    fn dimensions(&self) -> Vec<DimensionDefinition> {
        vec![
            DimensionDefinition {
                key: SKY.into(),
                dimension_type: "test:sky_type".into(),
                type_json: SKY_TYPE.into(),
                biomes: vec![BiomeDefinition {
                    key: SKY_BIOME.into(),
                    json: SKY_BIOME_JSON.into(),
                }],
                natural_spawning: false,
                claims_spawn: false,
            },
            DimensionDefinition {
                key: MEADOW.into(),
                dimension_type: OVERWORLD.into(),
                type_json: String::new(),
                biomes: Vec::new(),
                natural_spawning: true,
                claims_spawn: false,
            },
            DimensionDefinition {
                key: BARREN.into(),
                dimension_type: OVERWORLD.into(),
                type_json: String::new(),
                biomes: Vec::new(),
                natural_spawning: false,
                claims_spawn: false,
            },
            DimensionDefinition {
                key: THE_NETHER.into(),
                dimension_type: THE_NETHER.into(),
                type_json: String::new(),
                biomes: Vec::new(),
                natural_spawning: true,
                claims_spawn: false,
            },
        ]
    }

    fn generator(
        &self,
        dimension: &str,
        _seed: i64,
        biomes: &Arc<Biomes>,
    ) -> Arc<dyn ChunkGenerator> {
        let biome = if dimension == SKY {
            SKY_BIOME
        } else {
            "minecraft:plains"
        };
        Arc::new(Meadow {
            biome: biomes.id(biome).expect("a known biome"),
        })
    }
}

struct Dimensions;

impl Feature for Dimensions {
    fn id(&self) -> &'static str {
        "test-dimensions"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().dimension_providers.push(Arc::new(Provider));
        }
        Ok(())
    }
}

fn start(backend: Arc<dyn WorldBackend>) -> TestWorld {
    TestWorld::start_with_features_on(backend, vec![Box::new(Dimensions)])
}

fn memory() -> Arc<dyn WorldBackend> {
    Arc::new(tuff_storage::memory::MemoryBackend::default())
}

async fn outcome(events: &mut mpsc::UnboundedReceiver<PlayerEvent>, seq: i32) -> ActionOutcome {
    expect(events, |event| match event {
        PlayerEvent::ActionResult { sequence, outcome } if sequence == seq => Some(outcome),
        _ => None,
    })
    .await
}

async fn snapshots(region: &RegionHandle) -> Vec<crate::world::messages::EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

fn compound(nbt: &[u8]) -> Compound {
    let mut input = nbt;
    Tag::read_network(&mut input, tuff_nbt::NbtLimits::NETWORK)
        .unwrap()
        .and_then(|tag| tag.as_compound().cloned())
        .expect("a compound")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_dimensions_join_the_world_with_their_types_and_biomes() {
    let world = start(memory());
    let custom = world.shared.dimensions.custom();
    assert_eq!(custom.rejected().len(), 1, "the nether already exists");
    let keys: Vec<&str> = world.shared.dimensions.keys();
    assert_eq!(&keys[3..], [SKY, MEADOW, BARREN]);

    let sky = world.shared.dimensions.by_key(SKY).unwrap();
    assert_eq!(sky.type_id(), DIMENSION_TYPE.entries.len() as i32);
    assert_eq!(sky.kind.name, "test:sky_type");
    assert_eq!(sky.shape, ChunkShape::new(0, 256));
    assert_eq!(sky.kind.coordinate_scale, 128.0);
    assert!(sky.default_clock.is_none());
    assert!(!sky.natural_spawning && !sky.special_spawners());
    let environment = sky.environment();
    assert!(environment.has_fixed_time);
    assert!(!environment.water_evaporates);
    assert!(!environment.respawn_anchor_works);
    assert_eq!(environment.bed_rule.can_sleep, BedCondition::Never);
    assert!(!environment.bed_rule.destroy_on_use);

    let meadow = world.shared.dimensions.by_key(MEADOW).unwrap();
    assert_eq!(meadow.kind.name, OVERWORLD);
    assert_eq!(
        meadow.type_id(),
        world.shared.dimensions.by_key(OVERWORLD).unwrap().type_id()
    );
    assert!(meadow.natural_spawning && !meadow.special_spawners());
    assert!(
        world
            .shared
            .dimensions
            .by_key(OVERWORLD)
            .unwrap()
            .special_spawners()
    );

    let biomes = world.shared.dimensions.biomes();
    let biome = WORLDGEN_BIOME.entries.len() as u16;
    assert_eq!(biomes.id(SKY_BIOME), Some(biome));
    assert_eq!(biomes.name(biome), SKY_BIOME);

    let types = custom.registry_entries(DIMENSION_TYPE.key);
    assert_eq!(types.len(), 1);
    assert_eq!(types[0].0, "test:sky_type");
    let data = compound(types[0].1);
    assert_eq!(data.get_bool("has_fixed_time"), Some(true));
    assert_eq!(data.get_str("skybox"), Some("none"));
    assert_eq!(data.get_int("height"), Some(256));
    let attributes = data.get_compound("attributes").unwrap();
    assert_eq!(
        attributes.get_str("minecraft:visual/sky_color"),
        Some("#80a0c0")
    );
    let biome_entries = custom.registry_entries(WORLDGEN_BIOME.key);
    assert_eq!(biome_entries.len(), 1);
    assert_eq!(biome_entries[0].0, SKY_BIOME);
    let data = compound(biome_entries[0].1);
    assert_eq!(
        data.get_compound("effects").unwrap().get_str("water_color"),
        Some("#3d57d6")
    );
    assert!(custom.registry_entries("minecraft:damage_type").is_empty());

    let packet = crate::protocol::encode_packet(&crate::protocol::configuration::RegistryData {
        registry: DIMENSION_TYPE.key,
        entries: DIMENSION_TYPE.entries,
        custom: &types,
    });
    let decoded =
        crate::trace_decode::typed_text(tuff_trace::ConnState::Configuration, &packet.into_bytes())
            .expect("registry data decodes");
    assert!(decoded.contains("test:sky_type"), "{decoded}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_reach_plugin_dimensions_and_their_chunks_save_with_custom_biomes() {
    let backend = memory();
    let world = start(backend.clone());
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let overworld = world.region(chunk.region()).await;
    subscribe(&overworld, &viewer, &mut events, &[chunk]).await;
    world.ticks(3).await;
    let (reply, summoned) = oneshot::channel();
    overworld
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:pig")
                .with("NoAI", true)
                .with("NoGravity", true),
            position: [8.5, 100.0, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());

    let moved = crate::world::commands::console::run(
        world.shared.clone(),
        "execute in test:sky run tp @e[type=pig] 4.5 64 4.5",
    )
    .await;
    assert_eq!(
        moved[0].to_value()["translate"],
        "commands.teleport.success.location.single"
    );
    let sky = world.region_in(SKY, chunk.region()).await;
    subscribe(&sky, &viewer, &mut events, &[chunk]).await;
    world.ticks(3).await;
    let pig = snapshots(&sky)
        .await
        .into_iter()
        .find(|e| e.kind == EntityType::Pig)
        .expect("the pig arrived in the plugin dimension");
    assert_eq!(pig.position, [4.5, 64.0, 4.5]);

    let set = crate::world::commands::console::run(
        world.shared.clone(),
        "execute in test:sky run setblock 1 70 1 minecraft:gold_block",
    )
    .await;
    assert_eq!(set[0].to_value()["translate"], "commands.setblock.success");
    let biomes = world.shared.dimensions.biomes().clone();
    world.shutdown().await;

    let shape = ChunkShape::new(0, 256);
    let mut store = backend.open_region(SKY, chunk.file_region()).unwrap();
    let blob = store
        .read_chunk(chunk)
        .unwrap()
        .expect("the sky chunk saved");
    let codec = backend.chunk_codec();
    let Decoded::Chunk(saved, _) = codec.decode_chunk_in(&blob, chunk, shape, &biomes).unwrap()
    else {
        panic!("saved chunk has terrain");
    };
    assert_eq!(saved.get(1, 70, 1).block(), Block::GoldBlock);
    assert_eq!(saved.get(1, GROUND, 1).block(), Block::GrassBlock);
    let custom = biomes.id(SKY_BIOME).unwrap();
    assert_eq!(saved.biome(1, 70, 1), custom);
    let Decoded::Chunk(vanilla_only, _) = codec.decode_chunk(&blob, chunk, shape).unwrap() else {
        panic!("saved chunk has terrain");
    };
    assert_eq!(
        vanilla_only.biome(1, 70, 1),
        tuff_world_model::chunk::plains(),
        "the biome is saved by name"
    );
    let entities = store
        .read_entities(chunk)
        .unwrap()
        .expect("the pig saved in the plugin dimension");
    let entities = codec.decode_entities(&entities, chunk).unwrap();
    assert!(
        entities
            .iter()
            .any(|e| e.get_str("id") == Some("minecraft:pig"))
    );
    assert!(
        backend
            .open_region(OVERWORLD, chunk.file_region())
            .unwrap()
            .read_entities(chunk)
            .unwrap()
            .is_none_or(|blob| codec.decode_entities(&blob, chunk).unwrap().is_empty()),
        "the pig left the overworld"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fillbiome_accepts_custom_biomes() {
    let world = start(memory());
    let run =
        |command: &'static str| crate::world::commands::console::run(world.shared.clone(), command);
    let ok = run("execute in test:sky run fillbiome 0 70 0 3 70 3 test:sky_meadow").await;
    let key = ok[0].to_value()["translate"].clone();
    assert_ne!(key, "command.unknown.argument", "{ok:?}");
    world.shutdown().await;
}

fn bed(pos: BlockPos) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let foot = Block::RedBed
        .default_state()
        .with_facing(tuff_vanilla_data::block::Facing::South)
        .unwrap();
    let head = foot
        .with_part(tuff_vanilla_data::block::Part::Head)
        .unwrap();
    vec![(pos, foot), (BlockPos::new(pos.x, pos.y, pos.z + 1), head)]
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beds_refuse_sleep_without_exploding_and_water_stays() {
    let world = start(memory());
    let (player, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    let sky = world.region_in(SKY, chunk.region()).await;
    subscribe(&sky, &player, &mut events, &[chunk]).await;
    world.ticks(3).await;

    let foot = BlockPos::new(8, GROUND + 1, 8);
    place_blocks(&world, &sky, bed(foot)).await;
    let mut click = place(&player, foot, Item::Stick, 1);
    click.item = None;
    click.held = None;
    click.position = [8.5, GROUND as f64 + 1.0, 6.5];
    sky.request(RegionRequest::Place(click)).await;
    let used: BedUse = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<BedUse>().cloned(),
        _ => None,
    })
    .await;
    assert!(!used.set_spawn);
    assert!(
        matches!(&used.sleep, Err(Some(message))
            if format!("{message:?}").contains("test.sky.no_sleep")),
        "{:?}",
        used.sleep
    );
    assert_eq!(
        block_at(&sky, foot).await.unwrap().block(),
        Block::RedBed,
        "the bed stays"
    );

    let floor = BlockPos::new(2, GROUND, 2);
    let above = BlockPos::new(2, GROUND + 1, 2);
    sky.request(RegionRequest::UseBucket(
        crate::world::messages::BucketUse {
            player: player.clone(),
            position: [2.5, GROUND as f64 + 1.0, 2.5],
            eye: [2.5, GROUND as f64 + 2.62, 2.5],
            look: crate::world::raycast::view_vector(0.0, 90.0),
            reach: 4.5,
            off_hand: false,
            item: Item::WaterBucket,
            fluid: Some(Block::Water),
            mob: None,
            mob_data: None,
            mob_components: None,
            sneaking: false,
            creative: false,
            sequence: 2,
            hit: None,
        },
    ))
    .await;
    assert!(matches!(
        outcome(&mut events, 2).await,
        ActionOutcome::Used {
            effect: HandEffect::Emptied(Item::Bucket),
            ..
        }
    ));
    assert_eq!(
        block_at(&sky, floor).await.unwrap().block(),
        Block::GrassBlock
    );
    assert_eq!(block_at(&sky, above).await.unwrap().block(), Block::Water);
    world.shutdown().await;
}

/// Runs natural spawning around a player in `dimension` for up to `cycles`
/// persistent-spawn ticks, returning the animals that spawned.
async fn spawn_cycles(
    world: &TestWorld,
    dimension: &str,
    cycles: u64,
) -> Vec<crate::world::messages::EntitySnapshot> {
    let (viewer, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(12, 12);
    let region = world.region_in(dimension, center.region()).await;
    let chunks: Vec<ChunkPos> = (-4..=4)
        .flat_map(|dx| (-4..=4).map(move |dz| ChunkPos::new(12 + dx, 12 + dz)))
        .collect();
    subscribe(&region, &viewer, &mut events, &chunks).await;
    let player = [200.5, GROUND as f64 + 1.0, 200.5];
    world.publish_players(dimension, &[(&viewer, player)]);
    world.ticks(5).await;
    let mut spawned = Vec::new();
    for cycle in 1..=cycles {
        world.clock.send_modify(|tick| *tick = cycle * 400 - 1);
        world
            .server_ticks
            .send_modify(|tick| *tick = cycle * 400 - 1);
        world.ticks(3).await;
        spawned = snapshots(&region)
            .await
            .into_iter()
            .filter(|s| s.kind != EntityType::Player)
            .collect();
        if !spawned.is_empty() {
            break;
        }
    }
    spawned
}

async fn spawned_in(dimension: &str) -> Vec<crate::world::messages::EntitySnapshot> {
    let world = start(memory());
    allow_spawning(&world);
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "spawn_phantoms",
                crate::world::game_rules::RuleValue::Bool(true),
            )
            .expect("a boolean rule");
    });
    let spawned = spawn_cycles(&world, dimension, 20).await;
    world.shutdown().await;
    spawned
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_spawns_naturally_where_the_provider_says_so() {
    assert!(
        !spawned_in(MEADOW).await.is_empty(),
        "animals spawn in a plugin dimension that allows it"
    );
    let none = spawned_in(BARREN).await;
    assert!(none.is_empty(), "{none:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_travel_into_plugin_dimensions_with_their_type() {
    use tuff_vanilla_data::packet::play::Clientbound;
    let world = start(memory());
    let (outbound, mut outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(21);
    let (_packets, packets_rx) = mpsc::channel(8);
    let mut session = crate::world::session::PlayerSession::new(
        world.shared.clone(),
        21,
        crate::auth::GameProfile {
            id: 21,
            name: "Traveller".into(),
            properties: Vec::new(),
        },
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        tuff_storage::admin::PermissionLevel::Owners,
    );
    let sky = world.dimension(SKY);
    let target = [4.5, GROUND as f64 + 1.0, 4.5];
    let moved = session
        .apply_directive(
            crate::world::messages::Directive::Teleport(crate::world::messages::Teleport {
                dimension: sky,
                position: target,
                relative: [false; 3],
                rotation: None,
                relative_rotation: [false; 2],
                look_at: None,
            }),
            crate::world::messages::DirectiveSource {
                name: TextComponent::new("Server"),
                player: None,
            },
        )
        .await;
    assert!(moved.affected);
    assert_eq!(session.dimension_id(), sky);
    assert_eq!(session.position(), target);
    let mut respawn = None;
    while let Ok(frame) = outbound_rx.try_recv() {
        let mut r = framed_payload(&frame);
        if r.read_var_int().unwrap() == Clientbound::Respawn.id() {
            respawn = Some((r.read_var_int().unwrap(), r.read_string().unwrap()));
        }
    }
    assert_eq!(
        respawn,
        Some((DIMENSION_TYPE.entries.len() as i32, SKY.to_owned()))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_test_coordinator_records_plugin_teleports() {
    let world = start(memory());
    let dimension = world.dimension(SKY);
    world
        .shared
        .coordinator
        .send(crate::world::messages::CoordinatorMessage::TeleportPlayer {
            uuid: 7,
            teleport: crate::world::messages::Teleport {
                dimension,
                position: [1.5, 64.0, -2.5],
                relative: [false; 3],
                rotation: None,
                relative_rotation: [false; 2],
                look_at: None,
            },
        })
        .await
        .unwrap();
    let (reply, synced) = oneshot::channel();
    world
        .shared
        .coordinator
        .send(crate::world::messages::CoordinatorMessage::Sync { reply })
        .await
        .unwrap();
    synced.await.unwrap();
    {
        let teleports = world.teleports.lock().unwrap();
        assert_eq!(teleports.len(), 1);
        assert_eq!(teleports[0].0, 7);
        assert_eq!(teleports[0].1.dimension, dimension);
        assert_eq!(teleports[0].1.position, [1.5, 64.0, -2.5]);
    }
    world.shutdown().await;
}
