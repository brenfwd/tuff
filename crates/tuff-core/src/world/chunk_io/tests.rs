use super::*;
use tuff_storage::memory::MemoryBackend;

const DIMENSION: &str = "minecraft:overworld";

fn write(pos: ChunkPos, blob: &[u8]) -> IoOp {
    IoOp::Write {
        pos,
        blocks: Some(blob.to_vec()),
        entities: EntityWrite::Keep,
        pois: EntityWrite::Keep,
        ticket: None,
    }
}

fn read_blocks(results: Vec<IoResult>) -> Option<Blob> {
    match results.into_iter().next() {
        Some(IoResult::Read { blocks, .. }) => blocks.unwrap(),
        _ => panic!("a read result"),
    }
}

fn stored(backend: &MemoryBackend, pos: ChunkPos) -> Option<Blob> {
    backend
        .open_region(DIMENSION, pos.file_region())
        .unwrap()
        .read_chunk(pos)
        .unwrap()
}

struct FailingBackend(MemoryBackend);

impl WorldBackend for FailingBackend {
    fn name(&self) -> &str {
        "failing"
    }

    fn lock(&self) -> std::io::Result<Box<dyn tuff_storage::backend::WorldLock>> {
        self.0.lock()
    }

    fn documents(&self) -> &dyn tuff_storage::backend::Documents {
        self.0.documents()
    }

    fn open_region(&self, _: &str, _: FileRegionPos) -> std::io::Result<Box<dyn RegionStore>> {
        Err(std::io::Error::other("disk full"))
    }

    fn blob_codec(&self) -> Arc<dyn tuff_storage::backend::BlobCodec> {
        self.0.blob_codec()
    }

    fn list_dimensions(&self) -> std::io::Result<Vec<String>> {
        self.0.list_dimensions()
    }

