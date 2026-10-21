//! Chunks and regions follow their viewers: what nobody views any more is
//! unloaded, regions without tickets close, and generation nobody waits
//! for is withdrawn, so memory stays flat while players travel.

use super::*;
use crate::world::generator::{ChunkPipeline, Finished, PipelineContext};
use crate::world::messages::{ManagerMessage, RegionInternal};

/// The 3x3 chunks around the middle of `region`.
fn area(region: RegionPos) -> Vec<ChunkPos> {
    let middle = ChunkPos::new(
        region.x * tuff_world_model::pos::REGION_CHUNKS + 16,
        region.z * tuff_world_model::pos::REGION_CHUNKS + 16,
    );
    (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| ChunkPos::new(middle.x + dx, middle.z + dz)))
        .collect()
}

async fn totals(world: &TestWorld) -> (u64, u64) {
    let snapshot = world.shared.metrics.collect().await;
    let regions = snapshot.manager.as_ref().map_or(0, |m| m.regions);
    (snapshot.world.chunks, regions)
}

async fn chunks_of(world: &TestWorld, region: RegionPos) -> u64 {
    let snapshot = world.shared.metrics.collect().await;
    let mut reports: Vec<_> = snapshot.slowest_regions.clone();
    reports.retain(|r| r.x == region.x && r.z == region.z);
    assert!(reports.len() <= 1);
    reports.first().map_or(0, |r| r.chunks)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn loaded_chunks_and_regions_drop_back_once_the_viewer_moves_away() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let overworld = world.dimension(OVERWORLD);
    let (viewer, mut events) = PlayerHandle::new(1000);
    let home = RegionPos::new(0, 0);
    let away = RegionPos::new(3, 0);

    let first = world.region(home).await;
    subscribe(&first, &viewer, &mut events, &area(home)).await;
    world.ticks(20).await;
    assert_eq!(totals(&world).await, (9, 1));

    let second = world.region(away).await;
    subscribe(&second, &viewer, &mut events, &area(away)).await;
    first
        .request(RegionRequest::Unsubscribe {
            player: viewer.id,
            chunks: area(home),
        })
        .await;
    world
        .manager
        .send(ManagerMessage::Release {
            dimension: overworld,
            region: home,
        })
        .await
        .unwrap();
    world.ticks(20).await;
    assert_eq!(chunks_of(&world, home).await, 9, "kept a while");

    world.ticks(140).await;
    assert_eq!(chunks_of(&world, home).await, 0, "unloaded once unviewed");
    assert_eq!(chunks_of(&world, away).await, 9);
    assert_eq!(totals(&world).await.1, 2, "the region waits before closing");

    world.ticks(600).await;
    assert_eq!(totals(&world).await.1, 1, "the unreferenced region closed");
    assert!(
        stored_chunk(dir.path(), area(home)[4], false).is_some(),
        "unloaded chunks were saved"
    );
    world.shutdown().await;
}

enum Asked {
    Want(ChunkPos, RegionHandle),
    Unwant(ChunkPos),
}

fn asked() -> &'static std::sync::Mutex<Option<mpsc::UnboundedSender<Asked>>> {
    static ASKED: std::sync::OnceLock<std::sync::Mutex<Option<mpsc::UnboundedSender<Asked>>>> =
        std::sync::OnceLock::new();
    ASKED.get_or_init(Default::default)
}

/// A pipeline that only reports what regions ask of it.
struct Recording;

impl ChunkPipeline for Recording {
    fn want(&self, pos: ChunkPos, reply: RegionHandle, _priority: u32) -> bool {
        if let Some(tx) = asked().lock().unwrap().as_ref() {
            let _ = tx.send(Asked::Want(pos, reply));
        }
        true
    }

    fn unwant(&self, pos: ChunkPos, _region: RegionPos) {
        if let Some(tx) = asked().lock().unwrap().as_ref() {
            let _ = tx.send(Asked::Unwant(pos));
        }
    }
}

struct Staged;

impl ChunkGenerator for Staged {
    fn generate(&self, _: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        tuff_world_model::chunk::Chunk::empty(shape)
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 61, 0)
    }

    fn pipeline(self: Arc<Self>, _: PipelineContext) -> Option<Arc<dyn ChunkPipeline>> {
        Some(Arc::new(Recording))
    }
}

