//! The map actor against a running test world: tiles drawn from loaded
//! regions and from storage, kept current by polling, zoomed out, and
//! served with versioned ETags under a rate limit.

use tuff_core::config::Value;
use tuff_core::world::dimension::OVERWORLD;
use tuff_feature_builtin_world_map::WorldMapFeature;
use tuff_feature_builtin_world_map::service::{MapHandle, MapService, RATE_KEY, TileReply};
use tuff_feature_builtin_world_map::tile::{SIDE, TileKey};
use tuff_test_support::*;
use tuff_vanilla_data::Block;

fn map(world: &TestWorld) -> MapHandle {
    world
        .shared
        .services
        .get::<MapService>()
        .expect("the map service runs")
        .handle()
        .expect("the map service started")
        .clone()
}

fn overworld(world: &TestWorld) -> u16 {
    world
        .shared
        .dimensions
        .iter()
        .position(|d| d.key == OVERWORLD)
        .unwrap() as u16
}

fn gold() -> u8 {
    Block::GoldBlock.default_state().map_color()
}

async fn png(
    map: &MapHandle,
    key: TileKey,
    if_none_match: Option<String>,
) -> (String, bytes::Bytes) {
    match map.tile(key, "test", if_none_match).await {
        TileReply::Png { etag, png } => (etag, png),
        other => panic!("expected a tile, got {other:?}"),
    }
}

fn at(pixels: &[u8], x: usize, z: usize) -> u8 {
    pixels[z * SIDE + x]
}

