//! `simulation-distance`: entities in loaded chunks beyond it stop ticking.

use super::*;
use crate::world::SimulationSettings;
use crate::world::messages::PlayerLooks;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;

async fn heights(simulation_distance: i32) -> (f64, f64) {
    let dir = TestDir::new();
    let world = TestWorld::start_simulating(
        dir.path(),
        4,
        SimulationSettings {
            simulation_distance,
            pause_when_empty_seconds: -1,
            broadcast_range_percent: 100,
        },
    );
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let near = ChunkPos::new(0, 0);
    let far = ChunkPos::new(3, 0);
    subscribe(&region, &player, &mut events, &[near, far]).await;
    let y = SKY as f64 + 40.0;
    region
        .request(player_at(&player, [8.5, y, 8.5], PlayerLooks::default()))
        .await;
    for x in [8.5, 56.5] {
        summon(
            &region,
            EntityType::Item,
            Compound::new().with(
                "Item",
                tuff_item::item::ItemPicture::new(Item::Stone, 1).to_nbt(),
            ),
            [x, y, 8.5],
        )
        .await;
    }
    world.ticks(10).await;
    let items = snapshots(&region).await;
    let at = |x: f64| {
        items
            .iter()
            .find(|s| s.kind == EntityType::Item && (s.position[0] - x).abs() < 1.0)
            .expect("item")
            .position[1]
    };
    (at(8.5), at(56.5))
}

