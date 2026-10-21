use std::sync::Mutex;
use std::time::Duration;
use tuff_core::storage::codec::WorldBackendExt as _;

use tuff_vanilla_data::Block;

use super::*;
use crate::chunk_generator::StagedGenerator;

use tuff_core::world::dimension::DimensionId;
use tuff_core::world::generator::ChunkPipeline;
use tuff_core::world::generator::{URGENT, VIEWED};

const SHAPE: ChunkShape = ChunkShape::new(0, 16);

/// Decoration writes one stone into each chunk of its window, at an x
/// that says which neighbour wrote it, and checks no two running jobs
/// share a chunk.
#[derive(Default)]
struct Marker {
    busy: Mutex<HashSet<ChunkPos>>,
    decorated: Mutex<HashMap<ChunkPos, u32>>,
    starts: Mutex<HashMap<(i32, i32), u32>>,
    shrunk: std::sync::atomic::AtomicUsize,
}

impl StagedGenerator for Marker {
    fn starts(&self, block: (i32, i32), _: ChunkShape) -> StartsBlock {
        *self.starts.lock().unwrap().entry(block).or_default() += 1;
        StartsBlock {
            block,
            starts: Vec::new(),
        }
    }

    fn terrain(&self, pos: ChunkPos, shape: ChunkShape, starts: &[Arc<StartsBlock>]) -> ProtoChunk {
        let given: HashSet<(i32, i32)> = starts.iter().map(|b| b.block).collect();
        let expected: HashSet<(i32, i32)> = StartsBlock::around(pos).collect();
        assert_eq!(given, expected, "terrain of {pos:?} got the wrong starts");
        let mut proto = ProtoChunk::new(pos, shape);
        proto.finish_terrain();
        proto
    }

    fn decorate(&self, center: ChunkPos, mut window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        {
            let mut busy = self.busy.lock().unwrap();
            for chunk in &window {
                assert!(busy.insert(chunk.pos), "{:?} is in two windows", chunk.pos);
            }
        }
        *self.decorated.lock().unwrap().entry(center).or_default() += 1;
        std::thread::sleep(Duration::from_millis(1));
        for chunk in &mut window {
            let (dx, dz) = (center.x - chunk.pos.x, center.z - chunk.pos.z);
            chunk.set(dx + 1 + 3 * (dz + 1), 0, 0, Block::Stone.default_state());
        }
        let mut busy = self.busy.lock().unwrap();
        for chunk in &window {
            busy.remove(&chunk.pos);
        }
        window
    }

    fn finish(&self, proto: ProtoChunk) -> Finished {
        Finished::new(proto.into_chunk())
    }

