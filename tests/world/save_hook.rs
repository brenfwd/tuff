use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use tuff_core::feature::{Feature, LoadContext};
use tuff_core::storage::normalise::{SaveNormaliser, SaveNormaliserEntry, SaveView};
use tuff_core::world::messages::ManagerMessage;
use tuff_item::load::{LoadError, Phase};
use tuff_nbt::Compound;

#[derive(Clone, Copy)]
enum Mode {
    Mark,
    Fail,
    Panic,
}

struct Hook {
    key: &'static str,
    mode: Mode,
    calls: Arc<AtomicUsize>,
    dirty_reads: Arc<AtomicUsize>,
}

impl SaveNormaliser for Hook {
    fn name(&self) -> &'static str {
        "test_hook"
    }

    fn normalise(&self, view: &mut SaveView) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((_, meta)) = view.blocks.as_mut() {
            if meta.extra.get_str(self.key).is_some() {
                self.dirty_reads.fetch_add(1, Ordering::SeqCst);
            }
            meta.extra.insert(self.key, "yes");
        }
        match self.mode {
            Mode::Mark => Ok(()),
            Mode::Fail => Err("boom".into()),
            Mode::Panic => panic!("boom"),
        }
    }
}

struct Plug(
    Vec<(i32, &'static str, Mode)>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
);

impl Feature for Plug {
    fn id(&self) -> &'static str {
        "save-hook-test"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            for (order, key, mode) in &self.0 {
                cx.registries().save_normalisers.push(SaveNormaliserEntry {
                    order: *order,
                    hook: Arc::new(Hook {
                        key,
                        mode: *mode,
                        calls: self.1.clone(),
                        dirty_reads: self.2.clone(),
                    }),
                });
            }
        }
        Ok(())
    }
}

fn counters() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    (Arc::default(), Arc::default())
}

async fn break_block(world: &TestWorld, sequence: i32) {
    let (viewer, mut events) = PlayerHandle::new(100);
    let pos = ChunkPos::new(0, 0);
    let height = Generator::new(1).height(1, 1);
    let region = world.region(pos.region()).await;
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    region
        .request(RegionRequest::Break {
            player: viewer.clone(),
            pos: BlockPos::new(1, height - sequence, 1),
            tool: None,
            mode: BreakMode::Finish,
            sequence,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { .. } => Some(()),
        _ => None,
    })
    .await;
}

fn saved(dir: &Path) -> Compound {
    stored_chunk(dir, ChunkPos::new(0, 0), false).expect("chunk saved")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_save_carries_hook_changes_and_live_world_is_untouched() {
    let dir = TestDir::new();
    let (calls, dirty) = counters();
    let plug = Plug(
        vec![(0, "Marker", Mode::Mark)],
        calls.clone(),
        dirty.clone(),
    );
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(plug)]);
    break_block(&world, 0).await;
    world
        .manager
        .send(ManagerMessage::SaveAll {
            done: None,
            snapshotted: None,
        })
        .await
        .unwrap();
    world.ticks(3).await;
    let after_save_all = calls.load(Ordering::SeqCst);
    assert!(after_save_all >= 1, "save-all runs the hook");
    break_block(&world, 1).await;
    world.shutdown().await;
    assert!(calls.load(Ordering::SeqCst) > after_save_all);
    assert_eq!(dirty.load(Ordering::SeqCst), 0);
    assert_eq!(saved(dir.path()).get_str("Marker"), Some("yes"));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn autosave_runs_the_hook() {
    let dir = TestDir::new();
    let (calls, dirty) = counters();
    let plug = Plug(vec![(0, "Marker", Mode::Mark)], calls.clone(), dirty);
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(plug)]);
    break_block(&world, 0).await;
    world.ticks(6000).await;
    assert!(calls.load(Ordering::SeqCst) >= 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_hooks_keep_the_save_and_later_hooks_still_run() {
    let dir = TestDir::new();
    let (calls, dirty) = counters();
    let plug = Plug(
        vec![
            (2, "Late", Mode::Mark),
            (0, "Failed", Mode::Fail),
            (1, "Panicked", Mode::Panic),
        ],
        calls,
        dirty,
    );
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(plug)]);
    break_block(&world, 0).await;
    world.shutdown().await;
    let root = saved(dir.path());
    assert_eq!(root.get_str("Late"), Some("yes"));
    assert_eq!(root.get_str("Failed"), None);
    assert_eq!(root.get_str("Panicked"), None);
    let Decoded::Chunk(chunk, _) = decode_stored(root, ChunkPos::new(0, 0)).unwrap() else {
        panic!("terrain saved");
    };
    let height = Generator::new(1).height(1, 1);
    assert!(chunk.get(1, height, 1).block() == Block::Air);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inert_hook_writes_the_same_bytes_as_none() {
    async fn run(features: Vec<Box<dyn Feature>>) -> Compound {
        let dir = TestDir::new();
        let world = TestWorld::start_with_features(dir.path(), features);
        break_block(&world, 0).await;
        world.shutdown().await;
        saved(dir.path())
    }
    let (calls, dirty) = counters();
    let inert = Plug(vec![], calls.clone(), dirty);
    assert_eq!(run(vec![]).await, run(vec![Box::new(inert)]).await);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