fn recording(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    if settings.key == OVERWORLD {
        Arc::new(Staged)
    } else {
        tuff_test_support::placeholder::for_settings(settings, seed)
    }
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Asked>) -> Asked {
    tokio::time::timeout(TIMEOUT, rx.recv())
        .await
        .expect("the region asks the pipeline")
        .expect("open")
}

/// A viewer leaving chunks still being generated withdraws them from the
/// pipeline; one finished anyway is installed and saved, not lost.
#[tuff_core::watchdog]
#[tokio::test]
async fn generation_nobody_waits_for_is_withdrawn() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    *asked().lock().unwrap() = Some(tx);
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), recording);
    let home = RegionPos::new(0, 0);
    let region = world.region(home).await;
    let (viewer, _events) = PlayerHandle::new(1000);
    let chunks = area(home);
    region
        .request(RegionRequest::Subscribe {
            player: viewer.clone(),
            chunks: chunks.clone(),
        })
        .await;
    let mut wanted = HashSet::new();
    let mut reply = None;
    while wanted.len() < chunks.len() {
        if let Asked::Want(pos, handle) = next(&mut rx).await {
            wanted.insert(pos);
            reply = Some(handle);
        }
    }
    region
        .request(RegionRequest::Unsubscribe {
            player: viewer.id,
            chunks: chunks.clone(),
        })
        .await;
    world.clock.send_modify(|tick| *tick = 20);
    world.server_ticks.send_modify(|tick| *tick = 20);
    let mut withdrawn = HashSet::new();
    while withdrawn.len() < chunks.len() {
        if let Asked::Unwant(pos) = next(&mut rx).await {
            withdrawn.insert(pos);
        }
    }
    assert_eq!(withdrawn, wanted);

    let late = chunks[4];
    let mut chunk = tuff_world_model::chunk::Chunk::empty(ChunkShape::OVERWORLD);
    chunk.set(0, 0, 0, Block::Stone.default_state());
    reply.expect("asked").internal(RegionInternal::Generated {
        pos: late,
        finished: Box::new(Finished::new(chunk)),
    });
    world.ticks(1).await;
    let at = |pos: ChunkPos| BlockPos::new(pos.x * 16, 0, pos.z * 16);
    assert_eq!(
        block_at(&region, at(late)).await.map(|s| s.block()),
        Some(Block::Stone),
        "a chunk delivered after its withdrawal is installed"
    );
    assert_eq!(block_at(&region, at(chunks[0])).await, None);
    world.ticks(140).await;
    assert_eq!(block_at(&region, at(late)).await, None, "and unloads");
    world.shutdown().await;
    assert!(stored_chunk(dir.path(), late, false).is_some(), "saved");
    *asked().lock().unwrap() = None;
}

