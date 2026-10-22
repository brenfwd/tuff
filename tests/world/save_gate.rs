//! The save gate: `/save-off` keeps changed chunks loaded and unsaved, a
//! flushing save waits until everything is written, and a hold keeps
//! every write away from storage until it is released.

use super::*;
use crate::storage::codec::WorldBackendExt as _;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::storage::codec::ChunkCodec;
use tuff_core::world::chunk_io::{HoldBusy, HoldPhase, SaveState};
use tuff_core::world::messages::ManagerMessage;
use tuff_core::world::save_gate::{DEFAULT_HOLD_LIMIT, HoldError, hold_saves};
use tuff_nbt::Compound;
use tuff_storage::backend::{Documents, RegionStore, WorldBackend, WorldLock};
use tuff_storage::document::{Document, DocumentId};
use tuff_world_model::pos::FileRegionPos;

#[derive(Default)]
struct Writes {
    chunks: AtomicUsize,
    documents: AtomicUsize,
}

/// A backend counting every write that reaches it.
struct Counting {
    inner: Arc<dyn WorldBackend>,
    writes: Arc<Writes>,
}

impl Documents for Counting {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        self.inner.documents().read(document)
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        self.writes.documents.fetch_add(1, Ordering::SeqCst);
        self.inner.documents().write(document, root)
    }

    fn exists(&self, document: Document<'_>) -> bool {
        self.inner.documents().exists(document)
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        self.inner.documents().list()
    }
}

struct CountingStore {
    inner: Box<dyn RegionStore>,
    writes: Arc<Writes>,
}

impl RegionStore for CountingStore {
    fn read_chunk(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_chunk(pos)
    }

    fn write_chunk(&mut self, pos: ChunkPos, blob: &[u8]) -> io::Result<()> {
        self.writes.chunks.fetch_add(1, Ordering::SeqCst);
        self.inner.write_chunk(pos, blob)
    }

    fn remove_chunk(&mut self, pos: ChunkPos) -> io::Result<()> {
        self.writes.chunks.fetch_add(1, Ordering::SeqCst);
        self.inner.remove_chunk(pos)
    }

    fn read_entities(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_entities(pos)
    }

    fn write_entities(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        self.writes.chunks.fetch_add(1, Ordering::SeqCst);
        self.inner.write_entities(pos, blob)
    }

    fn read_pois(&mut self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_pois(pos)
    }

    fn write_pois(&mut self, pos: ChunkPos, blob: Option<&[u8]>) -> io::Result<()> {
        self.writes.chunks.fetch_add(1, Ordering::SeqCst);
        self.inner.write_pois(pos, blob)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn list_chunks(&mut self) -> io::Result<Vec<ChunkPos>> {
        self.inner.list_chunks()
    }
}

impl WorldBackend for Counting {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        self.inner.lock()
    }

    fn documents(&self) -> &dyn Documents {
        self
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        Ok(Box::new(CountingStore {
            inner: self.inner.open_region(dimension, region)?,
            writes: self.writes.clone(),
        }))
    }

    fn blob_codec(&self) -> Arc<dyn tuff_storage::backend::BlobCodec> {
        self.inner.blob_codec()
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        self.inner.list_dimensions()
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        self.inner.list_regions(dimension)
    }
}

fn counted(dir: &Path) -> (TestWorld, Arc<Writes>) {
    let writes = Arc::new(Writes::default());
    let backend = Counting {
        inner: tuff_core::storage::Storage::default().open(dir).unwrap(),
        writes: writes.clone(),
    };
    (TestWorld::start_with(Arc::new(backend)), writes)
}

fn chunk_writes(writes: &Writes) -> usize {
    writes.chunks.load(Ordering::SeqCst)
}

fn document_writes(writes: &Writes) -> usize {
    writes.documents.load(Ordering::SeqCst)
}

const POS: ChunkPos = ChunkPos::new(0, 0);

