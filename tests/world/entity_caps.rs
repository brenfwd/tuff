//! Per-chunk entity caps trim a chunk on save and on load.

use super::*;
use crate::storage::codec::WorldBackendExt as _;
use tuff_core::config::Value;

const ITEM_CAP: &str = "tuff.world.entityCaps.item";

fn journaled(dir: &Path) -> Arc<dyn WorldBackend> {
    tuff_core::storage::journaled(Storage::default().open(dir).unwrap(), dir).unwrap()
}

async fn save(world: &TestWorld) {
    let (done, saved) = oneshot::channel();
    world
        .shared
        .coordinator
        .send(CoordinatorMessage::SaveAll { flush: true, done })
        .await
        .unwrap();
    saved.await.unwrap().unwrap();
}

async fn load_chunk(world: &TestWorld) -> (PlayerHandle, mpsc::UnboundedReceiver<PlayerEvent>) {
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[pos]).await;
    (player, events)
}

async fn summon_items(world: &TestWorld, n: usize) {
    for _ in 0..n {
        let answer = crate::world::commands::console::run(
            world.shared.clone(),
            "summon minecraft:item 1.5 120 1.5 {Item:{id:\"minecraft:stone\",count:1}}",
        )
        .await;
        assert_eq!(answer[0].to_value()["translate"], "commands.summon.success");
    }
}

fn saved_item_count(backend: &Arc<dyn WorldBackend>) -> usize {
    let pos = ChunkPos::new(0, 0);
    let blob = backend
        .open_region(OVERWORLD, pos.file_region())
        .unwrap()
        .read_entities(pos)
        .unwrap();
    let Some(blob) = blob else { return 0 };
    backend
        .chunk_codec()
        .decode_entities(&blob, pos)
        .unwrap()
        .iter()
        .filter(|e| e.get_str("id") == Some("minecraft:item"))
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entity_caps_trim_live_entities_on_save() {
    let dir = TestDir::new();
    let backend = journaled(dir.path());
    let world = TestWorld::start_with(backend.clone());
    world.shared.config.set(ITEM_CAP, Value::Int(3)).unwrap();
    let _viewer = load_chunk(&world).await;
    summon_items(&world, 6).await;
    save(&world).await;
    world.shutdown().await;
    assert_eq!(saved_item_count(&backend), 3);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entity_caps_trim_a_saved_chunk_on_load() {
    let dir = TestDir::new();
    let backend = journaled(dir.path());
    {
        let world = TestWorld::start_with(backend.clone());
        world.shared.config.set(ITEM_CAP, Value::Int(0)).unwrap();
        let _viewer = load_chunk(&world).await;
        summon_items(&world, 6).await;
        save(&world).await;
        world.shutdown().await;
    }
    assert_eq!(saved_item_count(&backend), 6);

    let world = TestWorld::start_with(backend.clone());
    world.shared.config.set(ITEM_CAP, Value::Int(2)).unwrap();
    let _viewer = load_chunk(&world).await;
    let answer = crate::world::commands::console::run(
        world.shared.clone(),
        "execute if entity @e[type=minecraft:item]",
    )
    .await;
    let text = format!("{:?}", answer[0].to_value());
    assert!(text.contains("ount: 2") || text.contains("\"2\""), "{text}");
    world.shutdown().await;
}
