//! Season-sensitive spawns with the calendar pinned to Halloween. Own target so the pin
//! cannot reach the other scenario tests.

use tuff_core::world::messages::PlayerLooks;
use tuff_feature_builtin_vanilla_mobs::calendar::pin_halloween;
use tuff_test_support::*;
use tuff_vanilla_data::registry::EntityType;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trap_horsemen_wear_an_iron_helmet_or_a_halloween_head_on_halloween() {
    pin_halloween(Some(true));
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(300);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::SkeletonHorse,
        Compound::new().with("SkeletonTrap", true),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 40.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(20).await;
    region
        .request(player_at(
            &player,
            [10.5, SKY as f64 + 1.0, 10.5],
            PlayerLooks::default(),
        ))
        .await;
    wait_for(&world, &region, 120, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Skeleton)
            .count()
            == 4
    })
    .await;
    world.shutdown().await;
    let saved = stored_entities(dir.path(), chunk);
    let mut riders = 0;
    for horse in saved
        .iter()
        .filter(|e| e.get_str("id") == Some("minecraft:skeleton_horse"))
    {
        let rider = horse.get_list("Passengers").unwrap()[0]
            .as_compound()
            .unwrap();
        let worn = format!("{:?}", rider.get("equipment"));
        assert!(
            ["iron_helmet", "carved_pumpkin", "jack_o_lantern"]
                .iter()
                .any(|head| worn.contains(head)),
            "every horseman has a head item: {rider:?}"
        );
        riders += 1;
    }
    assert_eq!(riders, 4);
}