    fn shrink(&self) {
        self.shrunk
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Wants every chunk of `rings` (squares around `center`, one after
/// another) from a pipeline of `shards` shards and checks that every
/// finished chunk carries each neighbour's decoration, that no chunk
/// is decorated twice and that structure starts are computed once.
async fn every_decoration_once(shards: usize, center: ChunkPos, rings: &[i32]) {
    every_decoration_once_limited(shards, center, rings, None).await;
}

async fn every_decoration_once_limited(
    shards: usize,
    center: ChunkPos,
    rings: &[i32],
    max_decorating: Option<usize>,
) {
    let marker = Arc::new(Marker::default());
    let (_clock_tx, clock) = watch::channel(0);
    let pipeline = GenerationActor::spawn_with(
        marker.clone(),
        PipelineContext {
            shape: SHAPE,
            clock,
            storage: None,
            tags: Default::default(),
        },
        shards,
        Limits {
            decorating: max_decorating,
            committed: None,
        },
    );
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let mut finished = HashMap::new();
    for &radius in rings {
        let wanted: Vec<ChunkPos> = within(center, radius)
            .filter(|pos| !finished.contains_key(pos))
            .collect();
        for pos in &wanted {
            assert!(pipeline.want(*pos, region.clone(), 0));
        }
        for (pos, chunk) in receive(&mut inbox, wanted.len()).await {
            assert!(finished.insert(pos, chunk).is_none(), "{pos:?} twice");
        }
    }
    for (pos, chunk) in &finished {
        for x in 0..9 {
            assert_eq!(
                chunk.get(x, 0, 0).block(),
                Block::Stone,
                "{pos:?} misses the decoration of neighbour {x}"
            );
        }
    }
    let outer = rings.iter().max().copied().unwrap_or(0) + 1;
    let counts = marker.decorated.lock().unwrap();
    assert!(counts.values().all(|n| *n == 1), "{counts:?}");
    assert_eq!(counts.len(), ((2 * outer + 1) * (2 * outer + 1)) as usize);
    let starts = marker.starts.lock().unwrap();
    assert!(
        starts.values().all(|n| *n == 1),
        "structure starts computed more than once: {starts:?}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn finished_chunks_carry_every_neighbours_decoration_once() {
    every_decoration_once(1, ChunkPos::new(3, 3), &[1, 3]).await;
}

/// Windows across the squares of several shards borrow chunks from
/// each other without decorating anything twice or deadlocking.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn shards_share_windows_across_their_squares() {
    for shards in [2, 3, 7] {
        every_decoration_once(shards, ChunkPos::new(0, 0), &[2, 5, 11]).await;
    }
}

/// With room for one decoration at a time per shard, the needed ones
/// wait in line and every wanted chunk still finishes, decorated once.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn decorations_beyond_the_limit_wait_in_line() {
    for shards in [1, 3] {
        every_decoration_once_limited(shards, ChunkPos::new(0, 0), &[2, 4], Some(1)).await;
    }
}

fn lone_shard() -> Shard {
    let (_clock_tx, clock) = watch::channel(0);
    let (tx, _rx) = mpsc::unbounded_channel();
    let routing = Arc::new(Routing {
        shards: vec![tx.downgrade()],
        started: Instant::now(),
    });
    let (jobs_tx, _jobs) = mpsc::unbounded_channel();
    let cx = PipelineContext {
        shape: SHAPE,
        clock,
        storage: None,
        tags: Default::default(),
    };
    Shard::new(0, routing, Arc::new(Marker::default()), &cx, jobs_tx, 0)
}

fn want(shard: &mut Shard, pos: ChunkPos, region: &RegionHandle, priority: u32) {
    shard.on_message(ShardMessage::Want {
        pos,
        reply: region.clone(),
        priority,
    });
    shard.commit();
    while let Some(message) = shard.local.pop_front() {
        shard.on_message(message);
    }
}

/// Withdrawing the only request for a chunk drops everything that
/// waited on it: the request, its needed decorations and their places
/// in line. Decorations already started still finish.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_withdrawn_want_releases_what_waited_on_it() {
    let mut shard = lone_shard();
    shard.max_decorating = 1;
    let (region, _requests, _inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let pos = ChunkPos::new(0, 0);
    want(&mut shard, pos, &region, VIEWED);
    assert_eq!(shard.needed.len(), 9);
    assert_eq!(shard.decorating.len(), 1);
    assert_eq!(shard.queued.len(), 8);
    shard.on_message(ShardMessage::Unwant {
        pos,
        region: region.pos,
    });
    assert!(shard.wanted.is_empty() && shard.priorities.is_empty());
    assert!(shard.needed.is_empty(), "{:?}", shard.needed);
    assert!(shard.queued.is_empty());
    assert_eq!(shard.decorating.len(), 1, "the started decoration goes on");
    shard.admit();
    assert_eq!(shard.decorating.len(), 1, "nothing withdrawn is started");
}

/// A need shared with another wanted chunk survives withdrawing one of
/// them.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_withdrawn_want_keeps_needs_others_share() {
    let mut shard = lone_shard();
    shard.max_decorating = 0;
    let (region, _requests, _inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    want(&mut shard, ChunkPos::new(0, 0), &region, VIEWED);
    want(&mut shard, ChunkPos::new(2, 0), &region, VIEWED);
    assert_eq!(shard.needed.len(), 15);
    shard.on_message(ShardMessage::Unwant {
        pos: ChunkPos::new(0, 0),
        region: region.pos,
    });
    let mut needed: Vec<ChunkPos> = shard.needed.keys().copied().collect();
    needed.sort();
    let mut expected: Vec<ChunkPos> = around(ChunkPos::new(2, 0)).collect();
    expected.sort();
    assert_eq!(needed, expected);
}

/// Urgent requests (a command or teleport waiting) jump the line;
/// requests of equal priority are served oldest first.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn urgent_wants_jump_the_line_and_equals_go_oldest_first() {
    let mut shard = lone_shard();
    shard.max_decorating = 0;
    let (region, _requests, _inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let viewed = ChunkPos::new(0, 0);
    let later = ChunkPos::new(10, 0);
    let urgent = ChunkPos::new(20, 0);
    want(&mut shard, viewed, &region, VIEWED);
    want(&mut shard, later, &region, VIEWED);
    want(&mut shard, urgent, &region, URGENT);
    let mut order = Vec::new();
    while let Some((_, Reverse(seq), center)) = shard.queue.pop() {
        if shard
            .queued
            .get(&center)
            .is_some_and(|(_, latest)| *latest == seq)
        {
            order.push(center);
        }
    }
    let near = |center: &ChunkPos, pos: ChunkPos| around(pos).any(|n| n == *center);
    assert!(order[..9].iter().all(|c| near(c, urgent)), "{order:?}");
    assert!(order[9..18].iter().all(|c| near(c, viewed)), "{order:?}");
    assert!(order[18..].iter().all(|c| near(c, later)), "{order:?}");
}

fn committed(shard: &Shard) -> Vec<ChunkPos> {
    let mut committed: Vec<ChunkPos> = shard.committed.keys().copied().collect();
    committed.sort();
    committed
}

/// Only a bounded number of wanted chunks have their decorations
/// asked for; the rest wait until a committed one is delivered or
/// withdrawn. Urgent ones never wait.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn commitments_stay_bounded_except_urgent_ones() {
    let mut shard = lone_shard();
    shard.max_decorating = 0;
    shard.max_committed = 2;
    let (region, _requests, _inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let wanted: Vec<ChunkPos> = (0..4).map(|i| ChunkPos::new(10 * i, 0)).collect();
    for pos in &wanted {
        want(&mut shard, *pos, &region, VIEWED);
    }
    assert_eq!(committed(&shard), wanted[..2]);
    assert_eq!(shard.needed.len(), 18);
    assert_eq!(shard.candidates.len(), 2);
    shard.on_message(ShardMessage::Unwant {
        pos: wanted[0],
        region: region.pos,
    });
    shard.commit();
    assert_eq!(committed(&shard), wanted[1..3]);
    assert_eq!(shard.needed.len(), 18);
    let urgent = ChunkPos::new(100, 0);
    want(&mut shard, urgent, &region, URGENT);
    assert_eq!(shard.committed.len(), 3);
    assert!(shard.committed.contains_key(&urgent));
    assert!(
        shard.committed[&urgent] < shard.committed[&wanted[1]],
        "urgent decorations are served first"
    );
}

/// A waiting chunk whose decorations are mostly asked for already is
/// committed before one that needs all nine, so finished areas grow
/// together instead of scattering; equals go oldest first.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn neighbours_of_committed_chunks_are_committed_first() {
    let mut shard = lone_shard();
    shard.max_decorating = 0;
    shard.max_committed = 1;
    let (region, _requests, _inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let first = ChunkPos::new(0, 0);
    let far = ChunkPos::new(20, 0);
    let farther = ChunkPos::new(40, 0);
    let diagonal = ChunkPos::new(1, 1);
    let beside = ChunkPos::new(1, 0);
    for pos in [first, far, farther, diagonal, beside] {
        want(&mut shard, pos, &region, VIEWED);
    }
    let costs: Vec<(ChunkPos, u8)> = [far, farther, diagonal, beside]
        .iter()
        .map(|pos| (*pos, shard.candidate_keys[pos].1))
        .collect();
    assert_eq!(costs, [(far, 9), (farther, 9), (diagonal, 5), (beside, 3)]);
    let mut order = Vec::new();
    for _ in 0..4 {
        let before = committed(&shard);
        shard.max_committed += 1;
        shard.commit();
        let added: Vec<ChunkPos> = committed(&shard)
            .into_iter()
            .filter(|pos| !before.contains(pos))
            .collect();
        assert_eq!(added.len(), 1);
        order.push(added[0]);
    }
    assert_eq!(order, [beside, diagonal, far, farther]);
}

/// A chunk handed to its region while a stale window request still
/// waits for it must be regenerated, or the shard never goes idle and
/// a flush spins forever.
#[tuff_core::watchdog]
#[test]
fn a_delivered_chunk_a_pending_window_waits_on_is_regenerated() {
    let (_clock_tx, clock) = watch::channel(0);
    let (tx, _rx) = mpsc::unbounded_channel();
    let routing = Arc::new(Routing {
        shards: vec![tx.downgrade()],
        started: Instant::now(),
    });
    let (jobs_tx, _jobs) = mpsc::unbounded_channel();
    let cx = PipelineContext {
        shape: SHAPE,
        clock,
        storage: None,
        tags: Default::default(),
    };
    let mut shard = Shard::new(0, routing, Arc::new(Marker::default()), &cx, jobs_tx, 0);
    let pos = ChunkPos::new(0, 0);
    shard.slots.insert(pos, Slot::new(SlotState::Delivering, 0));
    shard.pending_grants.push(PendingGrant {
        ticket: Ticket { shard: 0, id: 0 },
        positions: vec![pos],
    });
    shard.on_job(JobResult::Delivered(pos));
    assert!(
        matches!(
            shard.slots[&pos].state,
            SlotState::WaitingStarts | SlotState::Generating
        ),
        "a chunk a pending grant needs stays delivered"
    );
}

/// A forget that lands while a chunk is on its way to its region
/// drops it too, and its late delivery report does not touch the
/// slot a new want made since.
#[tuff_core::watchdog]
#[test]
fn a_forget_drops_a_chunk_whose_delivery_is_in_flight() {
    let mut shard = lone_shard();
    let pos = ChunkPos::new(0, 0);
    let mut slot = Slot::new(SlotState::Delivering, 0);
    slot.decorated = true;
    shard.slots.insert(pos, slot);
    shard.on_message(ShardMessage::Forget {
        area: Arc::new(|_| true),
        done: oneshot::channel().0,
    });
    assert!(!shard.slots.contains_key(&pos));
    shard.slots.insert(pos, Slot::new(SlotState::Lent, 0));
    shard.on_job(JobResult::Delivered(pos));
    assert!(matches!(shard.slots[&pos].state, SlotState::Lent));
}

/// A committed chunk whose neighbours were decorated when it was
/// asked for has them decorated again once a forget dropped them, or
/// it is never delivered.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn a_forget_renews_the_decorations_committed_chunks_wait_on() {
    let mut shard = lone_shard();
    let (region, _requests, _inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let pos = ChunkPos::new(0, 0);
    for neighbour in around(pos) {
        let state = if neighbour == pos {
            SlotState::Generating
        } else {
            SlotState::Delivered
        };
        let mut slot = Slot::new(state, 0);
        slot.decorated = true;
        shard.slots.insert(neighbour, slot);
    }
    want(&mut shard, pos, &region, VIEWED);
    assert!(shard.committed.contains_key(&pos));
    assert!(shard.needed.is_empty());

    shard.on_message(ShardMessage::Forget {
        area: Arc::new(|_| true),
        done: oneshot::channel().0,
    });
    shard.on_message(ShardMessage::Renew {
        done: oneshot::channel().0,
    });
    while let Some(message) = shard.local.pop_front() {
        shard.on_message(message);
    }
    let mut needed: Vec<ChunkPos> = shard.needed.keys().copied().collect();
    needed.sort_by_key(|p| (p.x, p.z));
    let mut expected: Vec<ChunkPos> = around(pos).filter(|n| *n != pos).collect();
    expected.sort_by_key(|p| (p.x, p.z));
    assert_eq!(needed, expected);
}

async fn receive(
    inbox: &mut mpsc::UnboundedReceiver<RegionInternal>,
    count: usize,
) -> HashMap<ChunkPos, Chunk> {
    let mut finished = HashMap::new();
    while finished.len() < count {
        let message = tokio::time::timeout(Duration::from_secs(20), inbox.recv())
            .await
            .expect("chunks arrive")
            .expect("pipeline alive");
        if let RegionInternal::Generated {
            pos,
            finished: done,
        } = message
        {
            assert!(finished.insert(pos, done.chunk).is_none(), "{pos:?} twice");
        }
    }
    finished
}

/// Chunks in progress survive the pipeline: a flush saves them, a new
/// pipeline over the same storage reads them back, and no chunk is
/// decorated twice or loses a neighbour's decoration.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn chunks_in_progress_survive_a_restart() {
    for shards in [1, 3] {
        survive_a_restart(shards).await;
    }
}

async fn survive_a_restart(shards: usize) {
    use tuff_core::world::chunk_io::ChunkIo;
    use tuff_core::world::generator::ProtoStorage;
    use tuff_storage::backend::WorldBackend;
    use tuff_storage::memory::MemoryBackend;

    let backend: Arc<dyn WorldBackend> = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let marker = Arc::new(Marker::default());
    let (_clock_tx, clock) = watch::channel(0);
    let start = || {
        GenerationActor::spawn_sharded(
            marker.clone(),
            PipelineContext {
                shape: SHAPE,
                clock: clock.clone(),
                storage: Some(ProtoStorage::new(
                    io.clone(),
                    "minecraft:overworld",
                    backend.chunk_codec(),
                )),
                tags: Default::default(),
            },
            shards,
        )
    };
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let first = start();
    assert!(first.want(ChunkPos::new(7, 3), region.clone(), 0));
    let delivered = receive(&mut inbox, 1).await;
    let codec = backend.chunk_codec();
    let blobs = delivered
        .iter()
        .map(|(pos, chunk)| {
            let meta = tuff_world_model::chunk_meta::ChunkMeta::generated();
            (*pos, codec.encode_chunk(chunk, &meta, *pos, 0).unwrap())
        })
        .collect();
    ProtoStorage::new(io.clone(), "minecraft:overworld", codec.clone())
        .write(blobs)
        .await
        .unwrap();
    let (done, flushed) = oneshot::channel();
    first.flush(done);
    flushed.await.expect("flush answers");
    drop(first);
    assert_eq!(marker.decorated.lock().unwrap().len(), 9);

    let second = start();
    let wanted: Vec<ChunkPos> = within(ChunkPos::new(9, 3), 1).collect();
    for pos in &wanted {
        assert!(second.want(*pos, region.clone(), 0));
    }
    let finished = receive(&mut inbox, wanted.len()).await;
    for (pos, chunk) in &finished {
        for x in 0..9 {
            assert_eq!(
                chunk.get(x, 0, 0).block(),
                Block::Stone,
                "{pos:?} misses the decoration of neighbour {x}"
            );
        }
    }
    let counts = marker.decorated.lock().unwrap();
    assert!(counts.values().all(|n| *n == 1), "{counts:?}");
    assert_eq!(counts.len(), 9 + 5 * 5 - 2 * 3, "{counts:?}");
}

/// Chunks a regenerating job cleared from storage and had the
/// pipeline forget come back fresh, with every neighbour decorating
/// them again.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn forgotten_chunks_are_generated_and_decorated_afresh() {
    for shards in [1, 3] {
        regenerate_after_forgetting(shards).await;
    }
}

