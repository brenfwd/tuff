//! The world actors' metrics reporters over a test world.

use super::*;
use tuff_vanilla_data::entity::MobCategory;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regions_the_manager_and_chunk_io_report_every_20_ticks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(2, 2);
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    region
        .request(update(&player, 1, [40.5, 100.0, 40.5]))
        .await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", "minecraft:cow"),
            position: [36.5, 67.0, 36.5],
            fresh: true,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(40).await;

    let snapshot = world.shared.metrics.collect().await;
    let report = snapshot
        .slowest_regions
        .iter()
        .find(|r| r.dimension == "minecraft:overworld" && (r.x, r.z) == (0, 0))
        .expect("the region reported");
    assert_eq!(report.ticks, 20);
    assert!(report.tick >= 20);
    assert!(report.chunks >= 9, "{report:?}");
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let all = answer.await.unwrap();
    assert_eq!(report.entities, 1, "{all:?}");
    assert_eq!(report.players, 1);
    assert!(report.mspt_max >= report.mspt_mean);
    assert_eq!(
        snapshot.world.entities_by_category[MobCategory::Creature.name()],
        1
    );
    let manager = snapshot.manager.as_ref().expect("the manager reported");
    assert!(manager.regions >= 1 && manager.region_tickets >= 1);
    assert!(snapshot.chunk_io.is_some());
    assert!(snapshot.tick.is_none(), "a test world has no coordinator");
    assert_eq!(snapshot.dropped_reports, 0);
    assert!(
        world
            .shared
            .metrics
            .history(crate::metrics::WORLD_ENTITIES, Duration::from_secs(60))
            .await
            .is_some_and(|points| points.last().is_some_and(|p| p.value == 1.0))
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshots_reach_the_panel_event_stream_within_two_seconds_of_ticks() {
    use tuff_web_ui::auth::{AuthContext, Principal};
    use tuff_web_ui::dashboard::{Gate, forward_snapshots};
    use tuff_web_ui::events::{METRICS, PanelEvents};

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(2, 2);
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, chunk).await;
    let bus = PanelEvents::spawn();
    let ctx = AuthContext {
        principal: Principal::Console,
        level: tuff_storage::admin::PermissionLevel::Owners,
        session: "s".into(),
        csrf: String::new(),
        permissions: std::sync::Arc::new(crate::permission::View::default()),
    };
    let mut stream = bus.stream(&ctx, METRICS, None).await.unwrap();
    tokio::spawn(forward_snapshots(
        world.shared.metrics.subscribe(),
        |snapshot| snapshot.to_json(),
        bus.clone(),
        || Gate::Publish { audit_failures: 0 },
    ));
    world.ticks(40).await;
    let ran = *world.clock.borrow();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let event = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("no snapshot with the region's report within two seconds")
            .unwrap();
        assert_eq!(event.event.as_deref(), Some("snapshot"));
        let data: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        let reported = data["snapshot"]["slowest_regions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["x"] == 0 && r["z"] == 0 && r["tick"].as_u64() >= Some(ran - 20));
        if reported {
            break;
        }
    }
    world.shutdown().await;
}
