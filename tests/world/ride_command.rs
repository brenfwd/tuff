//! `/ride`: mounting entities on entities (also across a region border)
//! and players on entities, refusing loops, players as vehicles and
//! passengers already riding, and dismounting.

use super::*;
use crate::world::messages::{RideAnswer, RideOp, RidePassenger};
use crate::world::session::PlayerSession;
use tuff_vanilla_data::registry::EntityType;

async fn vehicle_of(world: &TestWorld, passenger: i32) -> Option<i32> {
    for region in [RegionPos::new(0, 0), RegionPos::new(1, 0)] {
        let region = world.region(region).await;
        let answer = region
            .ask(move |world| world.ride(RideOp::VehicleOf { passenger }))
            .await;
        if let Some(RideAnswer::Vehicle(vehicle)) = answer {
            return Some(vehicle.id);
        }
    }
    None
}

async fn succeeded(
    world: &TestWorld,
    player: &mut PlayerSession,
    holder: &str,
    command: &str,
) -> bool {
    player
        .run_command(&format!(
            "execute store success score {holder} out run {command}"
        ))
        .await;
    let shared = world.shared.clone();
    let holder = holder.to_owned();
    shared
        .scoreboard
        .read(move |board| board.score(&holder, "out").map(|s| s.value))
        .await
        .flatten()
        == Some(1)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ride_mounts_refuses_and_dismounts_entities() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 29)).await;
    let ai = Compound::new().with("NoAI", true);
    summon(
        &region,
        EntityType::Pig,
        ai.clone(),
        [20.5, SKY as f64 + 1.0, 20.5],
    )
    .await;
    summon(
        &region,
        EntityType::Zombie,
        ai,
        [26.5, SKY as f64 + 1.0, 26.5],
    )
    .await;
    world.ticks(2).await;
    let pig = mobs(&region, EntityType::Pig).await[0].id;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player.pose.position = [24.5, SKY as f64 + 1.0, 24.5];
    player
        .run_command("scoreboard objectives add out dummy")
        .await;

    let zombie_on_pig = "ride @e[type=zombie,limit=1] mount @e[type=pig,limit=1]";
    assert!(succeeded(&world, &mut player, "#mount", zombie_on_pig).await);
    assert_eq!(vehicle_of(&world, zombie).await, Some(pig));
    world.ticks(1).await;
    let seated = mobs(&region, EntityType::Zombie).await[0].position;
    let below = mobs(&region, EntityType::Pig).await[0].position;
    assert!(
        (seated[0] - below[0]).abs() < 1.0e-6 && seated[1] > below[1],
        "the zombie sits on the pig: {seated:?} on {below:?}"
    );
    assert!(
        !succeeded(&world, &mut player, "#again", zombie_on_pig).await,
        "a passenger already riding is refused"
    );
    assert!(
        !succeeded(
            &world,
            &mut player,
            "#loop",
            "ride @e[type=pig,limit=1] mount @e[type=zombie,limit=1]"
        )
        .await,
        "a vehicle cannot ride its own passenger"
    );
    assert_eq!(vehicle_of(&world, pig).await, None);

    assert!(
        succeeded(
            &world,
            &mut player,
            "#off",
            "ride @e[type=zombie,limit=1] dismount"
        )
        .await
    );
    assert_eq!(vehicle_of(&world, zombie).await, None);
    assert!(
        !succeeded(
            &world,
            &mut player,
            "#none",
            "ride @e[type=zombie,limit=1] dismount"
        )
        .await,
        "nothing to get off"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ride_boards_a_passenger_from_another_region() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west_chunk = ChunkPos::new(30, 1);
    let east_chunk = ChunkPos::new(33, 1);
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, west_chunk).await;
    let (other, mut other_events) = PlayerHandle::new(1001);
    view_around(&world, &other, &mut other_events, east_chunk).await;
    let ai = Compound::new().with("NoAI", true);
    summon(
        &west,
        EntityType::Zombie,
        ai.clone(),
        [488.5, SKY as f64 + 1.0, 24.5],
    )
    .await;
    summon(
        &east,
        EntityType::Horse,
        ai,
        [536.5, SKY as f64 + 1.0, 24.5],
    )
    .await;
    world.ticks(2).await;
    let horse = mobs(&east, EntityType::Horse).await[0].id;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player
        .run_command("scoreboard objectives add out dummy")
        .await;
    assert!(
        succeeded(
            &world,
            &mut player,
            "#far",
            "ride @e[type=zombie,limit=1] mount @e[type=horse,limit=1]"
        )
        .await
    );
    assert!(mobs(&west, EntityType::Zombie).await.is_empty());
    let zombie = mobs(&east, EntityType::Zombie).await;
    assert_eq!(zombie.len(), 1, "the zombie moved to the horse's region");
    assert_eq!(vehicle_of(&world, zombie[0].id).await, Some(horse));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ride_seats_a_player_on_any_entity_and_takes_it_off() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 29)).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        [20.5, SKY as f64 + 1.0, 20.5],
    )
    .await;
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    let (rider, mut rider_events) = PlayerHandle::new(3000);
    let ask = |op: RideOp| {
        let region = region.clone();
        async move { region.ask(move |world| world.ride(op)).await.unwrap() }
    };
    let passenger = RidePassenger::Player {
        handle: rider.clone(),
        uuid: 3000,
    };
    let mounted = ask(RideOp::Mount {
        passenger: passenger.clone(),
        vehicle: cow,
    })
    .await;
    assert!(matches!(mounted, RideAnswer::Vehicle(v) if v.id == cow));
    let mut seated = false;
    while let Ok(event) = rider_events.try_recv() {
        seated |= matches!(event, PlayerEvent::Riding(riding) if riding.vehicle == cow);
    }
    assert!(
        seated,
        "a cow has no seat, but the command seats the player"
    );
    assert_eq!(vehicle_of(&world, 3000).await, Some(cow));
    let off = ask(RideOp::Dismount { passenger }).await;
    assert!(matches!(off, RideAnswer::Vehicle(v) if v.id == cow));
    let mut dismounted = false;
    while let Ok(event) = rider_events.try_recv() {
        dismounted |= matches!(event, PlayerEvent::Dismounted { vehicle, .. } if vehicle == cow);
    }
    assert!(dismounted);
    assert_eq!(vehicle_of(&world, 3000).await, None);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rider_never_dismounts_to_a_spot_outside_the_world_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let id = world.dimension("minecraft:overworld");
    world.borders[id.index()].send_replace(crate::world::border::Border::fixed([0.0, 0.0], 20.0));
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(0, 14)).await;
    let facing_out = Compound::new().with("NoAI", true).with(
        "Rotation",
        Tag::List(vec![Tag::Float(-90.0), Tag::Float(0.0)]),
    );
    summon(
        &region,
        EntityType::OakBoat,
        facing_out,
        [9.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player.pose.position = [5.5, SKY as f64 + 1.0, 8.5];
    player
        .run_command("ride @e[type=zombie,limit=1] mount @e[type=oak_boat,limit=1]")
        .await;
    world.ticks(1).await;
    player
        .run_command("ride @e[type=zombie,limit=1] dismount")
        .await;
    world.ticks(1).await;
    let landed = mobs(&region, EntityType::Zombie).await[0].position;
    assert!(landed[0] < 10.0, "stays inside the border: {landed:?}");
    world.shutdown().await;
}