    fn list_regions(&self, dimension: &str) -> std::io::Result<Vec<FileRegionPos>> {
        self.0.list_regions(dimension)
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn failed_reads_and_writes_are_counted_for_the_disk_warning() {
    let io = ChunkIo::start(Arc::new(FailingBackend(MemoryBackend::default())));
    let mut failures = io.failures();
    assert_eq!(*failures.borrow(), 0);
    let pos = ChunkPos::new(0, 0);
    io.submit(DIMENSION, vec![write(pos, b"a"), IoOp::Read(pos)])
        .await
        .unwrap();
    failures.changed().await.unwrap();
    assert_eq!(*failures.borrow(), 2);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn scratch_dimensions_stay_in_memory_and_vanish_when_dropped() {
    let backend = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let pos = ChunkPos::new(3, -2);
    const ARENA: &str = "game:arena";
    assert!(io.scratch(ARENA, true).await.unwrap());
    assert!(!io.scratch(ARENA, true).await.unwrap(), "already kept");
    io.submit(ARENA, vec![write(pos, b"a")]).await.unwrap();
    let read = io.submit(ARENA, vec![IoOp::Read(pos)]).await.unwrap();
    assert_eq!(read_blocks(read).as_deref(), Some(&b"a"[..]));
    assert!(backend.list_dimensions().unwrap().is_empty());

    assert!(io.scratch(ARENA, false).await.unwrap());
    assert!(!io.scratch(ARENA, false).await.unwrap());
    io.submit(ARENA, vec![write(pos, b"b")]).await.unwrap();
    let read = io.submit(ARENA, vec![IoOp::Read(pos)]).await.unwrap();
    assert_eq!(read_blocks(read), None);
    assert!(backend.list_dimensions().unwrap().is_empty());

    assert!(io.scratch(ARENA, true).await.unwrap());
    let read = io.submit(ARENA, vec![IoOp::Read(pos)]).await.unwrap();
    assert_eq!(read_blocks(read), None, "a new scratch starts empty");
    io.submit(DIMENSION, vec![write(pos, b"c")]).await.unwrap();
    assert_eq!(stored(&backend, pos).as_deref(), Some(&b"c"[..]));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn blocks_only_read_skips_entities_and_pois() {
    let backend = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend);
    let pos = ChunkPos::new(1, 1);
    let op = IoOp::Write {
        pos,
        blocks: Some(b"b".to_vec()),
        entities: EntityWrite::Write(b"e".to_vec()),
        pois: EntityWrite::Write(b"p".to_vec()),
        ticket: None,
    };
    io.submit(DIMENSION, vec![op]).await.unwrap();
    let full = io.submit(DIMENSION, vec![IoOp::Read(pos)]).await.unwrap();
    assert!(matches!(
        &full[0],
        IoResult::Read {
            entities: Ok(Some(_)),
            pois: Ok(Some(_)),
            ..
        }
    ));
    let only = io
        .submit(DIMENSION, vec![IoOp::ReadBlocks(pos)])
        .await
        .unwrap();
    match &only[0] {
        IoResult::Read {
            blocks,
            entities,
            pois,
            ..
        } => {
            assert_eq!(blocks.as_ref().unwrap().as_deref(), Some(&b"b"[..]));
            assert!(matches!(entities, Ok(None)) && matches!(pois, Ok(None)));
        }
        _ => panic!("a read result"),
    }
}

#[tuff_core::watchdog]
#[test]
fn a_dropped_job_reports_failure() {
    let (done, mut rx) = mpsc::unbounded_channel();
    drop(JobGuard {
        done,
        target: Some((("overworld".into(), FileRegionPos::new(0, 0)), 7)),
        store: None,
        writes: true,
    });
    let sent = rx.try_recv().expect("failed job reported");
    assert_eq!(sent.batch, 7);
    assert!(sent.results.is_none());
    assert!(sent.writes);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_stops_the_server_and_storage_keeps_serving() {
    let backend = Arc::new(MemoryBackend::default());
    let (stop, mut stops) = mpsc::unbounded_channel();
    let io = ChunkIo::start_with(backend.clone(), None, Fatal::channel(stop));
    let (a, b) = (ChunkPos::new(0, 0), ChunkPos::new(40, 0));
    io.submit(DIMENSION, vec![write(a, b"a")]).await.unwrap();
    let before = crate::metrics::actor_panics().get();
    assert!(io.tx.send(Message::Panic).is_ok());
    let held = io
        .begin_hold("test", Duration::from_secs(600))
        .await
        .expect("the gate still answers");
    assert!(io.close_hold(held).await);
    let parked = io.submit(DIMENSION, vec![write(b, b"b")]);
    io.release_hold(held).await;
    parked.await.expect("the write is answered");
    assert_eq!(stored(&backend, a).as_deref(), Some(&b"a"[..]));
    assert_eq!(stored(&backend, b).as_deref(), Some(&b"b"[..]));
    assert_eq!(stops.recv().await, Some(()));
    assert!(crate::metrics::actor_panics().get() > before);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn storage_gives_up_after_repeated_panics() {
    let backend = Arc::new(MemoryBackend::default());
    let (stop, mut stops) = mpsc::unbounded_channel();
    let io = ChunkIo::start_with(backend.clone(), None, Fatal::channel(stop));
    for _ in 0..tuff_supervise::CRITICAL_PANICS {
        assert!(io.tx.send(Message::Panic).is_ok());
    }
    let answer = io.submit(DIMENSION, vec![write(ChunkPos::new(0, 0), b"a")]);
    assert!(answer.await.is_err(), "a dead actor drops the answer");
    assert_eq!(stops.recv().await, Some(()));
    assert!(
        stops.try_recv().is_err(),
        "the server is asked to stop once"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn held_writes_wait_and_reads_of_them_wait_behind_them() {
    let backend = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let (a, b) = (ChunkPos::new(0, 0), ChunkPos::new(1, 0));
    let id = io
        .begin_hold("test", Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(io.saves().hold, Some(HoldPhase::Flushing));
    assert!(io.close_hold(id).await);
    assert_eq!(io.saves().hold, Some(HoldPhase::Held));
    assert!(!io.saves().saves());

    let mut written = io.submit(DIMENSION, vec![write(a, b"new")]);
    let mut read_back = io.submit(DIMENSION, vec![IoOp::Read(a)]);
    let other = io.submit(DIMENSION, vec![IoOp::Read(b)]).await.unwrap();
    assert_eq!(read_blocks(other), None, "other chunks still read");
    assert!(written.try_recv().is_err());
    assert!(read_back.try_recv().is_err());
    assert_eq!(stored(&backend, a), None);

    io.release_hold(id).await;
    assert_eq!(io.saves(), SaveState::default());
    written.await.unwrap();
    assert_eq!(read_blocks(read_back.await.unwrap()), Some(b"new".to_vec()));
    assert_eq!(stored(&backend, a), Some(b"new".to_vec()));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn shrink_closes_idle_stores_and_reopens_on_use() {
    let backend = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let chunks = [
        ChunkPos::new(0, 0),
        ChunkPos::new(40, 0),
        ChunkPos::new(0, 40),
    ];
    let ops = chunks.iter().map(|pos| write(*pos, b"data")).collect();
    io.submit(DIMENSION, ops).await.unwrap();
    assert_eq!(io.shrink().await.unwrap(), 3);
    assert_eq!(io.shrink().await.unwrap(), 0);
    let read = io
        .submit(DIMENSION, vec![IoOp::Read(chunks[1])])
        .await
        .unwrap();
    assert_eq!(read_blocks(read), Some(b"data".to_vec()));
    assert_eq!(io.shrink().await.unwrap(), 1);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_remove_deletes_the_chunk_and_its_records_and_waits_out_a_hold() {
    let backend = Arc::new(MemoryBackend::default());
    let io = ChunkIo::start(backend.clone());
    let a = ChunkPos::new(3, -2);
    let full = IoOp::Write {
        pos: a,
        blocks: Some(b"chunk".to_vec()),
        entities: EntityWrite::Write(b"pig".to_vec()),
        pois: EntityWrite::Write(b"bed".to_vec()),
        ticket: None,
    };
    io.submit(DIMENSION, vec![full]).await.unwrap();
    let id = io
        .begin_hold("test", Duration::from_secs(600))
        .await
        .unwrap();
    assert!(io.close_hold(id).await);
    let mut removed = io.submit(DIMENSION, vec![IoOp::Remove(a), IoOp::Read(a)]);
    tokio::task::yield_now().await;
    assert!(removed.try_recv().is_err(), "a remove is a write");
    assert_eq!(stored(&backend, a), Some(b"chunk".to_vec()));
    io.release_hold(id).await;
    let results = removed.await.unwrap();
    assert!(matches!(results[0], IoResult::Removed { pos, ok: true } if pos == a));
    let IoResult::Read {
        blocks,
        entities,
        pois,
        ..
    } = &results[1]
    else {
        panic!("a read result");
    };
    assert_eq!(
        (blocks.clone(), entities.clone(), pois.clone()),
        (Ok(None), Ok(None), Ok(None))
    );
    let mut store = backend.open_region(DIMENSION, a.file_region()).unwrap();
    assert_eq!(store.list_chunks().unwrap(), Vec::<ChunkPos>::new());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn one_hold_at_a_time_and_stale_ids_do_nothing() {
    let io = ChunkIo::start(Arc::new(MemoryBackend::default()));
    let id = io
        .begin_hold("first", Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(
        io.begin_hold("second", Duration::from_secs(600)).await,
        Err(HoldBusy {
            owner: "first".into()
        })
    );
    io.release_hold(id + 1).await;
    assert!(io.saves().hold.is_some());
    io.release_hold(id).await;
    assert!(!io.close_hold(id).await);
    let next = io
        .begin_hold("second", Duration::from_secs(600))
        .await
        .unwrap();
    assert_ne!(next, id);
    io.release_all().await;
    assert_eq!(io.saves(), SaveState::default());
}
