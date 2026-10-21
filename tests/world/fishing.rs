//! Fishing: a cast bobber lasting while its owner holds the rod, and
//! reeling one in with a fish on the line.

use super::*;
use crate::world::entity::{Culprit, Notice};
use crate::world::inventory::Inventory;
use crate::world::messages::{AdvancementEvent, PlayerLooks};
use crate::world::stats::Stat;
use tuff_core::world::projectile::Owner;
use tuff_feature_builtin_vanilla_combat::projectile::bobber::Bobber;
use tuff_vanilla_data::registry::{EntityType, StatType};

const UUID: u128 = 77;

fn rod_looks() -> PlayerLooks {
    PlayerLooks {
        held: [Some(Item::FishingRod), None],
        eye_height: 1.62,
        height: 1.8,
        ..Default::default()
    }
}

fn owner(player: &PlayerHandle) -> Owner {
    Owner::of(
        Culprit::player(player.id, UUID, "Tester"),
        Some(player.clone()),
    )
}

fn pool() -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = pen(2, 13);
    for x in 5..=9 {
        for z in 5..=9 {
            blocks.push((BlockPos::new(x, SKY + 1, z), Block::Water.default_state()));
        }
    }
    blocks
}

fn bobber(world: &TestWorld, player: &PlayerHandle) -> Bobber {
    Bobber::cast(
        world.shared.next_entity_id(),
        [7.5, f64::from(SKY) + 1.2, 7.5],
        (0.0, 90.0),
        owner(player),
        tuff_item::item::ItemSnapshot::describe(Item::FishingRod, 1, Default::default()),
        0.0,
        &mut rand::thread_rng(),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reeling_in_a_bite_lands_loot_and_meets_the_hooked_trigger() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(2000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pool()).await;
    let bobber = bobber(&world, &viewer).with_fish_on();
    let id = bobber.base.id;
    region
        .request(RegionRequest::SpawnEntity(Box::new(bobber)))
        .await;
    region
        .request(player_at(
            &viewer,
            [7.5, f64::from(SKY) + 1.0, 4.5],
            rod_looks(),
        ))
        .await;
    wait_for(&world, &region, 10, |found| {
        found.iter().any(|s| s.kind == EntityType::FishingBobber)
    })
    .await;
    region
        .request(RegionRequest::Notify {
            target: id,
            notice: Notice::Reel,
        })
        .await;
    let (items, damage) = ticking_expect(&world, &mut events, 20, {
        let mut items = None;
        move |event| match event {
            PlayerEvent::Advancement(event) => {
                if let AdvancementEvent::FishingRodHooked { items: caught, .. } = *event {
                    items = Some(caught);
                }
                None
            }
            PlayerEvent::Reeled { damage, .. } => Some((items.take(), damage)),
            _ => None,
        }
    })
    .await;
    assert_eq!(damage, 1, "a catch wears the rod by one");
    assert_eq!(items.map(|i| i.len()), Some(1), "one catch");
    wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
            && !found.iter().any(|s| s.kind == EntityType::FishingBobber)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bobber_vanishes_once_its_owner_stops_holding_the_rod() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(2000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pool()).await;
    let bobber = bobber(&world, &viewer);
    let id = bobber.base.id;
    region
        .request(RegionRequest::SpawnEntity(Box::new(bobber)))
        .await;
    let rod = [7.5, f64::from(SKY) + 1.0, 4.5];
    region.request(player_at(&viewer, rod, rod_looks())).await;
    world.ticks(3).await;
    assert!(
        snapshots(&region)
            .await
            .iter()
            .any(|s| s.kind == EntityType::FishingBobber),
        "it stays while the rod is held"
    );
    region
        .request(player_at(&viewer, rod, PlayerLooks::default()))
        .await;
    let gone = ticking_expect(&world, &mut events, 20, |event| match event {
        PlayerEvent::BobberGone(gone) => Some(gone),
        _ => None,
    })
    .await;
    assert_eq!(gone, id);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn using_a_rod_casts_then_reels_in() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (mut player, outbound) = session_with_outbound(&world, 1000);
    std::mem::forget(outbound);
    player.stand_at([4.5, f64::from(SKY) + 1.0, 4.5], true);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::FishingRod, 1, MintReason::Test)),
    );
    player.publish(true).await;
    let use_item = || crate::protocol::play::Serverbound::UseItem {
        off_hand: false,
        sequence: 1,
    };
    player.handle_packet(use_item()).await;
    assert!(
        player
            .ext
            .entry::<tuff_feature_builtin_vanilla_items::world::session::item_use::FishingBobber>()
            .0
            .is_some(),
        "the first use casts"
    );
    wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::FishingBobber)
    })
    .await;
    player.handle_packet(use_item()).await;
    assert!(
        player
            .ext
            .entry::<tuff_feature_builtin_vanilla_items::world::session::item_use::FishingBobber>()
            .0
            .is_none(),
        "the second use reels in"
    );
    assert_eq!(
        player.stat(Stat::item(StatType::Used, Item::FishingRod)),
        1,
        "only the cast counts as a use"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bobber_waiting_on_water_shows_fish_particles() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(2000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pool()).await;
    let bobber = bobber(&world, &viewer);
    region
        .request(RegionRequest::SpawnEntity(Box::new(bobber)))
        .await;
    region
        .request(player_at(
            &viewer,
            [7.5, f64::from(SKY) + 1.0, 4.5],
            rod_looks(),
        ))
        .await;
    let particles = tuff_vanilla_data::packet::play::Clientbound::LevelParticles.id();
    ticking_expect(&world, &mut events, 700, |event| match event {
        PlayerEvent::ChunkPackets { packets, .. } | PlayerEvent::Packets(packets) => packets
            .iter()
            .any(|f| framed_packet_id(f) == particles)
            .then_some(()),
        _ => None,
    })
    .await;
    world.shutdown().await;
}