async fn view(
    world: &TestWorld,
    viewer: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
) -> RegionHandle {
    let region = world.region(POS.region()).await;
    subscribe(&region, viewer, events, &[POS]).await;
    region
}

/// Breaks the block `depth` below the top at (1, 1) of `POS`.
async fn dig(
    region: &RegionHandle,
    viewer: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    depth: i32,
) {
    let height = Generator::new(1).height(1, 1);
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: BlockPos::new(1, height - depth, 1),
            tool: None,
            mode: BreakMode::Finish,
            sequence: depth,
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult { .. } => Some(()),
        _ => None,
    })
    .await;
}

async fn leave(region: &RegionHandle, viewer: &PlayerHandle) {
    region
        .request(RegionRequest::Unsubscribe {
            player: viewer.id,
            chunks: vec![POS],
        })
        .await;
}

/// The stored block `depth` below the top at (1, 1), if the chunk is
/// stored.
fn stored_block(dir: &Path, depth: i32) -> Option<Block> {
    let root = stored_chunk(dir, POS, false)?;
    let Decoded::Chunk(chunk, _) = decode_stored(root, POS).unwrap() else {
        panic!("terrain saved");
    };
    let height = Generator::new(1).height(1, 1);
    Some(chunk.get(1, height - depth, 1).block())
}