async fn place_gold(
    world: &TestWorld,
    region: &RegionHandle,
    xs: std::ops::Range<i32>,
    zs: std::ops::Range<i32>,
) {
    let mut blocks = Vec::new();
    for x in xs {
        for z in zs.clone() {
            blocks.push((BlockPos::new(x, 200, z), Block::GoldBlock.default_state()));
        }
    }
    place_blocks(world, region, blocks).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loaded_chunk_is_drawn_and_a_placed_block_patches_its_tile() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    let map = map(&world);
    let key = TileKey::new(overworld(&world), 0, 0, 0);

    let (etag, bytes) = png(&map, key, None).await;
    assert_eq!(bytes[..4], [0x89, b'P', b'N', b'G']);
    let pixels = map.pixels(key).await.unwrap();
    assert_ne!(at(&pixels, 4, 4), 0, "the loaded chunk is drawn");
    assert_ne!(at(&pixels, 4, 4) >> 2, gold());
    assert_eq!(
        at(&pixels, 300, 300),
        0,
        "chunks never loaded or saved are empty"
    );

    match map.tile(key, "test", Some(etag.clone())).await {
        TileReply::NotModified { etag: same } => assert_eq!(same, etag),
        other => panic!("expected 304, got {other:?}"),
    }
    let version = map.version(key).await.unwrap();
    map.poll().await;
    assert_eq!(map.version(key).await, Some(version), "nothing changed");

    place_gold(&world, &region, 4..5, 4..5).await;
    map.poll().await;
    let patched = map.version(key).await.unwrap();
    assert!(patched > version, "the change bumps the version");
    let pixels = map.pixels(key).await.unwrap();
    assert_eq!(at(&pixels, 4, 4) >> 2, gold());
    let (fresh, _) = png(&map, key, Some(etag.clone())).await;
    assert_ne!(fresh, etag, "the old ETag no longer matches");
    let stats = map.stats().await;
    assert_eq!(stats.rendered, 1);
    assert_eq!(stats.patched, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_chunks_are_drawn_through_chunk_io_without_loading_regions() {
    let dir = TestDir::new();
    {
        let world = TestWorld::start_with_features(dir.path(), vec![]);
        let region = world.region(RegionPos::new(0, 0)).await;
        let (player, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
        place_gold(&world, &region, 4..6, 4..6).await;
        world.shutdown().await;
    }
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    let map = map(&world);
    let dim = overworld(&world);
    let key = TileKey::new(dim, 0, 0, 0);
    png(&map, key, None).await;
    let pixels = map.pixels(key).await.unwrap();
    assert_eq!(at(&pixels, 4, 4) >> 2, gold());
    assert_ne!(at(&pixels, 10, 10), 0);
    assert!(map.stats().await.saved_reads >= 1);
    let running =
        tuff_feature_builtin_world_map::gather::running(&world.shared, world.dimension(OVERWORLD))
            .await;
    assert!(running.is_empty(), "drawing loads no region");

    let zoomed = TileKey::new(dim, 1, 0, 0);
    png(&map, zoomed, None).await;
    let pixels = map.pixels(zoomed).await.unwrap();
    assert_eq!(
        at(&pixels, 2, 2) >> 2,
        gold(),
        "the level above shows the gold"
    );
    assert_ne!(at(&pixels, 5, 5), 0);
    assert_eq!(at(&pixels, 300, 300), 0, "an empty child is transparent");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_areas_cost_no_reads_and_tiles_are_rate_limited() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    world.shared.config.set(RATE_KEY, Value::Int(1)).unwrap();
    let map = map(&world);
    let key = TileKey::new(overworld(&world), 3, 100, -100);
    for _ in 0..4 {
        png(&map, key, None).await;
    }
    assert!(matches!(
        map.tile(key, "test", None).await,
        TileReply::Limited(_)
    ));
    assert!(
        matches!(map.tile(key, "other", None).await, TileReply::Png { .. }),
        "budgets are per session"
    );
    let stats = map.stats().await;
    assert_eq!((stats.rendered, stats.saved_reads), (0, 0));
    assert!(matches!(
        map.tile(TileKey::new(99, 0, 0, 0), "other", None).await,
        TileReply::NotFound
    ));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_map_hears_about_changed_tiles_and_metrics_count_renders() {
    use tuff_feature_builtin_world_map::service::{TILES_TOPIC, Viewport};
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    let map = map(&world);
    let dim = overworld(&world);
    let key = TileKey::new(dim, 0, 0, 0);
    let zoomed = TileKey::new(dim, 1, 0, 0);
    png(&map, zoomed, None).await;
    let before = map.version(zoomed).await.unwrap();

    let sub = world
        .shared
        .web
        .events
        .subscribe(vec![TILES_TOPIC.to_owned()], "test", None)
        .await
        .expect("the tiles topic accepts subscribers");
    let mut live = sub.live;
    let view = Viewport {
        dim,
        level: 0,
        min: [0, 0],
        max: [64, 64],
    };
    map.viewport("test", view).await;
    place_gold(&world, &region, 8..10, 8..10).await;
    map.poll().await;
    let after = map.version(zoomed).await.unwrap();
    assert!(after > before, "the zoomed tile is patched in place");
    let pixels = map.pixels(zoomed).await.unwrap();
    assert_eq!(at(&pixels, 4, 4) >> 2, gold());

    let version = map.version(key).await.unwrap();
    let wanted = serde_json::json!([dim, 0, 0, 0, version]);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(left, live.recv())
            .await
            .expect("a changed event is published")
            .expect("the bus stays open");
        let body: serde_json::Value = serde_json::from_str(&event.event.data).unwrap();
        if body["tiles"]
            .as_array()
            .is_some_and(|t| t.contains(&wanted))
        {
            break;
        }
    }

    let snapshot = world.shared.metrics.collect().await;
    assert!(snapshot.series["map.tiles_rendered"].value >= 1);
    assert!(snapshot.series["map.tiles_patched"].value >= 1);
    assert!(snapshot.series["map.cache_bytes"].value > 0);
    world.shutdown().await;
}

async fn wait_for_pixels(map: &MapHandle, key: TileKey, x: usize, z: usize) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let pixels = map.pixels(key).await.unwrap_or_default();
        if pixels.get(z * SIDE + x).is_some_and(|p| *p != 0) {
            return pixels;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{key:?} never showed the generated chunk"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pre_generated_chunks_show_up_on_cached_and_new_tiles() {
    use tuff_core::world::pregen::{Area, JobSpec, Shape};
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    let map = map(&world);
    let dim = overworld(&world);
    let cached = TileKey::new(dim, 0, 0, 0);
    let zoomed = TileKey::new(dim, 1, 0, 0);
    png(&map, cached, None).await;
    png(&map, zoomed, None).await;
    assert!(map.pixels(cached).await.unwrap().iter().all(|p| *p == 0));
    let version = map.version(cached).await.unwrap();

    let spec = JobSpec {
        dimension: world.dimension(OVERWORLD),
        area: Area::new(512, 40, 40, Shape::Square),
        regenerate: false,
        by: "test".to_owned(),
    };
    world
        .shared
        .pregen
        .start(&world.shared, spec)
        .await
        .unwrap();
    let done = world.shared.pregen.wait().await.unwrap();
    assert_eq!(done.failed, 0);

    let pixels = wait_for_pixels(&map, cached, 500, 40).await;
    assert_eq!(
        pixels[40 * SIDE + 300],
        0,
        "only generated chunks are drawn"
    );
    assert!(map.version(cached).await.unwrap() > version);
    png(&map, zoomed, None).await;
    let pixels = map.pixels(zoomed).await.unwrap();
    assert_ne!(at(&pixels, 250, 20), 0, "the level above is drawn again");

    let fresh = TileKey::new(dim, 0, 1, 0);
    png(&map, fresh, None).await;
    let pixels = map.pixels(fresh).await.unwrap();
    assert_ne!(
        at(&pixels, 10, 40),
        0,
        "a tile asked for later reads the new chunks"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panel_jobs_are_validated_and_regenerating_needs_confirming() {
    use tuff_core::world::pregen::Shape;
    use tuff_feature_builtin_world_map::api::job;
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    let by = || "Alex".to_owned();
    let spec = job(
        &world.shared,
        br#"{"dim":0,"x":-5,"z":7,"radius":64,"shape":"circle"}"#,
        by(),
    )
    .unwrap();
    assert_eq!(
        (spec.area.x, spec.area.z, spec.area.radius, spec.area.shape),
        (-5, 7, 64, Shape::Circle)
    );
    assert!(!spec.regenerate);
    assert_eq!(
        spec.dimension,
        world.shared.dimensions.iter().next().unwrap().id
    );
    let unconfirmed = br#"{"dim":0,"x":0,"z":0,"radius":1,"shape":"square","regenerate":true}"#;
    assert!(job(&world.shared, unconfirmed, by()).is_none());
    let confirmed = br#"{"dim":0,"x":0,"z":0,"radius":1,"shape":"square","regenerate":true,"confirm":"regenerate"}"#;
    assert!(job(&world.shared, confirmed, by()).unwrap().regenerate);
    for bad in [
        &br#"{"dim":0,"x":0,"z":0,"radius":-1,"shape":"square"}"#[..],
        br#"{"dim":0,"x":0,"z":0,"radius":100001,"shape":"square"}"#,
        br#"{"dim":0,"x":0,"z":0,"radius":1,"shape":"hexagon"}"#,
        br#"{"dim":99,"x":0,"z":0,"radius":1,"shape":"square"}"#,
        br#"{"dim":0,"z":0,"radius":1,"shape":"square"}"#,
        b"nope",
    ] {
        assert!(job(&world.shared, bad, by()).is_none());
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entities_in_viewed_regions_are_published_as_markers() {
    use tuff_core::world::messages::RegionRequest;
    use tuff_feature_builtin_world_map::service::{MARKERS_TOPIC, Viewport};
    use tuff_nbt::Compound;
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(WorldMapFeature)]);
    let region = world.region(RegionPos::new(0, 0)).await;
    let far = world.region(RegionPos::new(5, 5)).await;
    for (target, position) in [(&region, [8.5, 80.0, 8.5]), (&far, [2600.5, 80.0, 2600.5])] {
        let (reply, summoned) = tokio::sync::oneshot::channel();
        target
            .request(RegionRequest::Summon {
                nbt: Compound::new().with("id", "minecraft:pig"),
                position,
                fresh: true,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    let sub = world
        .shared
        .web
        .events
        .subscribe(vec![MARKERS_TOPIC.to_owned()], "test", None)
        .await
        .expect("the markers topic accepts subscribers");
    let mut live = sub.live;
    let map = map(&world);
    map.viewport(
        "test",
        Viewport {
            dim: overworld(&world),
            level: 0,
            min: [0, 0],
            max: [64, 64],
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(left, live.recv())
            .await
            .expect("an entities event is published")
            .expect("the bus stays open");
        if event.event.event.as_deref() != Some("entities") {
            continue;
        }
        let body: serde_json::Value = serde_json::from_str(&event.event.data).unwrap();
        let rows = body["entities"].as_array().unwrap();
        if rows.is_empty() {
            continue;
        }
        assert_eq!(rows.len(), 1, "the unviewed region is not asked");
        assert_eq!(rows[0]["type"], "minecraft:pig");
        assert_eq!(rows[0]["group"], "mob");
        break;
    }
    world.shutdown().await;
}
