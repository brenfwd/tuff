//! Persistence and storage: saving/loading across restarts, unreadable
//! chunks, pluggable backends, light dirtiness and per-dimension storage.

use super::*;
use crate::storage::codec::WorldBackendExt as _;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edits_persist_across_restarts() {
    let dir = TestDir::new();
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let target = BlockPos::new(1, height, 1);

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 7,
        })
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 7,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Broken { state, .. } if state.block() == Block::GrassBlock)
    );
    world.shutdown().await;

    let root = stored_chunk(dir.path(), pos, false).expect("chunk saved");
    assert_eq!(root.get_bool("isLightOn"), Some(true));
    let Decoded::Chunk(chunk, meta) = decode_stored(root, pos).unwrap() else {
        panic!("saved chunk has terrain");
    };
    let light = chunk.light().expect("saved light");
    let SectionLight::Data(sky) =
        &light.sky[((height >> 4) - min_light_section(ChunkShape::OVERWORLD)) as usize]
    else {
        panic!("no sky light where the block was broken");
    };
    let index = ((height & 15) << 8 | 1 << 4 | 1) as usize;
    assert_eq!(sky[index >> 1] >> ((index & 1) * 4) & 0x0F, 15);
    assert_eq!(meta.status, tuff_world_model::chunk_meta::STATUS_FULL);
    assert!(chunk.get(1, height, 1).block() == Block::Air);
    assert_eq!(
        chunk.get(2, height, 1),
        Generator::new(1).generate(pos).get(2, height, 1)
    );
    let entities = stored_chunk(dir.path(), pos, true).expect("drops saved");
    assert_eq!(entities.get_list("Entities").unwrap().len(), 1);

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let item = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) if track.player.is_none() => Some(track),
        _ => None,
    })
    .await;
    assert_eq!(item.chunk, pos);
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 8,
        })
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 8,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Rejected(_)),
        "already air after reload"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreadable_chunks_are_never_overwritten() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(3, 4);
    let region_dir = VanillaBackend::new(dir.path())
        .paths()
        .region_dir(OVERWORLD);
    let garbage = tuff_storage::region_file::RawChunk::compress(vec![0xFF; 64]).unwrap();
    {
        let mut file = RegionFile::open(&region_dir, 0, 0, true).unwrap().unwrap();
        file.write(pos, &garbage).unwrap();
    }

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let height = Generator::new(1).height(pos.x * 16, pos.z * 16);
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: BlockPos::new(pos.x * 16, height, pos.z * 16),
            tool: None,
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;

    let mut file = RegionFile::open(&region_dir, 0, 0, false).unwrap().unwrap();
    assert_eq!(file.read(pos).unwrap(), Some(garbage));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regions_work_on_any_backend() {
    let backend: Arc<dyn WorldBackend> = Arc::new(tuff_storage::memory::MemoryBackend::default());
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let target = BlockPos::new(1, height, 1);

    let world = TestWorld::start_with(backend.clone());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 7,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 7,
            outcome: ActionOutcome::Broken { .. },
        } => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;

    let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
    let blob = store.read_chunk(pos).unwrap().expect("chunk saved");
    let Decoded::Chunk(chunk, _) = backend
        .chunk_codec()
        .decode_chunk(&blob, pos, ChunkShape::OVERWORLD)
        .unwrap()
    else {
        panic!("saved chunk has terrain");
    };
    assert_eq!(chunk.get(1, height, 1).block(), Block::Air);
    assert!(store.read_entities(pos).unwrap().is_some());

    let world = TestWorld::start_with(backend.clone());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 8,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 8,
            outcome: ActionOutcome::Rejected(_),
        } => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edits_send_light_updates() {
    let dir = TestDir::new();
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);

    let world = TestWorld::start(dir.path());
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: BlockPos::new(1, height, 1),
            tool: None,
            mode: BreakMode::Creative,
            sequence: 1,
        })
        .await;
    world.ticks(2).await;
    let light_update = tuff_vanilla_data::packet::play::Clientbound::LightUpdate.id();
    expect(&mut events, |event| match event {
        PlayerEvent::ChunkPackets {
            pos: at, packets, ..
        } if at == pos && packets.iter().any(|p| framed_packet_id(p) == light_update) => Some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

/// The primitive `/tp` (see `command_execution`) uses to hold a teleport
/// back until its destination exists: `RegionRequest::AwaitChunk` must
/// install an ungenerated chunk before replying, not just check whether it
/// happens to be there already.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn await_chunk_installs_ungenerated_terrain_before_replying() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = BlockPos::new(20_048, 200, 20_048);
    let region = world.region(ChunkPos::of_block(pos).region()).await;

    assert_eq!(
        block_at(&region, pos).await,
        None,
        "chunk should not be loaded yet"
    );

    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::AwaitChunk { pos, reply })
        .await;
    answer.await.unwrap();

    assert!(
        block_at(&region, pos).await.is_some(),
        "AwaitChunk must install the chunk before replying"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regions_of_different_dimensions_are_distinct_actors_with_distinct_storage() {
    let backend: Arc<dyn WorldBackend> = Arc::new(tuff_storage::memory::MemoryBackend::default());
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let world = TestWorld::start_with(backend.clone());
    let overworld = world.region(pos.region()).await;
    let nether = world.region_in(THE_NETHER, pos.region()).await;
    assert!(!overworld.same(&nether));
    assert_ne!(overworld.dimension, nether.dimension);
    assert_eq!(overworld.pos, nether.pos);
    assert!(world.region(pos.region()).await.same(&overworld));

    for region in [&overworld, &nether] {
        region
            .request(RegionRequest::Subscribe {
                player: viewer.clone(),
                chunks: vec![pos],
            })
            .await;
    }
    let mut packets = std::collections::HashMap::new();
    while packets.len() < 2 {
        let (dimension, packet) = expect(&mut events, |event| match event {
            PlayerEvent::Chunk {
                dimension,
                pos: at,
                packet,
                ..
            } if at == pos => Some((dimension, packet)),
            _ => None,
        })
        .await;
        packets.insert(dimension, packet);
    }
    let overworld_chunk = &packets[&overworld.dimension];
    let nether_chunk = &packets[&nether.dimension];
    assert_ne!(overworld_chunk, nether_chunk);

    let nether_floor = BlockPos::new(3, 0, 3);
    nether
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: BlockPos::new(3, 1, 3),
            tool: None,
            mode: BreakMode::Creative,
            sequence: 1,
        })
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Broken { state, .. } if state.block() == Block::Netherrack),
        "{outcome:?}"
    );
    let (reply, answer) = oneshot::channel();
    nether
        .request(RegionRequest::BlockAt {
            pos: nether_floor,
            reply,
        })
        .await;
    assert_eq!(answer.await.unwrap().unwrap().block(), Block::Bedrock);
    let (reply, answer) = oneshot::channel();
    overworld
        .request(RegionRequest::BlockAt {
            pos: BlockPos::new(3, 1, 3),
            reply,
        })
        .await;
    assert_eq!(answer.await.unwrap().unwrap().block(), Block::Stone);
    world.shutdown().await;

    let nether_shape = ChunkShape::new(0, 256);
    let mut store = backend.open_region(THE_NETHER, pos.file_region()).unwrap();
    let blob = store.read_chunk(pos).unwrap().expect("nether chunk saved");
    let Decoded::Chunk(chunk, _) = backend
        .chunk_codec()
        .decode_chunk(&blob, pos, nether_shape)
        .unwrap()
    else {
        panic!("saved chunk has terrain");
    };
    assert_eq!(chunk.shape(), nether_shape);
    assert_eq!(chunk.get(3, 1, 3).block(), Block::Air);
    assert_eq!(chunk.get(3, 0, 3).block(), Block::Bedrock);
    let mut store = backend.open_region(OVERWORLD, pos.file_region()).unwrap();
    let blob = store
        .read_chunk(pos)
        .unwrap()
        .expect("overworld chunk saved");
    let Decoded::Chunk(chunk, _) = backend
        .chunk_codec()
        .decode_chunk(&blob, pos, ChunkShape::OVERWORLD)
        .unwrap()
    else {
        panic!("saved chunk has terrain");
    };
    assert_eq!(chunk.get(3, 1, 3).block(), Block::Stone);
    assert_eq!(chunk.get(3, -64, 3).block(), Block::Bedrock);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forced_chunk_tickets_are_written_by_the_periodic_autosave() {
    use crate::world::messages::{ChunkTicket, ManagerMessage};
    use tuff_storage::chunk_tickets::ChunkTickets;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let dimension = world.dimension(OVERWORLD);
    let key = world.shared.dimension(dimension).key.clone();
    let chunk = ChunkPos::new(2, 3);
    let (reply, answer) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension,
            chunks: vec![chunk],
            ticket: ChunkTicket::Forced,
            on: true,
            reply,
        })
        .await
        .unwrap();
    answer.await.unwrap();
    world.ticks(2).await;
    let saved = || {
        ChunkTickets::load(world.shared.backend.documents(), &key)
            .unwrap()
            .forced
    };
    assert!(saved().is_empty(), "nothing is written before an autosave");
    world.clock.send_modify(|tick| *tick = 5999);
    world.server_ticks.send_modify(|tick| *tick = 5999);
    world.ticks(1).await;
    for _ in 0..50 {
        if !saved().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(saved(), vec![chunk]);
    world.shutdown().await;
}