async fn regenerate_after_forgetting(shards: usize) {
    use tuff_core::world::chunk_io::{ChunkIo, IoOp};
    use tuff_core::world::generator::ProtoStorage;
    use tuff_storage::backend::WorldBackend;
    use tuff_storage::memory::MemoryBackend;

    let backend: Arc<dyn WorldBackend> = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let marker = Arc::new(Marker::default());
    let (_clock_tx, clock) = watch::channel(0);
    let codec = backend.chunk_codec();
    let pipeline = GenerationActor::spawn_sharded(
        marker.clone(),
        PipelineContext {
            shape: SHAPE,
            clock,
            storage: Some(ProtoStorage::new(
                io.clone(),
                "minecraft:overworld",
                codec.clone(),
            )),
            tags: Default::default(),
        },
        shards,
    );
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let centre = ChunkPos::new(7, 3);
    assert!(pipeline.want(centre, region.clone(), 0));
    let delivered = receive(&mut inbox, 1).await;
    let meta = tuff_world_model::chunk_meta::ChunkMeta::generated();
    let blob = codec
        .encode_chunk(&delivered[&centre], &meta, centre, 0)
        .unwrap();
    ProtoStorage::new(io.clone(), "minecraft:overworld", codec.clone())
        .write(vec![(centre, blob)])
        .await
        .unwrap();

    let removes = within(centre, 2).map(IoOp::Remove).collect();
    io.submit("minecraft:overworld", removes).await.unwrap();
    let (done, forgotten) = oneshot::channel();
    pipeline.forget(Arc::new(move |pos| pos.distance(centre) <= 2), done);
    forgotten.await.expect("forget answers");

    assert!(pipeline.want(centre, region.clone(), 0));
    let again = receive(&mut inbox, 1).await;
    for x in 0..9 {
        assert_eq!(
            again[&centre].get(x, 0, 0).block(),
            Block::Stone,
            "the regenerated chunk misses the decoration of neighbour {x}"
        );
    }
    let counts = marker.decorated.lock().unwrap();
    for pos in around(centre) {
        assert_eq!(counts.get(&pos), Some(&2), "{pos:?} with {shards} shards");
    }
}