#[tuff_core::watchdog]
#[tokio::test]
async fn entities_beyond_the_simulation_distance_do_not_tick() {
    let y = SKY as f64 + 40.0;
    let (near, far) = heights(1).await;
    assert!(near < y, "the near item falls");
    assert_eq!(far, y, "the far item holds still");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_simulation_distance_reaching_them_ticks_both() {
    let y = SKY as f64 + 40.0;
    let (near, far) = heights(3).await;
    assert!(near < y && far < y);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_distance_beyond_the_player_index_reach_is_honoured() {
    let y = SKY as f64 + 40.0;
    let dir = TestDir::new();
    let world = TestWorld::start_simulating(
        dir.path(),
        12,
        SimulationSettings {
            simulation_distance: 9,
            pause_when_empty_seconds: -1,
            broadcast_range_percent: 100,
        },
    );
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let near = ChunkPos::new(9, 0);
    let far = ChunkPos::new(11, 0);
    subscribe(
        &region,
        &player,
        &mut events,
        &[ChunkPos::new(0, 0), near, far],
    )
    .await;
    region
        .request(player_at(&player, [8.5, y, 8.5], PlayerLooks::default()))
        .await;
    for x in [9 * 16 + 8, 11 * 16 + 8] {
        summon(
            &region,
            EntityType::Item,
            Compound::new().with(
                "Item",
                tuff_item::item::ItemPicture::new(Item::Stone, 1).to_nbt(),
            ),
            [x as f64 + 0.5, y, 8.5],
        )
        .await;
    }
    world.ticks(10).await;
    let items = snapshots(&region).await;
    let at = |x: f64| {
        items
            .iter()
            .find(|s| s.kind == EntityType::Item && (s.position[0] - x).abs() < 1.0)
            .expect("item")
            .position[1]
    };
    assert!(at(152.5) < y, "inside nine chunks falls");
    assert_eq!(at(184.5), y, "outside nine chunks holds");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_spectator_alone_keeps_nearby_entities_ticking() {
    let y = SKY as f64 + 40.0;
    let dir = TestDir::new();
    let world = TestWorld::start_simulating(
        dir.path(),
        4,
        SimulationSettings {
            simulation_distance: 1,
            pause_when_empty_seconds: -1,
            broadcast_range_percent: 100,
        },
    );
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let near = ChunkPos::new(0, 0);
    let far = ChunkPos::new(3, 0);
    subscribe(&region, &player, &mut events, &[near, far]).await;
    let mut request = player_at(&player, [8.5, y, 8.5], PlayerLooks::default());
    if let RegionRequest::Player(update) = &mut request {
        update.spectator = true;
    }
    region.request(request).await;
    for x in [8.5, 56.5] {
        summon(
            &region,
            EntityType::Item,
            Compound::new().with(
                "Item",
                tuff_item::item::ItemPicture::new(Item::Stone, 1).to_nbt(),
            ),
            [x, y, 8.5],
        )
        .await;
    }
    world.ticks(10).await;
    let items = snapshots(&region).await;
    let at = |x: f64| {
        items
            .iter()
            .find(|s| s.kind == EntityType::Item && (s.position[0] - x).abs() < 1.0)
            .expect("item")
            .position[1]
    };
    assert!(at(8.5) < y, "the item beside the spectator falls");
    assert_eq!(at(56.5), y, "the far item holds still");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_spectator_in_a_neighbouring_region_keeps_chunks_simulating() {
    let y = SKY as f64 + 40.0;
    let dir = TestDir::new();
    let world = TestWorld::start_simulating(
        dir.path(),
        4,
        SimulationSettings {
            simulation_distance: 1,
            pause_when_empty_seconds: -1,
            broadcast_range_percent: 100,
        },
    );
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    let near = ChunkPos::new(31, 0);
    let far = ChunkPos::new(28, 0);
    subscribe(&region, &viewer, &mut events, &[near, far]).await;
    let spectator = [32.0 * 16.0 + 8.5, y, 8.5];
    world.players.send_replace(std::sync::Arc::new(
        crate::world::nearby::PlayerIndex::default()
            .with_spectators([(world.dimension(OVERWORLD), spectator)]),
    ));
    for x in [31.0 * 16.0 + 8.5, 28.0 * 16.0 + 8.5] {
        summon(
            &region,
            EntityType::Item,
            Compound::new().with(
                "Item",
                tuff_item::item::ItemPicture::new(Item::Stone, 1).to_nbt(),
            ),
            [x, y, 8.5],
        )
        .await;
    }
    world.ticks(10).await;
    let items = snapshots(&region).await;
    let at = |x: f64| {
        items
            .iter()
            .find(|s| s.kind == EntityType::Item && (s.position[0] - x).abs() < 1.0)
            .expect("item")
            .position[1]
    };
    assert!(
        at(31.0 * 16.0 + 8.5) < y,
        "beside the neighbouring spectator falls"
    );
    assert_eq!(at(28.0 * 16.0 + 8.5), y, "beyond its distance holds still");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_hostile_mob_near_a_lone_spectator_ticks_but_never_targets_it() {
    let y = SKY as f64 + 40.0;
    let dir = TestDir::new();
    let world = TestWorld::start_simulating(
        dir.path(),
        4,
        SimulationSettings {
            simulation_distance: 1,
            pause_when_empty_seconds: -1,
            broadcast_range_percent: 100,
        },
    );
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[ChunkPos::new(0, 0)]).await;
    let mut request = player_at(&player, [8.5, y, 8.5], PlayerLooks::default());
    if let RegionRequest::Player(update) = &mut request {
        update.spectator = true;
    }
    region.request(request).await;
    summon(&region, EntityType::Zombie, Compound::new(), [10.5, y, 8.5]).await;
    world.ticks(40).await;
    let zombie = snapshots(&region)
        .await
        .into_iter()
        .find(|s| s.kind == EntityType::Zombie)
        .expect("zombie");
    assert!(zombie.position[1] < y, "the zombie ticks and falls");
    let (tx, rx) = std::sync::mpsc::channel();
    let id = zombie.id;
    let pos = BlockPos::new(8, 0, 8);
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: crate::world::region_api::RegionTask::new(move |world| {
                let target = world
                    .entity(id)
                    .and_then(|t| t.entity.living())
                    .map(|l| l.target);
                let _ = tx.send(target);
            }),
        })
        .await;
    world.ticks(1).await;
    assert_eq!(rx.try_recv().ok().flatten(), Some(None), "no target");
}
