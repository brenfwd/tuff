//! `/tuff chunk generate`: every chunk of the shape is generated, saved and
//! unloaded again, progress shows on a boss bar, a regenerating job drops
//! what was in the chunks first, and jobs can be cancelled.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::storage::codec::WorldBackendExt as _;
use crate::world::messages::{BlockOp, BlockOpResult, ManagerMessage};
use crate::world::pregen::{Area, Finished, Shape};
use crate::world::region_api::RegionTask;
use crate::world::session::PlayerSession;
use crate::world::session::hooks::SessionTick;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::Block;

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect()
}

async fn finished(world: &TestWorld) -> Finished {
    tokio::time::timeout(Duration::from_secs(120), world.shared.pregen.wait())
        .await
        .expect("the job ends")
        .expect("a finished job")
}

/// Every chunk loaded in the running regions.
async fn loaded(world: &TestWorld) -> Vec<ChunkPos> {
    let (reply, regions) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .unwrap();
    let mut out = Vec::new();
    for region in regions.await.unwrap() {
        let (tx, rx) = oneshot::channel();
        region
            .request(RegionRequest::WhenLoaded {
                min: BlockPos::new(16, 0, 16),
                max: BlockPos::new(0, 0, 0),
                run: RegionTask::new(move |w| {
                    let _ = tx.send(w.loaded_chunks());
                }),
            })
            .await;
        if let Ok(chunks) = rx.await {
            out.extend(chunks);
        }
    }
    out
}

async fn running_regions(world: &TestWorld) -> usize {
    let (reply, regions) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .unwrap();
    regions.await.unwrap().len()
}

fn is_full(dir: &Path, pos: ChunkPos) -> bool {
    stored_chunk(dir, pos, false)
        .and_then(|root| root.get("Status").and_then(Tag::as_str).map(str::to_owned))
        .is_some_and(|status| status == "minecraft:full")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_chunk_of_the_shape_is_generated_saved_and_unloaded() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let area = Area::new(-100, 60, 40, Shape::Circle);
    let out = console(&world, "tuff chunk generate -100 60 40 circle").await;
    assert!(
        out.iter()
            .any(|l| l.contains(&format!("Generating {} chunks", area.count()))),
        "{out:?}"
    );
    let done = finished(&world).await;
    assert_eq!(done.generated, area.count());
    assert_eq!(done.failed, 0);
    assert!(!done.cancelled);

    for pos in area.chunks() {
        assert!(is_full(dir.path(), pos), "{pos:?} was saved in full");
    }
    let c = area.centre();
    let reach = area.reach() + 1;
    for dz in -reach..=reach {
        for dx in -reach..=reach {
            let pos = ChunkPos::new(c.x + dx, c.z + dz);
            if !area.contains(pos) {
                assert!(stored_chunk(dir.path(), pos, false).is_none(), "{pos:?}");
            }
        }
    }
    assert_eq!(loaded(&world).await, Vec::<ChunkPos>::new());
    world.ticks(620).await;
    assert_eq!(
        running_regions(&world).await,
        0,
        "no ticket outlives the job"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generating_again_keeps_what_was_built() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    console(&world, "tuff chunk generate 8 8 0 square").await;
    finished(&world).await;
    let pos = BlockPos::new(3, 200, 5);
    let region = world.region(RegionPos::new(0, 0)).await;
    set_block(&region, pos, Block::GoldBlock.default_state()).await;
    console(&world, "save-all flush").await;

    console(&world, "tuff chunk generate 8 8 20 square").await;
    let done = finished(&world).await;
    assert_eq!(done.generated, 9);
    assert_eq!(
        read_block(&region, pos).await,
        Some(Block::GoldBlock.default_state())
    );
    world.shutdown().await;
}

async fn read_block(region: &RegionHandle, pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Get {
                positions: vec![pos],
                load: true,
            },
            reply,
        })
        .await;
    match answer.await.unwrap() {
        BlockOpResult::States(states) => states[0],
        _ => None,
    }
}

