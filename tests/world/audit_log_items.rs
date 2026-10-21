//! The audit log's item rows (thrown, dropped and picked up items) and
//! kills, which roll back by summoning the victim again.

use super::*;
use tuff_feature_builtin_audit_log::AuditLogFeature;
use tuff_feature_builtin_audit_log::filter::Filter;
use tuff_feature_builtin_audit_log::record::Kind;
use tuff_feature_builtin_audit_log::store::Order;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::EntityType;

fn start(dir: &Path) -> TestWorld {
    TestWorld::start_with_features(
        dir,
        vec![Box::new(AuditLogFeature::at(dir.join("audit.sqlite")))],
    )
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn rows(
    world: &TestWorld,
    kinds: Vec<Kind>,
) -> Vec<tuff_feature_builtin_audit_log::store::Row> {
    let log = world
        .shared
        .services
        .get::<tuff_feature_builtin_audit_log::AuditLog>()
        .expect("the service runs");
    log.sync().await.unwrap();
    log.lookup(
        Filter {
            kinds,
            ..Filter::default()
        },
        Order::OldestFirst,
        100,
        0,
    )
    .await
    .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_killed_mob_is_summoned_again_by_rollback_and_removed_by_restore() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let nbt = Compound::new()
        .with("NoAI", Tag::Byte(1))
        .with("CustomName", "\"Bessie\"");
    summon(&region, EntityType::Cow, nbt, [6.5, SKY as f64 + 1.0, 6.5]).await;
    assert_eq!(mobs(&region, EntityType::Cow).await.len(), 1);

    console(&world, "kill @e[type=minecraft:cow]").await;
    world.ticks(60).await;
    assert!(mobs(&region, EntityType::Cow).await.is_empty());

    let logged = rows(&world, vec![Kind::Kill]).await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].material.as_deref(), Some("minecraft:cow"));
    assert_eq!(logged[0].pos, [6, SKY + 1, 6]);
    let saved = logged[0].block_entity().expect("the victim is saved");
    assert!(saved.get("Health").is_none() && saved.get("DeathTime").is_none());

    let rolled = console(&world, "tuff audit rollback t:1h r:#global a:kill").await;
    assert!(rolled[0].contains("Rolled back"), "{rolled:?}");
    world.ticks(2).await;
    assert_eq!(mobs(&region, EntityType::Cow).await.len(), 1);

    console(&world, "tuff audit restore t:1h r:#global a:kill").await;
    world.ticks(2).await;
    assert!(mobs(&region, EntityType::Cow).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thrown_item_and_its_pickup_are_item_rows() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let at = [6.5, SKY as f64 + 1.0, 6.5];
    region.request(update(&player, 1, at)).await;
    let item = Compound::new()
        .with("id", "minecraft:item")
        .with(
            "Item",
            Tag::Compound(
                Compound::new()
                    .with("id", Item::Stone.name())
                    .with("count", Tag::Int(3)),
            ),
        )
        .with("Thrower", Tag::uuid(0xabcd))
        .with("PickupDelay", Tag::Short(0));
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: item,
            position: at,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(2).await;
    let mut offered = None;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::OfferItem { item, offer, .. } = event {
            offered = Some((item, offer));
        }
    }
    let (item, offer) = offered.expect("the item is offered");
    region.internal(RegionInternal::ItemTaken {
        item,
        offer,
        player: player.clone(),
        taken: 3,
    });
    world.ticks(2).await;

    let dropped = rows(&world, vec![Kind::ItemRemove]).await;
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert_eq!(dropped[0].material.as_deref(), Some("minecraft:stone"));
    assert_eq!(dropped[0].amount, 3);
    let taken = rows(&world, vec![Kind::ItemAdd]).await;
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].amount, 3);
    world.shutdown().await;
}