struct Counter(i32);

struct CounterSaved;

impl crate::world::session::hooks::PlayerSaved for CounterSaved {
    fn document(&self, uuid: u128) -> tuff_storage::document::Document<'static> {
        tuff_storage::document::Document::PlayerData {
            id: "counter-test",
            uuid,
        }
    }

    fn load(&self, session: &mut crate::world::session::PlayerSession, root: Option<Compound>) {
        let value = root.and_then(|root| root.get_int("n")).unwrap_or(-1);
        session.ext.entry_or(Counter(value));
    }

    fn save(&self, session: &crate::world::session::PlayerSession) -> Option<Compound> {
        let counter = session.ext.get::<Counter>()?;
        Some(Compound::new().with("n", counter.0 + 1))
    }
}

struct CounterFeature;

impl crate::feature::Feature for CounterFeature {
    fn id(&self) -> &'static str {
        "counter-test"
    }

    fn load(&self, cx: &mut crate::feature::LoadContext) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let id = self.id();
            cx.registries()
                .player_saved
                .register(id, id, Box::new(CounterSaved))?;
        }
        Ok(())
    }
}

async fn counter_survives_rejoin(backend: std::sync::Arc<dyn tuff_storage::backend::WorldBackend>) {
    let first = TestWorld::start_with_features_on(backend.clone(), vec![Box::new(CounterFeature)]);
    let (mut player, _out) = session_with_outbound(&first, 5);
    player.load_saved().await;
    assert_eq!(player.ext.get::<Counter>().unwrap().0, -1);
    player.save().await;
    first.shutdown().await;

    let second = TestWorld::start_with_features_on(backend, vec![Box::new(CounterFeature)]);
    let (mut again, _out) = session_with_outbound(&second, 5);
    again.load_saved().await;
    assert_eq!(again.ext.get::<Counter>().unwrap().0, 0);
    second.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_player_data_survives_rejoin_on_a_folder_world() {
    let dir = TestDir::new();
    counter_survives_rejoin(Storage::default().open(dir.path()).unwrap()).await;
    assert!(
        dir.path()
            .join("data/players/counter-test")
            .read_dir()
            .unwrap()
            .count()
            == 1
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_player_data_survives_rejoin_on_a_sqlite_world() {
    let dir = TestDir::new();
    let backend = std::sync::Arc::new(
        tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
    );
    counter_survives_rejoin(backend).await;
}