async fn set_block(region: &RegionHandle, pos: BlockPos, state: tuff_vanilla_data::BlockState) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: vec![(pos, state)],
                destroy: false,
                keep: false,
                strict: true,
                quiet: true,
                data: None,
            },
            reply,
        })
        .await;
    assert!(matches!(answer.await.unwrap(), BlockOpResult::Set(1)));
}

fn entity_ids(dir: &Path, pos: ChunkPos) -> Vec<String> {
    let Some(root) = stored_chunk(dir, pos, true) else {
        return Vec::new();
    };
    match root.get("Entities") {
        Some(Tag::List(list)) => list
            .iter()
            .filter_map(|tag| match tag {
                Tag::Compound(entity) => entity.get("id").and_then(Tag::as_str).map(str::to_owned),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn block_entity_ids(dir: &Path, pos: ChunkPos) -> Vec<String> {
    let Some(root) = stored_chunk(dir, pos, false) else {
        return Vec::new();
    };
    match root.get("block_entities") {
        Some(Tag::List(list)) => list
            .iter()
            .filter_map(|tag| match tag {
                Tag::Compound(entity) => entity.get("id").and_then(Tag::as_str).map(str::to_owned),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A gold block, a chest and a pig in `chunk`, saved.
async fn furnish(world: &TestWorld, dir: &Path, region: &RegionHandle, chunk: ChunkPos) {
    let (x, z) = (chunk.x * 16 + 4, chunk.z * 16 + 4);
    set_block(
        region,
        BlockPos::new(x, 200, z),
        Block::GoldBlock.default_state(),
    )
    .await;
    set_block(
        region,
        BlockPos::new(x + 1, 200, z),
        Block::Chest.default_state(),
    )
    .await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:pig")
                .with("NoAI", true)
                .with("PersistenceRequired", true),
            position: [f64::from(x) + 2.5, 201.0, f64::from(z) + 0.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    console(world, "save-all flush").await;
    assert_eq!(entity_ids(dir, chunk), ["minecraft:pig"]);
    assert!(block_entity_ids(dir, chunk).contains(&"minecraft:chest".to_owned()));
}

async fn assert_natural(dir: &Path, region: &RegionHandle, chunk: ChunkPos) {
    assert!(is_full(dir, chunk));
    assert_eq!(entity_ids(dir, chunk), Vec::<String>::new(), "{chunk:?}");
    assert!(!block_entity_ids(dir, chunk).contains(&"minecraft:chest".to_owned()));
    let natural = Generator::new(1).generate(chunk);
    let (x, z) = (chunk.x * 16 + 4, chunk.z * 16 + 4);
    for pos in [BlockPos::new(x, 200, z), BlockPos::new(x + 1, 200, z)] {
        assert_eq!(
            read_block(region, pos).await,
            Some(natural.get(pos.x, pos.y, pos.z))
        );
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_regenerate_drops_blocks_entities_and_block_entities() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let unloaded = ChunkPos::new(1, 1);
    let kept = ChunkPos::new(3, 1);
    furnish(&world, dir.path(), &region, unloaded).await;
    world.ticks(200).await;
    assert!(!loaded(&world).await.contains(&unloaded));
    furnish(&world, dir.path(), &region, kept).await;

    let out = console(
        &world,
        "tuff chunk generate 40 24 16 square --force-regenerate",
    )
    .await;
    assert!(
        out.iter().any(|l| l.starts_with("Regenerating 9 chunks")),
        "{out:?}"
    );
    let done = finished(&world).await;
    assert_eq!((done.generated, done.failed), (9, 0));

    assert_natural(dir.path(), &region, unloaded).await;
    assert_natural(dir.path(), &region, kept).await;
    let (reply, entities) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    assert!(
        entities.await.unwrap().is_empty(),
        "the pigs went with their chunks"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_job_stops_early_and_status_says_so() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text(crate::world::pregen::PARALLEL_KEY, "1")
        .unwrap();
    console(&world, "tuff chunk generate 0 0 2000 square").await;
    let status = console(&world, "tuff chunk status").await;
    assert!(
        status[0].starts_with("Job 1 by Server in minecraft:overworld"),
        "{status:?}"
    );
    let out = console(&world, "tuff chunk cancel").await;
    assert!(
        out[0].starts_with("Cancelling chunk generation job 1"),
        "{out:?}"
    );
    let done = finished(&world).await;
    assert!(done.cancelled);
    assert!(done.generated < done.total);
    let status = console(&world, "tuff chunk status").await;
    assert_eq!(status[0], "No chunk generation job is running");
    assert!(status[1].starts_with("Last job 1 cancelled"), "{status:?}");
    let again = console(&world, "tuff chunk cancel").await;
    assert_eq!(again, ["no chunk generation job is running"]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_job_waits_for_the_first() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text(crate::world::pregen::PARALLEL_KEY, "1")
        .unwrap();
    console(&world, "tuff chunk generate 0 0 2000 square").await;
    let out = console(&world, "tuff chunk generate 0 0 10 circle").await;
    assert_eq!(out, ["chunk generation job 1 is still running"]);
    console(&world, "tuff chunk cancel").await;
    finished(&world).await;
    world.shutdown().await;
}

fn session(world: &TestWorld, id: i32) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(id);
    let (_packets, packets_rx) = mpsc::channel(8);
    let profile = GameProfile {
        id: id as u128,
        name: format!("Player{id}"),
        properties: Vec::new(),
    };
    let session = PlayerSession::new(
        world.shared.clone(),
        id,
        profile,
        ClientInformation::default(),
        outbound,
        handle,
        events,
        packets_rx,
        PermissionLevel::Owners,
    );
    (session, outbound_rx)
}

/// The action of each boss event for the job's bar the session sent.
fn bar_actions(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>) -> Vec<i32> {
    let id = tuff_vanilla_data::packet::play::Clientbound::BossEvent.id();
    let mut actions = Vec::new();
    while let Ok(frame) = outbound.try_recv() {
        let mut r = framed_payload(&frame);
        if r.read_var_int().ok() == Some(id)
            && r.read_uuid().ok() == Some(tuff_feature_builtin_core_commands::chunk::BAR_UUID)
        {
            actions.push(r.read_var_int().unwrap());
        }
    }
    actions
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operators_see_the_job_on_a_boss_bar_until_it_ends() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text(crate::world::pregen::PARALLEL_KEY, "1")
        .unwrap();
    let (mut session, mut outbound) = session(&world, 1000);
    session.run_session_tick_hooks(SessionTick::Late).await;
    assert_eq!(bar_actions(&mut outbound), Vec::<i32>::new());

    console(&world, "tuff chunk generate 0 0 2000 square").await;
    session.run_session_tick_hooks(SessionTick::Late).await;
    assert_eq!(bar_actions(&mut outbound), [0], "the bar is added");
    let mut watch = world.shared.pregen.watch();
    let first = watch.borrow_and_update().running.clone().unwrap().done;
    tokio::time::timeout(
        Duration::from_secs(60),
        watch.wait_for(|s| s.running.as_ref().is_some_and(|p| p.done > first)),
    )
    .await
    .unwrap()
    .unwrap();
    session.run_session_tick_hooks(SessionTick::Late).await;
    let updates = bar_actions(&mut outbound);
    assert!(updates.contains(&2) && updates.contains(&3), "{updates:?}");

    console(&world, "tuff chunk cancel").await;
    finished(&world).await;
    session.run_session_tick_hooks(SessionTick::Late).await;
    assert_eq!(bar_actions(&mut outbound), [1], "the bar is removed");
    world.shutdown().await;
}

fn priorities() -> &'static std::sync::Mutex<std::collections::HashMap<ChunkPos, u32>> {
    static ASKED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<ChunkPos, u32>>> =
        std::sync::OnceLock::new();
    ASKED.get_or_init(Default::default)
}

/// A pipeline that notes each priority asked for and answers at once.
struct Prompt;

impl generator::ChunkPipeline for Prompt {
    fn want(&self, pos: ChunkPos, reply: RegionHandle, priority: u32) -> bool {
        priorities().lock().unwrap().insert(pos, priority);
        let mut chunk = tuff_world_model::chunk::Chunk::empty(ChunkShape::OVERWORLD);
        chunk.set(0, 0, 0, Block::Stone.default_state());
        reply.internal(RegionInternal::Generated {
            pos,
            finished: Box::new(generator::Finished::new(chunk)),
        });
        true
    }
}

struct Prompted;

impl ChunkGenerator for Prompted {
    fn generate(&self, _: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        tuff_world_model::chunk::Chunk::empty(shape)
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 61, 0)
    }

    fn pipeline(
        self: Arc<Self>,
        _: generator::PipelineContext,
    ) -> Option<Arc<dyn generator::ChunkPipeline>> {
        Some(Arc::new(Prompt))
    }
}

fn prompted(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    if settings.key == OVERWORLD {
        Arc::new(Prompted)
    } else {
        tuff_test_support::placeholder::for_settings(settings, seed)
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pre_generation_asks_after_viewed_chunks() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), prompted);
    let region = world.region(RegionPos::new(-1, 1)).await;
    let viewed = ChunkPos::new(-20, 40);
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[viewed]).await;
    let spec = crate::world::pregen::JobSpec {
        dimension: world.dimension(OVERWORLD),
        area: Area::new(-312, 648, 16, Shape::Square),
        regenerate: false,
        by: "test".to_owned(),
    };
    world
        .shared
        .pregen
        .start(&world.shared, spec)
        .await
        .unwrap();
    let done = finished(&world).await;
    assert_eq!((done.generated, done.failed), (9, 0));
    let asked = priorities().lock().unwrap().clone();
    assert_eq!(asked.get(&viewed), Some(&generator::VIEWED));
    for pos in Area::new(-312, 648, 16, Shape::Square).chunks() {
        if pos != viewed {
            assert_eq!(asked.get(&pos), Some(&generator::BACKGROUND), "{pos:?}");
            assert!(is_full(dir.path(), pos));
        }
    }
    assert_eq!(loaded(&world).await, [viewed], "the viewed chunk stays");
    world.shutdown().await;
}

/// The decoration marks of the staged test generator in `chunk`.
async fn marks(region: &RegionHandle, chunk: ChunkPos) -> usize {
    let mut count = 0;
    for x in 0..9 {
        let pos = BlockPos::new(chunk.x * 16 + x, 61, chunk.z * 16);
        if read_block(region, pos).await.map(|s| s.block()) == Some(Block::GoldBlock) {
            count += 1;
        }
    }
    count
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regenerated_chunks_are_decorated_by_every_neighbour_again() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(
        Storage::default().open(dir.path()).unwrap(),
        super::pipeline::staged,
    );
    console(&world, "tuff chunk generate 104 104 16 square").await;
    assert_eq!(finished(&world).await.generated, 9);
    let centre = ChunkPos::new(6, 6);
    let region = world.region(centre.region()).await;
    assert_eq!(marks(&region, centre).await, 9);
    set_block(
        &region,
        BlockPos::new(96, 61, 96),
        Block::Air.default_state(),
    )
    .await;
    console(&world, "save-all flush").await;
    assert_eq!(marks(&region, centre).await, 8);

    console(
        &world,
        "tuff chunk generate 104 104 16 square --force-regenerate",
    )
    .await;
    let done = finished(&world).await;
    assert_eq!((done.generated, done.failed), (9, 0));
    assert_eq!(marks(&region, centre).await, 9, "decorated afresh");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_of_a_regenerated_chunk_is_sent_the_new_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let chunk = ChunkPos::new(10, 10);
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let gold = BlockPos::new(164, 200, 164);
    set_block(&region, gold, Block::GoldBlock.default_state()).await;

    console(
        &world,
        "tuff chunk generate 168 168 0 circle --force-regenerate",
    )
    .await;
    assert_eq!(finished(&world).await.generated, 1);
    expect(&mut events, |event| match event {
        PlayerEvent::Chunk { pos, .. } if pos == chunk => Some(()),
        _ => None,
    })
    .await;
    let natural = Generator::new(1).generate(chunk);
    assert_eq!(
        read_block(&region, gold).await,
        Some(natural.get(gold.x, gold.y, gold.z))
    );
    assert_eq!(
        loaded(&world).await,
        [chunk],
        "the viewed chunk stays loaded"
    );
    world.shutdown().await;
}

/// Ticks the world until the job ends, as a running server would.
async fn ticking_until_finished(world: &TestWorld) -> Finished {
    let run = async {
        let wait = world.shared.pregen.wait();
        tokio::pin!(wait);
        loop {
            tokio::select! {
                done = &mut wait => return done.expect("a finished job"),
                () = world.ticks(1) => {}
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(120), run).await {
        Ok(done) => done,
        Err(_) => panic!(
            "the job stalled: {:?}",
            world.shared.pregen.status().running
        ),
    }
}

/// Force-regenerates the 13x13 square of radius 100 around `(x, z)` with
/// a player standing in its middle, viewing all of it and more, and pigs
/// in it; checks the job ends, the player is sent every chunk again and,
/// with the staged test generator, that every inner chunk is decorated
/// by all its neighbours afresh.
async fn regenerate_around_a_player(world: &TestWorld, x: i32, z: i32, staged: bool) {
    let area = Area::new(x, z, 100, Shape::Square);
    assert_eq!(area.count(), 169);
    let centre = area.centre();
    let (viewer, mut events) = PlayerHandle::new(1000);
    let view: Vec<ChunkPos> = (-8..=8)
        .flat_map(|dz| (-8..=8).map(move |dx| ChunkPos::new(centre.x + dx, centre.z + dz)))
        .collect();
    tuff_test_support::Area::subscribe(world, &viewer, &mut events, &view).await;
    let standing = [f64::from(x) + 0.5, 61.0, f64::from(z) + 0.5];
    let home = world.region(centre.region()).await;
    home.request(update(&viewer, 1, standing)).await;
    world.publish_players(OVERWORLD, &[(&viewer, standing)]);
    for chunk in [centre, ChunkPos::new(centre.x + 3, centre.z - 2)] {
        let region = world.region(chunk.region()).await;
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt: Compound::new()
                    .with("id", "minecraft:pig")
                    .with("PersistenceRequired", true),
                position: [
                    f64::from(chunk.x * 16) + 8.5,
                    61.0,
                    f64::from(chunk.z * 16) + 8.5,
                ],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    world.ticks(5).await;
    if staged {
        set_block(
            &home,
            BlockPos::new(centre.x * 16, 61, centre.z * 16),
            Block::Air.default_state(),
        )
        .await;
    }
    while events.try_recv().is_ok() {}

    let out = console(
        world,
        &format!("tuff chunk generate {x} {z} 100 square --force-regenerate"),
    )
    .await;
    assert!(
        out.iter().any(|l| l.starts_with("Regenerating 169 chunks")),
        "{out:?}"
    );
    let done = ticking_until_finished(world).await;
    assert_eq!(
        (done.generated, done.failed, done.remove_failed),
        (169, 0, 0),
        "{done:?}"
    );

    let mut resent = HashSet::new();
    for _ in 0..200 {
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::Chunk { pos, .. } = event {
                resent.insert(pos);
            }
        }
        if area.chunks().all(|c| resent.contains(&c)) {
            break;
        }
        world.ticks(1).await;
    }
    let missing: Vec<ChunkPos> = area.chunks().filter(|c| !resent.contains(c)).collect();
    assert!(missing.is_empty(), "never resent: {missing:?}");
    if staged {
        let inner: Vec<ChunkPos> = area
            .chunks()
            .filter(|c| {
                (-1..=1)
                    .all(|dz| (-1..=1).all(|dx| area.contains(ChunkPos::new(c.x + dx, c.z + dz))))
            })
            .collect();
        for chunk in inner {
            let region = world.region(chunk.region()).await;
            assert_eq!(marks(&region, chunk).await, 9, "{chunk:?} decorated afresh");
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_regenerating_around_a_player_completes_and_resends_every_chunk() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(
        Storage::default().open(dir.path()).unwrap(),
        super::pipeline::staged,
    );
    regenerate_around_a_player(&world, 325, 166, true).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_regenerating_across_regions_in_sqlite_completes() {
    let dir = TestDir::new();
    let backend = Arc::new(
        tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
    );
    let world = TestWorld::start_generated(backend, super::pipeline::staged);
    regenerate_around_a_player(&world, 520, 520, true).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_regenerating_with_the_world_map_loaded_completes() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(
        dir.path(),
        vec![Box::new(tuff_feature_builtin_world_map::WorldMapFeature)],
    );
    regenerate_around_a_player(&world, 325, 166, false).await;
    world.shutdown().await;
}

/// A backend whose region stores cannot delete chunks.
struct Undeletable(Arc<dyn tuff_storage::backend::WorldBackend>);

struct UndeletableStore(Box<dyn tuff_storage::backend::RegionStore>);

impl tuff_storage::backend::RegionStore for UndeletableStore {
    fn read_chunk(&mut self, pos: ChunkPos) -> std::io::Result<Option<Vec<u8>>> {
        self.0.read_chunk(pos)
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> std::io::Result<()> {
        self.0.write_chunk(pos, blob)
    }

    fn read_entities(&mut self, pos: ChunkPos) -> std::io::Result<Option<Vec<u8>>> {
        self.0.read_entities(pos)
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> std::io::Result<()> {
        self.0.write_entities(pos, blob)
    }

    fn read_pois(&mut self, pos: ChunkPos) -> std::io::Result<Option<Vec<u8>>> {
        self.0.read_pois(pos)
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> std::io::Result<()> {
        self.0.write_pois(pos, blob)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }

    fn list_chunks(&mut self) -> std::io::Result<Vec<ChunkPos>> {
        self.0.list_chunks()
    }
}

impl tuff_storage::backend::WorldBackend for Undeletable {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn lock(&self) -> std::io::Result<Box<dyn tuff_storage::backend::WorldLock>> {
        self.0.lock()
    }

    fn documents(&self) -> &dyn tuff_storage::backend::Documents {
        self.0.documents()
    }

    fn open_region(
        &self,
        dimension: &str,
        region: tuff_world_model::pos::FileRegionPos,
    ) -> std::io::Result<Box<dyn tuff_storage::backend::RegionStore>> {
        Ok(Box::new(UndeletableStore(
            self.0.open_region(dimension, region)?,
        )))
    }

    fn blob_codec(&self) -> Arc<dyn tuff_storage::backend::BlobCodec> {
        self.0.blob_codec()
    }

    fn list_dimensions(&self) -> std::io::Result<Vec<String>> {
        self.0.list_dimensions()
    }

    fn list_regions(
        &self,
        dimension: &str,
    ) -> std::io::Result<Vec<tuff_world_model::pos::FileRegionPos>> {
        self.0.list_regions(dimension)
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_remove_is_reported_and_the_job_still_ends() {
    let dir = TestDir::new();
    let world = TestWorld::start_with(Arc::new(Undeletable(
        Storage::default().open(dir.path()).unwrap(),
    )));
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[ChunkPos::new(2, 2)]).await;
    console(&world, "tuff chunk generate 40 40 16 square").await;
    assert_eq!(finished(&world).await.generated, 9);

    console(
        &world,
        "tuff chunk generate 40 40 16 square --force-regenerate",
    )
    .await;
    let done = finished(&world).await;
    assert_eq!(
        (done.remove_failed, done.generated, done.failed),
        (9, 9, 0),
        "{done:?}"
    );
    let status = console(&world, "tuff chunk status").await;
    assert!(status[1].contains("9 could not be removed"), "{status:?}");
    assert_eq!(loaded(&world).await, [ChunkPos::new(2, 2)]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_shows_the_phase_and_what_chunks_wait_on() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text(crate::world::pregen::PARALLEL_KEY, "1")
        .unwrap();
    console(
        &world,
        "tuff chunk generate 0 0 2000 square --force-regenerate",
    )
    .await;
    let status = console(&world, "tuff chunk status").await;
    assert!(
        status[1].starts_with("Phase 1 of 2: removing, "),
        "{status:?}"
    );
    assert!(status[1].ends_with("; then generating"), "{status:?}");
    assert!(status[2].starts_with("Waiting on "), "{status:?}");
    let mut watch = world.shared.pregen.watch();
    let waited = tokio::time::timeout(
        Duration::from_secs(60),
        watch.wait_for(|s| s.running.as_ref().is_some_and(|p| !p.waiting.is_empty())),
    )
    .await
    .unwrap()
    .unwrap()
    .running
    .clone()
    .unwrap();
    assert_eq!(waited.in_flight, 1);
    console(&world, "tuff chunk cancel").await;
    let done = finished(&world).await;
    assert!(done.cancelled);
    assert_eq!(done.generated, 0);
    world.shutdown().await;
}

struct Platform;

impl ChunkGenerator for Platform {
    fn generate(&self, _: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        let mut chunk = tuff_world_model::chunk::Chunk::empty(shape);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, 64, z, Block::Stone.default_state());
            }
        }
        chunk
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 65, 0)
    }
}

fn platformed(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    if settings.key == OVERWORLD {
        Arc::new(Platform)
    } else {
        tuff_test_support::placeholder::for_settings(settings, seed)
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generating_and_regenerating_use_the_dimension_generator() {
    let dir = TestDir::new();
    let world =
        TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), platformed);
    let region = world.region(RegionPos::new(0, 0)).await;
    let stone = Some(Block::Stone.default_state());
    for line in [
        "tuff chunk generate 40 24 16 square",
        "tuff chunk generate 40 24 16 square --force-regenerate",
    ] {
        console(&world, line).await;
        let done = finished(&world).await;
        assert_eq!((done.generated, done.failed), (9, 0), "{line}");
        for pos in [BlockPos::new(24, 64, 24), BlockPos::new(40, 64, 40)] {
            assert_eq!(read_block(&region, pos).await, stone, "{line} {pos:?}");
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runtime_dimension_regenerates_with_its_base_generator() {
    let dir = TestDir::new();
    let world =
        TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), platformed);
    let base = world.dimension(OVERWORLD);
    let id = world
        .shared
        .dimensions
        .create("test:arena", base, "test")
        .unwrap();
    let stone = Some(Block::Stone.default_state());
    for regenerate in [false, true] {
        let spec = crate::world::pregen::JobSpec {
            dimension: id,
            area: Area::new(40, 40, 16, Shape::Square),
            regenerate,
            by: "test".to_owned(),
        };
        world
            .shared
            .pregen
            .start(&world.shared, spec)
            .await
            .unwrap();
        let done = finished(&world).await;
        assert_eq!((done.generated, done.failed), (9, 0), "{regenerate}");
        let region = world.region_in("test:arena", RegionPos::new(0, 0)).await;
        assert_eq!(
            read_block(&region, BlockPos::new(40, 64, 40)).await,
            stone,
            "{regenerate}"
        );
    }
    world.shutdown().await;
}