async fn chunk_packet(
    region: &RegionHandle,
    viewer: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    pos: ChunkPos,
) -> bytes::Bytes {
    region
        .request(RegionRequest::Subscribe {
            player: viewer.clone(),
            chunks: vec![pos],
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::Chunk {
            pos: at, packet, ..
        } if at == pos => Some(packet),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test]
async fn idle_chunk_packets_are_dropped_and_encoded_again() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let home = RegionPos::new(0, 0);
    let region = world.region(home).await;
    let (first, mut first_events) = PlayerHandle::new(1000);
    subscribe(&region, &first, &mut first_events, &area(home)).await;
    let middle = area(home)[4];

    let (second, mut second_events) = PlayerHandle::new(1001);
    let cached = chunk_packet(&region, &second, &mut second_events, middle).await;
    let (third, mut third_events) = PlayerHandle::new(1002);
    let shared = chunk_packet(&region, &third, &mut third_events, middle).await;
    assert_eq!(cached.as_ptr(), shared.as_ptr(), "a fresh packet is shared");

    world.ticks(140).await;
    let (fourth, mut fourth_events) = PlayerHandle::new(1003);
    let again = chunk_packet(&region, &fourth, &mut fourth_events, middle).await;
    assert_ne!(
        again.as_ptr(),
        cached.as_ptr(),
        "an idle packet was dropped"
    );
    assert_eq!(again, cached, "and encoded again unchanged");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_mirror_only_viewer_gets_blocks_changed_after_the_last_encode() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let home = RegionPos::new(0, 0);
    let region = world.region(home).await;
    let (first, mut first_events) = PlayerHandle::new(1000);
    subscribe(&region, &first, &mut first_events, &area(home)).await;
    let middle = area(home)[4];
    let spot = BlockPos::new(middle.x * 16 + 3, 70, middle.z * 16 + 5);
    let gold = Block::GoldBlock.default_state();
    place_blocks(&world, &region, [(spot, gold)]).await;
    assert_eq!(block_at(&region, spot).await, Some(gold));

    let (mut fake, mut fake_events) = PlayerHandle::new(1001);
    fake.mirror_only = true;
    region
        .request(RegionRequest::Subscribe {
            player: fake.clone(),
            chunks: vec![middle],
        })
        .await;
    let snapshot = expect(&mut fake_events, |event| match event {
        PlayerEvent::Chunk { pos, snapshot, .. } if pos == middle => Some(snapshot),
        _ => None,
    })
    .await
    .expect("a mirror-only viewer gets the chunk's contents");
    assert_eq!(snapshot.get(spot.x, spot.y, spot.z), gold);
    world.shutdown().await;
}

static IDLED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A pipeline that only counts how often the world went idle.
struct IdleCounting;

impl ChunkPipeline for IdleCounting {
    fn want(&self, _: ChunkPos, _: RegionHandle, _: u32) -> bool {
        true
    }

    fn idle(&self, done: oneshot::Sender<()>) {
        IDLED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = done.send(());
    }
}

struct IdleStaged;

impl ChunkGenerator for IdleStaged {
    fn generate(&self, _: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        tuff_world_model::chunk::Chunk::empty(shape)
    }

    fn sea_level(&self) -> i32 {
        63
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, 61, 0)
    }

    fn pipeline(self: Arc<Self>, _: PipelineContext) -> Option<Arc<dyn ChunkPipeline>> {
        Some(Arc::new(IdleCounting))
    }
}

fn idle_counting(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    if settings.key == OVERWORLD {
        Arc::new(IdleStaged)
    } else {
        tuff_test_support::placeholder::for_settings(settings, seed)
    }
}

async fn idled_at_least(world: &TestWorld, count: usize) {
    for _ in 0..200 {
        if IDLED.load(std::sync::atomic::Ordering::SeqCst) >= count {
            return;
        }
        world.ticks(1).await;
    }
    panic!("the world never went idle {count} times");
}

/// Once the last region has closed the world tells every generation
/// pipeline it is idle, once per stretch of idleness.
#[tuff_core::watchdog]
#[tokio::test]
async fn pipelines_hear_when_the_last_region_closes() {
    let dir = TestDir::new();
    let world =
        TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), idle_counting);
    idled_at_least(&world, 1).await;
    let home = RegionPos::new(0, 0);
    world.region(home).await;
    world.ticks(40).await;
    assert_eq!(IDLED.load(std::sync::atomic::Ordering::SeqCst), 1);
    world
        .manager
        .send(ManagerMessage::Release {
            dimension: world.dimension(OVERWORLD),
            region: home,
        })
        .await
        .unwrap();
    world.ticks(600).await;
    idled_at_least(&world, 2).await;
    world.ticks(40).await;
    assert_eq!(IDLED.load(std::sync::atomic::Ordering::SeqCst), 2);
    world.shutdown().await;
}

async fn open_region_files(world: &TestWorld) -> u64 {
    let snapshot = world.shared.metrics.collect().await;
    snapshot.chunk_io.as_ref().map_or(0, |io| io.open_files)
}

