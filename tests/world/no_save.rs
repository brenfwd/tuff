//! A `--no-save` world keeps the run's own changes across a chunk unload
//! and reload while nothing reaches disk.

use super::*;
use tuff_storage::no_save::NoSaveBackend;
use tuff_storage::sqlite::SqliteBackend;

async fn loaded_chunks(world: &TestWorld) -> u64 {
    world.shared.metrics.collect().await.world.chunks
}

async fn unload_and_reload_keeps_the_change(world: &TestWorld) {
    let chunk = ChunkPos::new(2, 2);
    let pos = BlockPos::new(37, 200, 38);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(world, &region, [(pos, Block::GoldBlock.default_state())]).await;
    region
        .request(RegionRequest::Unsubscribe {
            player: viewer.id,
            chunks: vec![chunk],
        })
        .await;
    world.ticks(150).await;
    assert_eq!(loaded_chunks(world).await, 0, "the chunk was unloaded");
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    assert_eq!(
        block_at(&region, pos).await,
        Some(Block::GoldBlock.default_state())
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_overlaid_vanilla_world_sees_its_writes_but_leaves_the_folder_empty() {
    let dir = TestDir::new();
    let inner = Storage::default().open(dir.path()).unwrap();
    let world = TestWorld::start_with(Arc::new(NoSaveBackend::new(inner)));
    unload_and_reload_keeps_the_change(&world).await;
    world.shutdown().await;
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_memory_server_file_keeps_chunks_across_unloads() {
    let backend = SqliteBackend::open_in_memory().unwrap();
    let world = TestWorld::start_with(Arc::new(backend));
    unload_and_reload_keeps_the_change(&world).await;
    world.shutdown().await;
}
