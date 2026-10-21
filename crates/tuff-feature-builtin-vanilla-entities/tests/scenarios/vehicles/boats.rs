//! Boats through a real region (floating, falling, riding, driving,
//! getting off and breaking) and the player spawn search.

use super::*;
use crate::protocol::play::{Movement, Serverbound};
use crate::world::entity::EntityAttack;
use crate::world::messages::VehicleRequest;
use tuff_vanilla_data::registry::EntityType;

pub(super) fn punch(damage: f32) -> EntityAttack {
    EntityAttack {
        attacker: 100,
        attacker_uuid: 77,
        attacker_position: [3.5, SKY as f64 + 1.0, 3.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boats_float_on_water_and_fall_onto_land() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 6..=10 {
        for z in 6..=10 {
            blocks.push((BlockPos::new(x, SKY - 1, z), Block::Stone.default_state()));
            blocks.push((BlockPos::new(x, SKY, z), Block::Water.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::OakBoat,
        Compound::new(),
        [8.5, SKY as f64 + 2.0, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::BirchBoat,
        Compound::new(),
        [3.5, SKY as f64 + 3.0, 3.5],
    )
    .await;
    world.ticks(80).await;
    let floating = mobs(&region, EntityType::OakBoat).await;
    let y = floating[0].position[1];
    assert!(
        y > SKY as f64 + 0.3 && y < SKY as f64 + 1.0,
        "the boat floats at the surface: {y}"
    );
    let landed = mobs(&region, EntityType::BirchBoat).await;
    assert!(
        (landed[0].position[1] - (SKY as f64 + 1.0)).abs() < 1.0e-6,
        "the boat rests on the grass: {:?}",
        landed[0].position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_ride_drive_and_leave_boats_and_punches_break_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let start = [6.5, SKY as f64 + 1.0, 6.5];
    summon(&region, EntityType::OakBoat, Compound::new(), start).await;
    let boat = mobs(&region, EntityType::OakBoat).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: boat,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert_eq!(riding.vehicle, boat);
    assert_eq!(riding.riders, vec![100]);
    assert!(riding.driver);
    assert!(
        (riding.seat[1] - 0.5625 / 3.0).abs() < 1.0e-6,
        "{:?} {:?}",
        riding.seat,
        riding.kind
    );

    let to = [7.75, SKY as f64 + 1.0, 7.0];
    region
        .request(RegionRequest::Vehicle(VehicleRequest::Move {
            player: 100,
            vehicle: boat,
            position: to,
            yaw: 90.0,
            pitch: 0.0,
            on_ground: true,
        }))
        .await;
    world.ticks(2).await;
    let moved = mobs(&region, EntityType::OakBoat).await;
    assert_eq!(moved[0].position, to, "the driver moves it");

    region
        .request(RegionRequest::Vehicle(VehicleRequest::Dismount {
            player: player.clone(),
            vehicle: boat,
            width: 0.6,
            yaw: 90.0,
        }))
        .await;
    let (vehicle, position) = expect(&mut events, |event| match event {
        PlayerEvent::Dismounted { vehicle, position } => Some((vehicle, position)),
        _ => None,
    })
    .await;
    assert_eq!(vehicle, boat);
    assert!(position.iter().all(|c| c.is_finite()));
    assert!(
        crate::world::distance_sq(position, to) < 4.0,
        "off beside the boat: {position:?}"
    );

    for _ in 0..5 {
        region
            .request(RegionRequest::AttackEntity {
                attacker: player.clone(),
                target: boat,
                attack: punch(1.0),
                sweep: None,
            })
            .await;
    }
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::OakBoat)
    })
    .await;
    let items: Vec<&str> = found
        .iter()
        .filter(|s| s.kind == EntityType::Item)
        .map(|s| s.name.as_str())
        .collect();
    assert!(items.contains(&"minecraft:oak_boat"), "{items:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tamed_saddled_horse_goes_where_its_drivers_client_moves_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(200);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let start = [6.5, SKY as f64 + 1.0, 6.5];
    let nbt = Compound::new().with("Owner", tuff_nbt::Tag::uuid(77)).with(
        "equipment",
        Compound::new().with(
            "saddle",
            tuff_item::item::ItemStack::new(tuff_vanilla_data::Item::Saddle, 1).to_nbt(),
        ),
    );
    summon(&region, EntityType::Horse, nbt, start).await;
    let horse = mobs(&region, EntityType::Horse).await[0].id;

    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: horse,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert_eq!(riding.vehicle, horse);
    assert!(
        riding.driver,
        "a tamed, saddled mount's rider is its driver"
    );

    region
        .request(RegionRequest::Vehicle(VehicleRequest::Drive {
            player: 200,
            vehicle: horse,
            input: crate::world::entity::RiderInput {
                forward: 1.0,
                strafe: 0.0,
                jumping: false,
                yaw: 90.0,
                ..Default::default()
            },
        }))
        .await;
    world.ticks(3).await;
    let idle = mobs(&region, EntityType::Horse).await;
    assert!(
        crate::world::distance_sq(idle[0].position, start) < 0.01,
        "the region does not travel a mount its client drives: {:?}",
        idle[0].position
    );
    let to = [start[0] + 1.0, start[1], start[2]];
    region
        .request(RegionRequest::Vehicle(VehicleRequest::Move {
            player: 200,
            vehicle: horse,
            position: to,
            yaw: 90.0,
            pitch: 0.0,
            on_ground: true,
        }))
        .await;
    world.ticks(2).await;
    let moved = mobs(&region, EntityType::Horse).await;
    assert_eq!(moved[0].position, to, "the driver's client moves it");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_untamed_horse_ignores_steering_input() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(201);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let start = [6.5, SKY as f64 + 1.0, 6.5];
    summon(&region, EntityType::Horse, Compound::new(), start).await;
    let horse = mobs(&region, EntityType::Horse).await[0].id;

    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: horse,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert!(
        !riding.driver,
        "an untamed, unsaddled horse does not respond to steering"
    );

    region
        .request(RegionRequest::Vehicle(VehicleRequest::Drive {
            player: 201,
            vehicle: horse,
            input: crate::world::entity::RiderInput {
                forward: 1.0,
                strafe: 0.0,
                jumping: false,
                yaw: 90.0,
                ..Default::default()
            },
        }))
        .await;
    world.ticks(20).await;
    let horse = mobs(&region, EntityType::Horse).await;
    assert!(
        crate::world::distance_sq(horse[0].position, start) < 4.0,
        "unsteered horses only wander, never bolt in a straight line: {:?}",
        horse[0].position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boat_items_place_boats_on_water() {
    use crate::world::messages::{ActionOutcome, BoatPlace, HandEffect};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 6..=10 {
        for z in 6..=10 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Water.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    region
        .request(RegionRequest::PlaceBoat(BoatPlace {
            player: player.clone(),
            eye: [8.5, SKY as f64 + 2.6, 5.5],
            look: [0.0, -0.8, 0.6],
            reach: 4.5,
            yaw: 0.0,
            kind: EntityType::SpruceBoat,
            item: Item::SpruceBoat,
            name: None,
            sequence: 3,
            off_hand: false,
        }))
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 3,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(
        outcome,
        ActionOutcome::Used {
            effect: HandEffect::Consume(1),
            ..
        }
    ));
    let boats = mobs(&region, EntityType::SpruceBoat).await;
    assert_eq!(boats.len(), 1);
    assert!(
        (boats[0].position[1] - (SKY as f64 + 8.0 / 9.0)).abs() < 0.01,
        "on the water surface: {:?}",
        boats[0].position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_item_is_refused_while_the_eye_is_inside_another_boat() {
    use crate::world::messages::{ActionOutcome, BoatPlace};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 4..=12 {
        for z in 4..=12 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Water.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::OakBoat,
        Compound::new(),
        [8.5, SKY as f64 + 0.9, 8.5],
    )
    .await;
    region
        .request(RegionRequest::PlaceBoat(BoatPlace {
            player: player.clone(),
            eye: [8.5, SKY as f64 + 1.2, 8.5],
            look: [0.0, -1.0, 0.0],
            reach: 4.5,
            yaw: 0.0,
            kind: EntityType::SpruceBoat,
            item: Item::SpruceBoat,
            name: None,
            sequence: 4,
            off_hand: false,
        }))
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 4,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Rejected(_)));
    assert!(mobs(&region, EntityType::SpruceBoat).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnecting_rider_remounts_the_same_vehicle_on_rejoin() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let start = [6.5, SKY as f64 + 1.0, 6.5];
    summon(&region, EntityType::OakBoat, Compound::new(), start).await;
    let boat = mobs(&region, EntityType::OakBoat).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: boat,
            interaction: interaction(&player, None),
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;

    const RIDER_UUID: u128 = 0xF00D;
    region
        .request(RegionRequest::Vehicle(VehicleRequest::LeaveOffline {
            player: 100,
            vehicle: boat,
            uuid: RIDER_UUID,
        }))
        .await;
    // The vehicle still exists (unlike a real dismount), riderless for now.
    let after_leave = mobs(&region, EntityType::OakBoat).await;
    assert_eq!(after_leave.len(), 1);

    // Saving and reloading must not lose the offline rider.
    let (rejoined, mut rejoined_events) = PlayerHandle::new(200);
    region
        .request(RegionRequest::ReclaimVehicle {
            player: rejoined.clone(),
            uuid: RIDER_UUID,
        })
        .await;
    let riding = expect(&mut rejoined_events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert_eq!(riding.vehicle, boat);
    assert_eq!(riding.riders, vec![200]);

    // A stale UUID (already reclaimed, or never mounted) finds nothing.
    let (nobody, mut nobody_events) = PlayerHandle::new(300);
    region
        .request(RegionRequest::ReclaimVehicle {
            player: nobody.clone(),
            uuid: RIDER_UUID,
        })
        .await;
    world.ticks(1).await;
    assert!(
        nobody_events.try_recv().is_err(),
        "no vehicle left to claim"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_item_places_on_a_rail_and_a_rider_can_get_on() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let rail = BlockPos::new(6, SKY + 1, 6);
    place_blocks(
        &world,
        &region,
        vec![(
            rail,
            Block::Rail
                .default_state()
                .with_shape(tuff_vanilla_data::block::Shape::EastWest)
                .unwrap(),
        )],
    )
    .await;
    region
        .request(RegionRequest::Place(place(
            &player,
            rail,
            Item::Minecart,
            1,
        )))
        .await;
    let placed = wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::Minecart)
    })
    .await;
    let cart = placed
        .iter()
        .find(|s| s.kind == EntityType::Minecart)
        .unwrap();
    assert!(
        (cart.position[1] - (SKY as f64 + 1.0 + 0.0625)).abs() < 1.0e-6,
        "{:?}",
        cart.position
    );
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: cart.id,
            interaction: interaction(&player, None),
        })
        .await;
    let riding = expect(&mut events, |event| match event {
        PlayerEvent::Riding(riding) => Some(riding),
        _ => None,
    })
    .await;
    assert_eq!(riding.vehicle, cart.id);
    assert!(!riding.driver, "vanilla drives minecarts server-side");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_minecart_follows_straight_rail_and_rolls_down_a_slope() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let mut blocks = Vec::new();
    for x in 5..=7 {
        blocks.push((
            BlockPos::new(x, SKY + 1, 6),
            Block::Rail
                .default_state()
                .with_shape(tuff_vanilla_data::block::Shape::EastWest)
                .unwrap(),
        ));
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Minecart,
        Compound::new().with(
            "Motion",
            vec![Tag::Double(0.1), Tag::Double(0.0), Tag::Double(0.0)],
        ),
        [6.0, SKY as f64 + 1.0 + 0.0625, 6.5],
    )
    .await;
    world.ticks(5).await;
    let rolling = mobs(&region, EntityType::Minecart).await;
    assert_eq!(rolling.len(), 1);
    assert!(
        (rolling[0].position[2] - 6.5).abs() < 1.0e-9,
        "snapped onto the rail's centerline: {:?}",
        rolling[0].position
    );
    assert!(
        rolling[0].position[0] > 6.0,
        "moving east along the rail: {:?}",
        rolling[0].position
    );

    let slope = BlockPos::new(8, SKY + 1, 10);
    place_blocks(
        &world,
        &region,
        vec![
            (BlockPos::new(9, SKY + 1, 10), Block::Stone.default_state()),
            (BlockPos::new(9, SKY + 2, 10), Block::Rail.default_state()),
            (slope, Block::Rail.default_state()),
        ],
    )
    .await;
    assert_eq!(
        block_at(&region, slope).await.and_then(|s| s.shape()),
        Some(tuff_vanilla_data::block::Shape::AscendingEast)
    );
    summon(
        &region,
        EntityType::Minecart,
        Compound::new(),
        [8.5, SKY as f64 + 1.5625, 10.5],
    )
    .await;
    let stopped = mobs(&region, EntityType::Minecart).await;
    let still = stopped
        .iter()
        .find(|s| (s.position[2] - 10.5).abs() < 0.4)
        .unwrap()
        .id;
    world.ticks(20).await;
    let after = mobs(&region, EntityType::Minecart)
        .await
        .into_iter()
        .find(|s| s.id == still)
        .unwrap();
    assert!(
        after.position[0] < 8.0,
        "an unpowered cart at rest slides down its own slope: {:?}",
        after.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_spawn_on_the_ground_within_the_respawn_radius() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 6..=8 {
        for z in 6..=8 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Water.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let center = BlockPos::new(7, SKY + 5, 7);
    for offset in [0, 3, 17, 400] {
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::FindSpawn {
                center,
                radius: 3,
                offset,
                reply,
            })
            .await;
        let found = answer.await.unwrap();
        assert_eq!(found[1], SKY as f64 + 1.0, "on the grass: {found:?}");
        assert!((found[0] - 7.5).abs() <= 3.0 && (found[2] - 7.5).abs() <= 3.0);
        let over_water = (6.0..9.0).contains(&found[0]) && (6.0..9.0).contains(&found[2]);
        assert!(!over_water, "never in the pool: {found:?}");
    }
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::FindSpawn {
            center,
            radius: 0,
            offset: 0,
            reply,
        })
        .await;
    assert_eq!(
        answer.await.unwrap(),
        [7.5, SKY as f64 + 1.0, 7.5],
        "with no radius, the water column falls back to the spawn's own column"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cushion_item_places_one_cushion_on_a_top_face_and_not_twice_in_a_spot() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let floor = BlockPos::new(6, SKY, 6);
    for sequence in [1, 2] {
        region
            .request(RegionRequest::Place(place(
                &player,
                floor,
                Item::RedCushion,
                sequence,
            )))
            .await;
        world.ticks(3).await;
    }
    assert_eq!(mobs(&region, EntityType::Cushion).await.len(), 1);
    world.shutdown().await;
}