/// Runs the game until the next autosave tick.
async fn autosave(world: &TestWorld) {
    let tick = *world.clock.borrow();
    world.clock.send_replace(tick.next_multiple_of(6000) - 1);
    world
        .server_ticks
        .send_replace(tick.next_multiple_of(6000) - 1);
    world.ticks(1).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn save_off_keeps_changed_chunks_loaded_until_save_on() {
    let dir = TestDir::new();
    let (world, writes) = counted(dir.path());
    let io = world.shared.chunk_io.get(&world.shared.backend).clone();
    assert!(io.set_autosave(false).await);
    assert!(!io.set_autosave(false).await, "already off");
    assert!(!world.shared.saves().autosaves());

    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = view(&world, &viewer, &mut events).await;
    dig(&region, &viewer, &mut events, 0).await;
    leave(&region, &viewer).await;
    world.ticks(140).await;
    autosave(&world).await;
    assert_eq!(
        chunk_writes(&writes),
        0,
        "nothing saved while saving is off"
    );
    assert_eq!(stored_block(dir.path(), 0), None);

    assert!(io.set_autosave(true).await);
    assert!(!io.set_autosave(true).await, "already on");
    world.ticks(40).await;
    assert!(chunk_writes(&writes) > 0, "the kept chunk saves on unload");
    assert_eq!(stored_block(dir.path(), 0), Some(Block::Air));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn save_all_saves_even_with_saving_off_and_flush_waits_for_the_writes() {
    let dir = TestDir::new();
    let (world, writes) = counted(dir.path());
    let io = world.shared.chunk_io.get(&world.shared.backend).clone();
    assert!(io.set_autosave(false).await);
    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = view(&world, &viewer, &mut events).await;
    dig(&region, &viewer, &mut events, 0).await;

    let (done, flushed) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::SaveAll {
            done: Some(done),
            snapshotted: None,
            epoch: None,
        })
        .await
        .unwrap();
    flushed.await.unwrap();
    assert!(chunk_writes(&writes) > 0);
    assert_eq!(stored_block(dir.path(), 0), Some(Block::Air));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_save_includes_requests_queued_before_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = view(&world, &viewer, &mut events).await;

    let (resume, paused) = std::sync::mpsc::channel::<()>();
    assert!(region.internal(RegionInternal::Run(
        tuff_core::world::region_api::RegionTask::new(move |_| {
            let _ = paused.recv();
        })
    )));
    let height = Generator::new(1).height(1, 1);
    assert!(
        region
            .request(RegionRequest::Break {
                player: viewer.clone(),
                pos: BlockPos::new(1, height, 1),
                tool: None,
                mode: BreakMode::Finish,
                sequence: 0,
            })
            .await
    );
    let (done, flushed) = oneshot::channel();
    assert!(region.internal(RegionInternal::SaveAll {
        done: Some(done),
        snapshotted: None,
        epoch: None,
    }));
    resume.send(()).unwrap();
    flushed.await.unwrap();
    assert_eq!(
        stored_block(dir.path(), 0),
        Some(Block::Air),
        "the break queued ahead of the save is in it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hold_keeps_storage_untouched_until_released() {
    let dir = TestDir::new();
    let (world, writes) = counted(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = view(&world, &viewer, &mut events).await;
    dig(&region, &viewer, &mut events, 0).await;

    let hold = hold_saves(&world.shared, "backup", DEFAULT_HOLD_LIMIT)
        .await
        .unwrap();
    assert_eq!(world.shared.saves().hold, Some(HoldPhase::Held));
    assert_eq!(
        stored_block(dir.path(), 0),
        Some(Block::Air),
        "the hold flushed first"
    );
    assert_eq!(
        hold_saves(&world.shared, "other", DEFAULT_HOLD_LIMIT)
            .await
            .err(),
        Some(HoldError::Busy(HoldBusy {
            owner: "backup".into()
        }))
    );
    let (chunks, documents) = (chunk_writes(&writes), document_writes(&writes));

    dig(&region, &viewer, &mut events, 1).await;
    let (done, flushed) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::SaveAll {
            done: Some(done),
            snapshotted: None,
            epoch: None,
        })
        .await
        .unwrap();
    flushed.await.unwrap();
    let backend = world.shared.backend.clone();
    let player = Compound::new().with("Score", 7);
    let written = player.clone();
    tokio::task::spawn_blocking(move || {
        backend
            .documents()
            .write(Document::Player(5), written)
            .unwrap();
        assert_eq!(
            backend.documents().read(Document::Player(5)).unwrap(),
            Some(Compound::new().with("Score", 7)),
            "a deferred write reads back"
        );
    })
    .await
    .unwrap();
    leave(&region, &viewer).await;
    world.ticks(140).await;
    autosave(&world).await;
    assert_eq!(chunk_writes(&writes), chunks, "no chunk write while held");
    assert_eq!(document_writes(&writes), documents, "no document write");
    assert_ne!(stored_block(dir.path(), 1), Some(Block::Air));

    hold.release().await;
    assert_eq!(world.shared.saves(), SaveState::default());
    assert_eq!(document_writes(&writes), documents + 1);
    world.ticks(40).await;
    assert!(chunk_writes(&writes) > chunks, "the kept chunk saves after");
    assert_eq!(stored_block(dir.path(), 1), Some(Block::Air));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hold_past_its_limit_reopens_saving() {
    let dir = TestDir::new();
    let (world, writes) = counted(dir.path());
    let io = world.shared.chunk_io.get(&world.shared.backend).clone();
    let id = io
        .begin_hold("crashed job", Duration::from_millis(200))
        .await
        .unwrap();
    assert!(io.close_hold(id).await);
    let backend = world.shared.backend.clone();
    let documents = document_writes(&writes);
    tokio::task::spawn_blocking(move || {
        backend
            .documents()
            .write(Document::Player(9), Compound::new())
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(document_writes(&writes), documents);
    io.watch_saves()
        .wait_for(|state| state.hold.is_none())
        .await
        .unwrap();
    io.release_hold(id).await;
    assert_eq!(document_writes(&writes), documents + 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_lifts_a_hold_and_saves_everything() {
    let dir = TestDir::new();
    let (world, _) = counted(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let region = view(&world, &viewer, &mut events).await;
    let hold = hold_saves(&world.shared, "backup", DEFAULT_HOLD_LIMIT)
        .await
        .unwrap();
    dig(&region, &viewer, &mut events, 0).await;
    world.shutdown().await;
    drop(hold);
    assert_eq!(stored_block(dir.path(), 0), Some(Block::Air));
}
