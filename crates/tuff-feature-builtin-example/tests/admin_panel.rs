//! The gold panel page updates live, with no page refresh, when gold is
//! placed in the world.

use tuff_feature_builtin_example::{ExampleFeature, GOLD_TOPIC};
use tuff_test_support::*;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placing_gold_publishes_a_count_event_with_no_refresh() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(ExampleFeature)]);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(3).await;

    let sub = world
        .shared
        .web
        .events
        .subscribe(vec![GOLD_TOPIC.to_owned()], "test", None)
        .await
        .expect("the gold topic accepts subscribers");
    let mut live = sub.live;

    let floor = BlockPos::new(4, Generator::new(1).height(4, 4), 4);
    region
        .request(RegionRequest::Place(place(
            &player,
            floor,
            Item::GoldBlock,
            1,
        )))
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;

    let published = tokio::time::timeout(std::time::Duration::from_secs(5), live.recv())
        .await
        .expect("a count event is published without any page refresh")
        .expect("the publish channel stays open");
    assert_eq!(published.topic.as_ref(), GOLD_TOPIC);
    let body: serde_json::Value =
        serde_json::from_str(&published.event.data).expect("the event body is json");
    assert_eq!(body["goldPlaced"], 1);

    world.shutdown().await;
}