/// Going idle drops delivered chunks and structure starts and saves
/// chunks in progress; work asked for afterwards reads them back and
/// computes starts again, and still decorates nothing twice.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_pipeline_drops_what_it_can_rebuild() {
    for shards in [1, 3] {
        idle_and_resume(shards).await;
    }
}

async fn idle_and_resume(shards: usize) {
    use tuff_core::world::chunk_io::ChunkIo;
    use tuff_core::world::generator::ProtoStorage;
    use tuff_storage::backend::WorldBackend;
    use tuff_storage::memory::MemoryBackend;

    let backend: Arc<dyn WorldBackend> = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let marker = Arc::new(Marker::default());
    let (_clock_tx, clock) = watch::channel(0);
    let codec = backend.chunk_codec();
    let pipeline = GenerationActor::spawn_sharded(
        marker.clone(),
        PipelineContext {
            shape: SHAPE,
            clock,
            storage: Some(ProtoStorage::new(
                io.clone(),
                "minecraft:overworld",
                codec.clone(),
            )),
            tags: Default::default(),
        },
        shards,
    );
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    assert!(pipeline.want(ChunkPos::new(7, 3), region.clone(), 0));
    let delivered = receive(&mut inbox, 1).await;
    let blobs = delivered
        .iter()
        .map(|(pos, chunk)| {
            let meta = tuff_world_model::chunk_meta::ChunkMeta::generated();
            (*pos, codec.encode_chunk(chunk, &meta, *pos, 0).unwrap())
        })
        .collect();
    ProtoStorage::new(io.clone(), "minecraft:overworld", codec.clone())
        .write(blobs)
        .await
        .unwrap();
    let (done, idle) = oneshot::channel();
    pipeline.idle(done);
    idle.await.expect("idle answers");
    assert_eq!(
        marker.shrunk.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one shard shrinks the generator"
    );

    let wanted: Vec<ChunkPos> = within(ChunkPos::new(9, 3), 1).collect();
    for pos in &wanted {
        assert!(pipeline.want(*pos, region.clone(), 0));
    }
    let finished = receive(&mut inbox, wanted.len()).await;
    for (pos, chunk) in &finished {
        for x in 0..9 {
            assert_eq!(
                chunk.get(x, 0, 0).block(),
                Block::Stone,
                "{pos:?} misses the decoration of neighbour {x}"
            );
        }
    }
    let counts = marker.decorated.lock().unwrap();
    assert!(counts.values().all(|n| *n == 1), "{counts:?}");
    let starts = marker.starts.lock().unwrap();
    assert!(
        starts.values().any(|n| *n == 2),
        "starts were dropped and computed again: {starts:?}"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_flush_without_storage_answers() {
    let (_clock_tx, clock) = watch::channel(0);
    let pipeline = GenerationActor::spawn(
        Arc::new(Marker::default()),
        PipelineContext {
            shape: SHAPE,
            clock,
            storage: None,
            tags: Default::default(),
        },
    );
    let (done, flushed) = oneshot::channel();
    pipeline.flush(done);
    flushed.await.expect("flush answers");
}
