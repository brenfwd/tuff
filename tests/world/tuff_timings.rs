//! `/tuff timings` over a ticking world: region, entity and plugin spans
//! land in the report with their region, type and plugin attribution.

use super::*;
use ::tuff_timings::report::{Report, ReportNode};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect()
}

fn child<'a>(node: &'a ReportNode, name: &str, tag: Option<&str>) -> Option<&'a ReportNode> {
    node.children
        .iter()
        .find(|c| c.name == name && (tag.is_none() || c.tag.as_deref() == tag))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_ticks_attribute_regions_entities_and_features() {
    let _serial = ::tuff_timings::serial_async().await;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(2).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:pig")
                .with("NoAI", true)
                .with("NoGravity", true),
            position: [8.5, 150.0, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    assert_eq!(
        console(&world, "tuff timings stop").await,
        ["no timings recording is running"]
    );
    let started = console(&world, "tuff timings record").await;
    assert!(started[0].starts_with("Recording timings"), "{started:?}");
    world.ticks(10).await;
    let status = console(&world, "tuff timings status").await;
    assert!(status[0].starts_with("Recording timings for"), "{status:?}");
    let stopped = console(&world, "tuff timings stop").await;
    assert!(stopped[0].starts_with("Timings timings-"), "{stopped:?}");
    assert!(stopped[1].starts_with("Not saved"), "{stopped:?}");
    assert!(stopped.iter().any(|l| l.starts_with("Most self time: ")));

    let last = world.shared.timings.status().await.unwrap().last.unwrap();
    let listed = console(&world, "tuff timings list").await;
    assert!(listed[0].contains(&last.id), "{listed:?}");
    let report = Report::from_json(&world.shared.timings.fetch(&last.id).await.unwrap()).unwrap();
    let tick = report
        .merged
        .iter()
        .find(|n| n.name == "region.tick" && n.tag.as_deref() == Some("minecraft:overworld 0,0"))
        .expect("the region's ticks");
    assert!(tick.count >= 10, "{}", tick.count);
    let entities = child(tick, "region.entities", None).expect("entity ticking");
    let pig = child(entities, "entity", Some("minecraft:pig")).expect("the pig's ticks");
    assert!(pig.count >= 10 && pig.total_ns > 0);
    let hooks = tick
        .children
        .iter()
        .filter(|n| n.name == "region.hooks")
        .flat_map(|n| n.children.iter())
        .filter(|n| n.name == "feature")
        .filter_map(|n| n.tag.as_deref())
        .collect::<Vec<_>>();
    assert!(hooks.contains(&"vanilla-entities"), "{hooks:?}");
    assert!(!hooks.contains(&crate::feature::UNATTRIBUTED), "{hooks:?}");
    let plugin = report
        .features
        .iter()
        .find(|p| p.id == "vanilla-entities")
        .unwrap();
    assert!(plugin.calls > 0);
    assert!(report.features.iter().any(|p| p.id == "core-commands"));
    assert!(tick.self_ns <= tick.total_ns);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn perf_start_and_stop_wrap_a_timings_recording() {
    let _serial = ::tuff_timings::serial_async().await;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    assert_eq!(
        console(&world, "perf stop").await,
        ["The performance profiler hasn't started"]
    );
    assert_eq!(
        console(&world, "perf start").await,
        ["Started a 10-second performance profiling run (use '/perf stop' to stop early)"]
    );
    assert_eq!(
        console(&world, "perf start").await,
        ["The performance profiler is already started"]
    );
    world.ticks(5).await;
    let stopped = console(&world, "perf stop").await;
    assert!(
        stopped[0].starts_with("Stopped performance profiling"),
        "{stopped:?}"
    );
    world.shutdown().await;
}
