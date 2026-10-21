//! Mobs holding spears charge their target and stab it, riders at the
//! speed of what they ride.

use super::*;
use crate::world::messages::{PlayerLooks, RideAnswer, RideOp, RidePassenger};
use tuff_feature_builtin_vanilla_mobs::spawning::SpawnReason;
use tuff_vanilla_data::registry::EntityType;

use crate::world::region_api::{RegionTask, RegionWorld};

const Y: f64 = SKY as f64 + 1.0;

async fn arena(
    roofed: bool,
) -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let chunks: Vec<ChunkPos> = (0..2)
        .flat_map(|x| (0..2).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(chunks[0].region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &chunks).await;
    let blocks = if roofed {
        roofed_pen(2, 29)
    } else {
        pen(2, 29)
    };
    place_blocks(&world, &region, blocks).await;
    (dir, world, region, player, events)
}

fn is_spear_hit(hit: &crate::world::entity::EntityHit) -> bool {
    hit.kind.name.contains("spear")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_foot_zombie_with_a_spear_stabs_instead_of_swinging() {
    let (_dir, world, region, player, mut events) = arena(true).await;
    let held = Compound::new().with(
        "equipment",
        Compound::new().with(
            "mainhand",
            Compound::new()
                .with("id", "minecraft:iron_spear")
                .with("count", 1),
        ),
    );
    summon(&region, EntityType::Zombie, held, [4.5, Y, 4.5]).await;
    region
        .request(player_at(&player, [18.5, Y, 4.5], PlayerLooks::default()))
        .await;
    let hit = ticking_expect(&world, &mut events, 600, |event| match event {
        PlayerEvent::EntityHurt(hit) => Some(*hit),
        _ => None,
    })
    .await;
    assert!(is_spear_hit(&hit), "{:?}", hit.kind.name);
    assert_eq!(hit.attacker.unwrap().kind, EntityType::Zombie);
    assert!(hit.amount >= 1.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_husk_on_a_camel_husk_charges_a_player_with_its_spear() {
    let (_dir, world, region, player, mut events) = arena(false).await;
    region_run(&region, |world| {
        for i in 0..60 {
            let at = [
                3.5 + f64::from(i % 6) * 2.0,
                Y,
                3.5 + f64::from(i / 6) * 2.0,
            ];
            tuff_feature_builtin_vanilla_mobs::world::region::spawning::spawn_fresh(
                world,
                EntityType::Husk,
                at,
                0.0,
                0,
                SpawnReason::Natural,
            );
        }
    })
    .await;
    world.ticks(2).await;
    let riders = region_run(&region, |world| {
        world
            .entity_ids()
            .into_iter()
            .filter(|id| {
                world.entity(*id).is_some_and(|e| {
                    e.entity.base().kind == EntityType::Husk
                        && e.entity.base().vehicle.as_ref().is_some_and(|v| {
                            world
                                .entity(v.id)
                                .is_some_and(|c| c.entity.base().kind == EntityType::CamelHusk)
                        })
                })
            })
            .collect::<Vec<i32>>()
    })
    .await;
    assert!(!riders.is_empty(), "a husk rode a camel husk");
    region
        .request(player_at(&player, [24.5, Y, 24.5], PlayerLooks::default()))
        .await;
    let hit = ticking_expect(&world, &mut events, 1200, |event| match event {
        PlayerEvent::EntityHurt(hit)
            if is_spear_hit(&hit)
                && hit
                    .attacker
                    .as_ref()
                    .is_some_and(|a| riders.contains(&a.id)) =>
        {
            Some(*hit)
        }
        _ => None,
    })
    .await;
    assert_eq!(hit.attacker.unwrap().kind, EntityType::Husk);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_charging_spear_zombie_knocks_a_rider_off_its_mount_and_wears_its_spear() {
    let (_dir, world, region, player, mut events) = arena(true).await;
    let at = [18.5, Y, 4.5];
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        at,
    )
    .await;
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    let passenger = RidePassenger::Player {
        handle: player.clone(),
        uuid: 1000,
    };
    let mounted = region
        .ask(move |world| {
            world.ride(RideOp::Mount {
                passenger,
                vehicle: cow,
            })
        })
        .await
        .unwrap();
    assert!(matches!(mounted, RideAnswer::Vehicle(v) if v.id == cow));
    let held = Compound::new().with(
        "equipment",
        Compound::new().with(
            "mainhand",
            Compound::new()
                .with("id", "minecraft:iron_spear")
                .with("count", 1),
        ),
    );
    summon(&region, EntityType::Zombie, held, [4.5, Y, 4.5]).await;
    ticking_expect(&world, &mut events, 900, |event| {
        matches!(event, PlayerEvent::StabDismounted).then_some(())
    })
    .await;
    let worn = region_run(&region, |world| {
        world.entity_ids().into_iter().find_map(|id| {
            let e = world.entity(id)?;
            (e.entity.base().kind == EntityType::Zombie).then(|| {
                e.entity
                    .equipment()
                    .into_iter()
                    .find(|(slot, _)| *slot == 0)
                    .and_then(|(_, s)| s.map(|s| s.damage()))
            })
        })
    })
    .await;
    assert!(worn.flatten().is_some_and(|d| d > 0), "{worn:?}");
    world.shutdown().await;
}

async fn region_run<T: Send + 'static>(
    region: &RegionHandle,
    run: impl FnOnce(&mut dyn RegionWorld) -> T + Send + 'static,
) -> T {
    let (sent, got) = std::sync::mpsc::channel();
    region
        .request(RegionRequest::Run(RegionTask::new(move |world| {
            sent.send(run(world)).unwrap();
        })))
        .await;
    got.recv().unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stab_dismount_request_unseats_a_mob_rider() {
    let (_dir, world, region, _player, _events) = arena(false).await;
    let at = [6.5, Y, 6.5];
    let no_ai = || Compound::new().with("NoAI", true);
    summon(&region, EntityType::Cow, no_ai(), at).await;
    summon(&region, EntityType::Zombie, no_ai(), at).await;
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let mounted = region
        .ask(move |world| {
            world.ride(RideOp::Mount {
                passenger: RidePassenger::Entity(zombie),
                vehicle: cow,
            })
        })
        .await
        .unwrap();
    assert!(matches!(mounted, RideAnswer::Vehicle(v) if v.id == cow));
    let vehicle_of = |id| {
        region_run(&region, move |world| {
            world
                .entity(id)
                .and_then(|e| e.entity.base().vehicle.is_some().then_some(()))
        })
    };
    assert!(vehicle_of(zombie).await.is_some());
    region
        .request(RegionRequest::StabDismount { target: zombie })
        .await;
    world.ticks(2).await;
    assert_eq!(vehicle_of(zombie).await, None);
    region
        .request(RegionRequest::StabDismount { target: cow })
        .await;
    world.ticks(1).await;
    assert!(mobs(&region, EntityType::Cow).await.len() == 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stab_knock_request_pushes_a_mob_without_hurting_it() {
    let (_dir, world, region, _player, _events) = arena(false).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        [6.5, Y, 6.5],
    )
    .await;
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    let start = region_run(&region, move |world| {
        world.entity(cow).map(|e| e.entity.base().position)
    })
    .await
    .unwrap();
    region
        .request(RegionRequest::StabKnock {
            target: cow,
            power: 0.4,
            dx: -1.0,
            dz: 0.0,
        })
        .await;
    world.ticks(3).await;
    let end = region_run(&region, move |world| {
        world.entity(cow).unwrap().entity.base().position
    })
    .await;
    assert!(end[0] > start[0] + 0.05, "{start:?} {end:?}");
    world.shutdown().await;
}
