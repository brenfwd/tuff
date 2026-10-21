//! Area broadcasts and queries reaching across region borders.

use super::*;
use crate::world::area;
use crate::world::region_api::RegionTask;
use crate::world::region_api::RegionWorld;

const WEST: ChunkPos = ChunkPos::new(31, 0);
const EAST: ChunkPos = ChunkPos::new(32, 0);

/// A box straddling the border between the west and east regions.
fn across() -> area::Area {
    area::around([512.0, 64.0, 8.0], 4.0)
}

async fn two_regions(world: &TestWorld) -> (RegionHandle, RegionHandle) {
    let west = world.region(WEST.region()).await;
    let east = world.region(EAST.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&west, &player, &mut events, &[WEST]).await;
    subscribe(&east, &player, &mut events, &[EAST]).await;
    world.ticks(2).await;
    (west, east)
}

async fn run_on(region: &RegionHandle, run: impl FnOnce(&mut dyn RegionWorld) + Send + 'static) {
    region
        .request(RegionRequest::Run(RegionTask::new(run)))
        .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broadcast_runs_on_the_other_regions_on_the_next_tick() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west, _east) = two_regions(&world).await;
    let (sent, ran) = std::sync::mpsc::channel();
    let (started, start) = oneshot::channel();
    run_on(&west, move |world| {
        started.send(world.tick()).unwrap();
        area::broadcast(world, across(), move |world| {
            sent.send((world.region_pos(), world.tick())).unwrap();
        });
    })
    .await;
    let start = start.await.unwrap();
    world.settle(start).await;
    world.ticks(3).await;
    let ran: Vec<_> = ran.try_iter().collect();
    assert_eq!(
        ran,
        [(EAST.region(), start + 1)],
        "only the neighbour runs it, on the tick after it was sent"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_query_reads_every_region_on_one_tick_and_answers_the_tick_after() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west, _east) = two_regions(&world).await;
    let (started, start) = oneshot::channel();
    let (answered, answer) = oneshot::channel();
    run_on(&west, move |world| {
        started.send(world.tick()).unwrap();
        area::query(
            world,
            across(),
            |world| (world.region_pos(), world.tick()),
            move |world, answers| {
                answered
                    .send((world.region_pos(), world.tick(), answers))
                    .unwrap();
            },
        );
    })
    .await;
    let start = start.await.unwrap();
    world.settle(start).await;
    world.ticks(4).await;
    let (asker, tick, answers) = answer.await.unwrap();
    assert_eq!(asker, WEST.region());
    assert_eq!(tick, start + 2, "answered the tick after the reads");
    assert_eq!(
        answers,
        [(WEST.region(), start + 1), (EAST.region(), start + 1)],
        "the asker's own answer first, all read on the same tick"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_query_inside_one_region_answers_from_it_alone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west, _east) = two_regions(&world).await;
    let (answered, answer) = oneshot::channel();
    run_on(&west, move |world| {
        area::query(
            world,
            area::around([100.0, 64.0, 100.0], 8.0),
            |world| world.region_pos(),
            move |_, answers| answered.send(answers).unwrap(),
        );
    })
    .await;
    world.ticks(3).await;
    assert_eq!(answer.await.unwrap(), [WEST.region()]);
    world.shutdown().await;
}

static ALERTED: &[tuff_vanilla_data::registry::EntityType] =
    &[tuff_vanilla_data::registry::EntityType::Zombie];

async fn summon_zombie(region: &RegionHandle, position: [f64; 3]) -> i32 {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:zombie")
                .with("NoAI", true),
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let (id, found) = oneshot::channel();
    run_on(region, move |world| {
        id.send(world.entity_ids()).unwrap();
    })
    .await;
    found.await.unwrap()[0]
}

fn target_of(world: &mut dyn RegionWorld, id: i32) -> Option<i32> {
    world.entity(id).unwrap().entity.living().unwrap().target
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_alert_reaches_a_neighbours_mob_on_the_next_tick_and_a_local_one_at_once() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west, east) = two_regions(&world).await;
    let near_west = summon_zombie(&west, [510.5, 64.0, 8.5]).await;
    let near_east = summon_zombie(&east, [514.5, 64.0, 8.5]).await;
    let (started, start) = oneshot::channel();
    let (local, local_target) = oneshot::channel();
    run_on(&west, move |world| {
        started.send(world.tick()).unwrap();
        let area = [506.0, 60.0, 4.0, 518.0, 70.0, 12.0];
        area::broadcast(world, across(), move |world| {
            world.alert_entities(ALERTED, area, 7);
        });
        world.alert_entities(ALERTED, area, 7);
        local.send(target_of(world, near_west)).unwrap();
    })
    .await;
    let start = start.await.unwrap();
    assert_eq!(
        local_target.await.unwrap(),
        Some(7),
        "same region, same tick"
    );
    world.settle(start).await;
    let (before, after) = (oneshot::channel(), oneshot::channel());
    world.ticks(1).await;
    run_on(&east, move |world| {
        before
            .0
            .send((world.tick(), target_of(world, near_east)))
            .unwrap();
    })
    .await;
    let (tick, target) = before.1.await.unwrap();
    assert!(tick > start);
    assert_eq!(target, Some(7), "neighbour alerted by tick N+1");
    run_on(&east, move |world| {
        after.0.send(target_of(world, near_east)).unwrap();
    })
    .await;
    assert_eq!(after.1.await.unwrap(), Some(7));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mobs_hurt_sound_reaches_a_player_in_the_neighbouring_region() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(WEST.region()).await;
    let east = world.region(EAST.region()).await;
    let (keeper, mut keeper_events) = PlayerHandle::new(200);
    subscribe(&west, &keeper, &mut keeper_events, &[WEST]).await;
    let zombie = summon_zombie(&west, [510.5, 64.0, 8.5]).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&east, &player, &mut events, &[EAST]).await;
    east.request(update(&player, 1, [518.5, 64.0, 8.5])).await;
    world.ticks(2).await;
    run_on(&west, move |world| {
        world.hurt_entity(zombie, &tuff_vanilla_data::damage_type::GENERIC, 1.0, None);
    })
    .await;
    world.ticks(4).await;
    let hurt = tuff_vanilla_data::registry::SoundEvent::EntityZombieHurt;
    expect(&mut events, |event| match event {
        PlayerEvent::Packets(packets) => packets.iter().find_map(|frame| {
            let mut r = framed_payload(frame);
            if r.read_var_int().unwrap() != tuff_vanilla_data::packet::play::Clientbound::Sound.id()
            {
                return None;
            }
            (r.read_var_int().unwrap() == hurt.id() as i32 + 1).then_some(())
        }),
        _ => None,
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entity_uuid_is_unique_level_wide_until_released() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    assert!(world.shared.claim_entity_uuid(7));
    assert!(!world.shared.claim_entity_uuid(7));
    assert!(world.shared.claim_entity_uuid(8));
    world.shared.release_entity_uuid(7);
    assert!(world.shared.claim_entity_uuid(7));
    world.shutdown().await;
}