/// An idle world closes the region files its regions left open; chunks
/// read back from them afterwards are intact.
#[tuff_core::watchdog]
#[tokio::test]
async fn an_idle_world_closes_its_region_files() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let home = RegionPos::new(0, 0);
    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = world.region(home).await;
    subscribe(&region, &viewer, &mut events, &area(home)).await;
    let middle = BlockPos::new(area(home)[4].x * 16, 70, area(home)[4].z * 16);
    place_blocks(
        &world,
        &region,
        [(middle, Block::GoldBlock.default_state())],
    )
    .await;
    region
        .request(RegionRequest::Unsubscribe {
            player: viewer.id,
            chunks: area(home),
        })
        .await;
    world
        .manager
        .send(ManagerMessage::Release {
            dimension: world.dimension(OVERWORLD),
            region: home,
        })
        .await
        .unwrap();
    world.ticks(160).await;
    assert!(open_region_files(&world).await > 0, "saving opened a file");
    let mut open = u64::MAX;
    for _ in 0..80 {
        world.ticks(20).await;
        open = open_region_files(&world).await;
        if open == 0 {
            break;
        }
    }
    assert_eq!(open, 0, "idle files closed");
    let region = world.region(home).await;
    subscribe(&region, &viewer, &mut events, &area(home)).await;
    assert_eq!(
        block_at(&region, middle).await.map(|s| s.block()),
        Some(Block::GoldBlock)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn lit_only_holds_keep_chunks_loaded_without_sending_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let home = RegionPos::new(0, 0);
    let region = world.region(home).await;
    let chunks = area(home);

    region
        .request(RegionRequest::HoldLit {
            player: viewer.id,
            chunks: chunks.clone(),
        })
        .await;
    world.ticks(160).await;
    assert_eq!(chunks_of(&world, home).await, 9, "held chunks stay loaded");
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::Chunk { .. }),
            "a held chunk was sent"
        );
    }

    subscribe(&region, &viewer, &mut events, &chunks[4..5]).await;
    region
        .request(RegionRequest::HoldLit {
            player: viewer.id,
            chunks: chunks[4..5].to_vec(),
        })
        .await;
    region
        .request(RegionRequest::Unsubscribe {
            player: viewer.id,
            chunks: chunks.clone(),
        })
        .await;
    world.ticks(160).await;
    assert_eq!(chunks_of(&world, home).await, 0, "released holds unload");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn entities_in_hold_only_chunks_wait_until_the_chunk_is_viewed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = world.region(RegionPos::new(0, 0)).await;
    let chunks = area(RegionPos::new(0, 0));

    region
        .request(RegionRequest::HoldLit {
            player: viewer.id,
            chunks: chunks.clone(),
        })
        .await;
    world.ticks(40).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
            position: [
                f64::from(chunks[0].x) * 16.0 + 8.5,
                200.0,
                f64::from(chunks[0].z) * 16.0 + 8.5,
            ],
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    world.ticks(20).await;
    let held = super::fluids::item_position(&region).await.1;
    assert_eq!(held[1], 200.0, "a held-only chunk does not tick entities");

    subscribe(&region, &viewer, &mut events, &chunks[0..1]).await;
    world.ticks(20).await;
    let viewed = super::fluids::item_position(&region).await.1;
    assert!(viewed[1] < 200.0, "a viewed chunk ticks entities");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn peeking_never_makes_chunks_but_follows_those_made_for_others() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (spectator, mut seen) = PlayerHandle::new(1000);
    let (walker, mut walked) = PlayerHandle::new(1001);
    let home = RegionPos::new(0, 0);
    let region = world.region(home).await;
    let chunks = area(home);

    region
        .request(RegionRequest::Peek {
            player: spectator.clone(),
            chunks: chunks.clone(),
        })
        .await;
    world.ticks(160).await;
    assert_eq!(chunks_of(&world, home).await, 0, "nothing was made");
    while let Ok(event) = seen.try_recv() {
        assert!(!matches!(event, PlayerEvent::Chunk { .. }));
    }

    subscribe(&region, &walker, &mut walked, &chunks).await;
    world.ticks(20).await;
    assert_eq!(chunks_of(&world, home).await, 9);
    let mut received = 0;
    while let Ok(event) = seen.try_recv() {
        received += usize::from(matches!(event, PlayerEvent::Chunk { .. }));
    }
    assert_eq!(received, 9, "the peeker gets what the walker made");

    let (late, mut late_events) = PlayerHandle::new(1002);
    region
        .request(RegionRequest::Peek {
            player: late,
            chunks: chunks[..1].to_vec(),
        })
        .await;
    world.ticks(2).await;
    let mut got = false;
    while let Ok(event) = late_events.try_recv() {
        got |= matches!(event, PlayerEvent::Chunk { .. });
    }
    assert!(got, "an existing chunk is sent to a peeker");
    world.shutdown().await;
}
