//! Region entity rules: duplicate UUIDs, item owners, summoning types no
//! plugin implements, and entities other than players using portals.

use super::*;
use crate::storage::level::THE_END;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::EntityType;

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect()
}

async fn snapshots(region: &RegionHandle) -> Vec<crate::world::messages::EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

async fn summon(region: &RegionHandle, nbt: Compound, position: [f64; 3]) -> bool {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position,
            fresh: false,
            reply,
        })
        .await;
    summoned.await.unwrap()
}

async fn watch(
    region: &RegionHandle,
    pos: ChunkPos,
) -> (PlayerHandle, mpsc::UnboundedReceiver<PlayerEvent>) {
    let (watcher, mut events) = PlayerHandle::new(900);
    subscribe(region, &watcher, &mut events, &[pos]).await;
    (watcher, events)
}

fn offered(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> bool {
    let mut found = false;
    while let Ok(event) = events.try_recv() {
        found |= matches!(event, PlayerEvent::OfferItem { .. });
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summoning_a_taken_uuid_fails() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let _watcher = watch(&region, ChunkPos::new(0, 0)).await;
    let line = "summon minecraft:pig 8 120 8 {NoAI:1b,UUID:[I;1,2,3,4]}";
    assert!(console(&world, line).await[0].starts_with("Summoned"));
    let failed = console(&world, line).await;
    assert!(failed[0].contains("duplicate UUIDs"), "{failed:?}");
    let nbt = Compound::new()
        .with("id", "minecraft:pig")
        .with("UUID", Tag::IntArray(vec![1, 2, 3, 4]));
    assert!(
        !summon(&region, nbt, [9.0, 120.0, 9.0]).await,
        "the region refuses a second entity with the same UUID"
    );
    let pigs = snapshots(&region)
        .await
        .into_iter()
        .filter(|e| e.kind == EntityType::Pig)
        .count();
    assert_eq!(pigs, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn types_without_their_own_entity_can_be_summoned() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let _watcher = watch(&region, ChunkPos::new(0, 0)).await;
    for (kind, name) in [
        (EntityType::BlockDisplay, "block_display"),
        (EntityType::Marker, "marker"),
        (EntityType::Interaction, "interaction"),
    ] {
        let line = format!("summon minecraft:{name} 8 100 8 {{Tags:[\"kept\"]}}");
        let answer = console(&world, &line).await;
        assert!(answer[0].starts_with("Summoned"), "{name}: {answer:?}");
        let found = snapshots(&region)
            .await
            .into_iter()
            .find(|e| e.kind == kind)
            .unwrap_or_else(|| panic!("{name} is in the region"));
        assert_eq!(found.tags, vec!["kept".to_owned()]);
        assert_eq!(found.position, [8.5, 100.0, 8.5]);
    }
    let failed = console(&world, "summon minecraft:player 8 100 8").await;
    assert!(!failed[0].starts_with("Summoned"), "{failed:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owned_item_is_only_offered_to_its_owner() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let y = Generator::new(1).height(40, 40) + 1;
    let at = [40.5, y as f64, 40.5];
    region.request(update(&viewer, 1, at)).await;
    let item = |owner: u128| {
        Compound::new()
            .with("id", "minecraft:item")
            .with(
                "Item",
                Compound::new()
                    .with("id", "minecraft:stone")
                    .with("count", 1),
            )
            .with("PickupDelay", 0i16)
            .with("Owner", Tag::uuid(owner))
    };
    assert!(summon(&region, item(7), at).await);
    for _ in 0..3 {
        world.ticks(1).await;
        assert!(!offered(&mut events), "someone else's item is not offered");
    }
    assert!(summon(&region, item(100), at).await);
    world.ticks(1).await;
    assert!(offered(&mut events), "the owner is offered their item");
    world.shutdown().await;
}

/// An obsidian frame around a lit 2x3 nether portal running east-west,
/// whose bottom-left portal block is `origin`.
fn lit_portal(origin: BlockPos) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut frame = Vec::new();
    let mut inside = Vec::new();
    for dx in -1..=2 {
        for dy in -1..=3 {
            let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z);
            let edge = dx == -1 || dx == 2 || dy == -1 || dy == 3;
            if !edge {
                inside.push((
                    pos,
                    tuff_feature_builtin_vanilla_dimensions::world::portal::Axis::X.portal_state(),
                ));
            } else if !((dx == -1 || dx == 2) && (dy == -1 || dy == 3)) {
                frame.push((pos, Block::Obsidian.default_state()));
            }
        }
    }
    frame.extend(inside);
    frame
}

async fn find_in(
    world: &TestWorld,
    dimension: &str,
    kind: EntityType,
) -> Option<crate::world::messages::EntitySnapshot> {
    for x in -1..=0 {
        for z in -1..=0 {
            let region = world.region_in(dimension, RegionPos::new(x, z)).await;
            if let Some(found) = snapshots(&region)
                .await
                .into_iter()
                .find(|e| e.kind == kind)
            {
                return Some(found);
            }
        }
    }
    None
}

async fn tick_until_found(
    world: &TestWorld,
    dimension: &str,
    kind: EntityType,
) -> Option<crate::world::messages::EntitySnapshot> {
    for _ in 0..200 {
        if let Some(found) = find_in(world, dimension, kind).await {
            return Some(found);
        }
        world.ticks(1).await;
    }
    None
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_in_a_nether_portal_goes_to_the_nether() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let height = Generator::new(1).height(8, 8);
    let origin = BlockPos::new(8, height + 1, 8);
    place_blocks(&world, &region, lit_portal(origin)).await;
    let pig = Compound::new()
        .with("id", "minecraft:pig")
        .with("NoAI", true);
    let at = [9.0, f64::from(origin.y), 8.5];
    assert!(summon(&region, pig, at).await);
    let arrived = tick_until_found(&world, THE_NETHER, EntityType::Pig)
        .await
        .expect("the pig reaches the nether");
    assert!(
        arrived.position[0].abs() < 20.0 && arrived.position[2].abs() < 20.0,
        "it comes out near the scaled position: {:?}",
        arrived.position
    );
    assert!(
        find_in(&world, OVERWORLD, EntityType::Pig).await.is_none(),
        "the pig left the overworld"
    );
    let nether = world
        .region_in(THE_NETHER, ChunkPos::of_position(arrived.position).region())
        .await;
    let at = BlockPos::new(
        arrived.position[0].floor() as i32,
        arrived.position[1].floor() as i32,
        arrived.position[2].floor() as i32,
    );
    assert_eq!(
        block_at(&nether, at).await.map(|s| s.block()),
        Some(Block::NetherPortal),
        "an exit portal was built where it arrived"
    );
    for _ in 0..40 {
        world.ticks(1).await;
    }
    assert!(
        find_in(&world, THE_NETHER, EntityType::Pig).await.is_some(),
        "the cooldown keeps it from going straight back"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vehicle_in_a_nether_portal_takes_its_passengers_along() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let height = Generator::new(1).height(8, 8);
    let origin = BlockPos::new(8, height + 1, 8);
    place_blocks(&world, &region, lit_portal(origin)).await;
    let pig = Compound::new()
        .with("id", "minecraft:pig")
        .with("NoAI", true)
        .with(
            "Passengers",
            Tag::List(vec![Tag::Compound(
                Compound::new()
                    .with("id", "minecraft:chicken")
                    .with("NoAI", true),
            )]),
        );
    let at = [9.0, f64::from(origin.y), 8.5];
    assert!(summon(&region, pig, at).await);
    let pig = tick_until_found(&world, THE_NETHER, EntityType::Pig)
        .await
        .expect("the pig reaches the nether");
    let mut chicken = None;
    for _ in 0..40 {
        chicken = find_in(&world, THE_NETHER, EntityType::Chicken).await;
        if chicken.is_some() {
            break;
        }
        world.ticks(1).await;
    }
    let chicken = chicken.expect("the rider came along");
    let apart = [0, 1, 2].map(|axis| (chicken.position[axis] - pig.position[axis]).abs());
    assert!(apart.iter().all(|d| *d < 2.5), "{apart:?}");
    assert!(find_in(&world, OVERWORLD, EntityType::Pig).await.is_none());
    assert!(
        find_in(&world, OVERWORLD, EntityType::Chicken)
            .await
            .is_none()
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_in_an_end_portal_lands_on_the_end_platform() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let portal = BlockPos::new(8, 200, 8);
    place_blocks(
        &world,
        &region,
        [(portal, Block::EndPortal.default_state())],
    )
    .await;
    let item = Compound::new()
        .with("id", "minecraft:item")
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1),
        )
        .with("NoGravity", true);
    assert!(summon(&region, item, [8.5, 200.5, 8.5]).await);
    let end = world.dimension(crate::storage::level::THE_END);
    let platform = world
        .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
        .await;
    let mut arrived = None;
    for _ in 0..200 {
        arrived = snapshots(&platform)
            .await
            .into_iter()
            .find(|e| e.kind == EntityType::Item);
        if arrived.is_some() {
            break;
        }
        world.ticks(1).await;
    }
    let arrived = arrived.expect("the item reaches the end");
    assert_eq!(arrived.dimension, end);
    assert_eq!(arrived.position, [100.5, 50.0, 0.5]);
    assert_eq!(
        block_at(&platform, BlockPos::new(100, 48, 0))
            .await
            .map(|s| s.block()),
        Some(Block::Obsidian),
        "the platform was built under it"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_leaving_the_end_lands_above_the_first_non_leaf_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let spawn = world.shared.spawn().pos;
    let column = |y| BlockPos::new(spawn.x, y, spawn.z);
    let home = world.region(ChunkPos::of_block(spawn).region()).await;
    let _home_watcher = watch(&home, ChunkPos::of_block(spawn)).await;
    place_blocks(
        &world,
        &home,
        [
            (column(250), Block::Stone.default_state()),
            (column(251), Block::OakLeaves.default_state()),
            (column(252), Block::OakLeaves.default_state()),
        ],
    )
    .await;
    let end_pos = ChunkPos::new(0, 0);
    let end = world
        .region_in(crate::storage::level::THE_END, end_pos.region())
        .await;
    let _end_watcher = watch(&end, end_pos).await;
    place_blocks(
        &world,
        &end,
        [(BlockPos::new(8, 200, 8), Block::EndPortal.default_state())],
    )
    .await;
    let item = Compound::new()
        .with("id", "minecraft:item")
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1),
        )
        .with("NoGravity", true);
    assert!(summon(&end, item, [8.5, 200.5, 8.5]).await);
    let arrived = tick_until_found(&world, OVERWORLD, EntityType::Item)
        .await
        .expect("the item comes home");
    assert_eq!(arrived.position[1], 252.0);
    world.shutdown().await;
}

async fn seat_player(region: &RegionHandle, handle: &PlayerHandle, vehicle: i32) {
    let passenger = crate::world::messages::RidePassenger::Player {
        handle: handle.clone(),
        uuid: u128::from(handle.id.unsigned_abs()),
    };
    let answer = region
        .ask(move |world| world.ride(crate::world::messages::RideOp::Mount { passenger, vehicle }))
        .await;
    assert!(matches!(
        answer,
        Some(crate::world::messages::RideAnswer::Vehicle(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vehicle_with_a_player_aboard_takes_the_player_through_a_nether_portal() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let height = Generator::new(1).height(8, 8);
    let origin = BlockPos::new(8, height + 1, 8);
    place_blocks(&world, &region, lit_portal(origin)).await;
    let pig = Compound::new()
        .with("id", "minecraft:pig")
        .with("NoAI", true);
    assert!(summon(&region, pig, [9.0, f64::from(origin.y), 8.5]).await);
    let vehicle = snapshots(&region).await[0].id;
    let (rider, mut events) = PlayerHandle::new(3000);
    seat_player(&region, &rider, vehicle).await;
    let nether = world.dimension(THE_NETHER);
    let (mut followed, mut reseated) = (false, false);
    for _ in 0..200 {
        while let Ok(event) = events.try_recv() {
            match event {
                PlayerEvent::PortalRide { dimension, .. } => followed |= dimension == nether,
                PlayerEvent::Riding(riding) => {
                    reseated |= riding.dimension == nether && riding.vehicle == vehicle;
                }
                _ => {}
            }
        }
        if reseated {
            break;
        }
        world.ticks(1).await;
    }
    assert!(followed, "the player is told to follow its vehicle");
    assert!(reseated, "the nether region seats the player again");
    assert!(find_in(&world, OVERWORLD, EntityType::Pig).await.is_none());
    let arrived = find_in(&world, THE_NETHER, EntityType::Pig)
        .await
        .expect("the vehicle is in the nether");
    assert_eq!(arrived.id, vehicle);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vehicle_leaving_the_end_with_an_unseen_credits_player_stays() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let spawn = world.shared.spawn().pos;
    let home = world.region(ChunkPos::of_block(spawn).region()).await;
    let _home_watcher = watch(&home, ChunkPos::of_block(spawn)).await;
    let end_pos = ChunkPos::new(0, 0);
    let end = world.region_in(THE_END, end_pos.region()).await;
    let _end_watcher = watch(&end, end_pos).await;
    place_blocks(
        &world,
        &end,
        [(BlockPos::new(8, 200, 8), Block::EndPortal.default_state())],
    )
    .await;
    let pig = Compound::new()
        .with("id", "minecraft:pig")
        .with("NoAI", true)
        .with("NoGravity", true);
    assert!(summon(&end, pig, [8.5, 200.5, 8.5]).await);
    let vehicle = snapshots(&end).await[0].id;
    let (rider, _events) = PlayerHandle::new(3001);
    seat_player(&end, &rider, vehicle).await;
    for _ in 0..40 {
        world.ticks(1).await;
    }
    assert!(find_in(&world, OVERWORLD, EntityType::Pig).await.is_none());
    assert!(find_in(&world, THE_END, EntityType::Pig).await.is_some());
    rider.set_seen_credits(true);
    let arrived = tick_until_found(&world, OVERWORLD, EntityType::Pig).await;
    assert!(arrived.is_some(), "it leaves once the credits were seen");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rider_session_follows_its_vehicle_into_the_new_dimension() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _frames) = session_with_outbound(&world, 3002);
    let nether = world.dimension(THE_NETHER);
    session.ride.riding = Some(Box::new(crate::world::messages::Riding {
        vehicle: 7,
        kind: EntityType::Pig,
        dimension: session.dimension_id(),
        position: [8.5, 70.0, 8.5],
        riders: vec![3002],
        seat: [0.0; 3],
        driver: true,
        steers: false,
        no_gravity: false,
        flying: false,
        ridden: None,
        given: Default::default(),
    }));
    session
        .on_event(PlayerEvent::PortalRide {
            dimension: nether,
            position: [8.5, 70.0, 8.5],
            turn: 90.0,
        })
        .await;
    assert_eq!(session.dimension_id(), nether);
    assert!(session.ride.riding.is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn getting_off_a_vehicle_starts_a_boarding_cooldown_that_ticks_down() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _frames) = session_with_outbound(&world, 3003);
    session.ride.riding = Some(Box::new(crate::world::messages::Riding {
        vehicle: 7,
        kind: EntityType::Pig,
        dimension: session.dimension_id(),
        position: [8.5, 70.0, 8.5],
        riders: vec![3003],
        seat: [0.0; 3],
        driver: true,
        steers: false,
        no_gravity: false,
        flying: false,
        ridden: None,
        given: Default::default(),
    }));
    assert_eq!(session.ride.boarding_cooldown, 0);
    session.dismounted(7, [f64::NAN; 3]).await;
    assert_eq!(session.ride.boarding_cooldown, 60);
    session.on_tick().await;
    assert_eq!(session.ride.boarding_cooldown, 59);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entity_through_a_portal_keeps_its_team() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let portal = BlockPos::new(8, 200, 8);
    place_blocks(
        &world,
        &region,
        [(portal, Block::EndPortal.default_state())],
    )
    .await;
    let holder = "00000001-0000-0002-0000-000300000004";
    console(&world, "team add blue").await;
    let item = Compound::new()
        .with("id", "minecraft:item")
        .with("UUID", Tag::IntArray(vec![1, 2, 3, 4]))
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1),
        )
        .with("NoGravity", true);
    assert!(summon(&region, item, [8.5, 200.5, 8.5]).await);
    console(&world, &format!("team join blue {holder}")).await;
    let team = || {
        world
            .shared
            .scoreboard
            .read(move |board| board.team_of(holder).map(|team| team.name.clone()))
    };
    assert_eq!(team().await.flatten().as_deref(), Some("blue"));
    let platform = world
        .region_in(crate::storage::level::THE_END, RegionPos::new(0, 0))
        .await;
    let mut arrived = false;
    for _ in 0..200 {
        arrived = snapshots(&platform)
            .await
            .iter()
            .any(|e| e.kind == EntityType::Item);
        if arrived {
            break;
        }
        world.ticks(1).await;
    }
    assert!(arrived, "the item reaches the end");
    world.ticks(5).await;
    assert_eq!(team().await.flatten().as_deref(), Some("blue"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entity_handed_to_a_region_it_is_not_in_reaches_its_own() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(31, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let item = Compound::new()
        .with("id", "minecraft:item")
        .with(
            "Item",
            Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1),
        )
        .with("NoGravity", true);
    summon(&region, item, [600.5, 200.5, 8.5]).await;
    let next = world.region(RegionPos::new(1, 0)).await;
    let mut arrived = false;
    for _ in 0..100 {
        arrived = snapshots(&next)
            .await
            .iter()
            .any(|e| e.kind == EntityType::Item);
        if arrived {
            break;
        }
        world.ticks(1).await;
    }
    assert!(arrived, "the item was handed to the region it is in");
    world.shutdown().await;
}
