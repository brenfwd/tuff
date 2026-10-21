//! A raid started next to a bell, ticked through its first waves.

use super::*;
use crate::world::messages::RegionInternal;
use crate::world::region_api::{RegionTask, RegionWorld};
use tuff_feature_builtin_vanilla_mobs::world::region::raids::{DebugAction, debug};
use tuff_vanilla_data::registry::EntityType;

async fn run_on<T: Send + 'static>(
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

async fn check(region: &RegionHandle, at: BlockPos) -> Vec<String> {
    let (reply, answer) = oneshot::channel();
    run_on(region, move |world| {
        debug(world, at, DebugAction::Check, reply)
    })
    .await;
    let reply = answer.await.unwrap();
    reply.messages.into_iter().chain(reply.failure).collect()
}

const RAIDERS: [EntityType; 6] = [
    EntityType::Pillager,
    EntityType::Vindicator,
    EntityType::Evoker,
    EntityType::Witch,
    EntityType::Ravager,
    EntityType::Illusioner,
];

async fn raiders(region: &RegionHandle) -> Vec<(i32, EntityType, [i32; 3])> {
    run_on(region, |world| {
        let mut found: Vec<_> = world
            .entity_ids()
            .into_iter()
            .filter_map(|id| world.entity(id).map(|e| (id, e)))
            .filter(|(_, e)| RAIDERS.contains(&e.entity.base().kind))
            .map(|(id, e)| {
                let base = e.entity.base();
                let p = base.position;
                (
                    id,
                    base.kind,
                    [
                        p[0].floor() as i32,
                        p[1].floor() as i32,
                        p[2].floor() as i32,
                    ],
                )
            })
            .collect();
        found.sort_by_key(|r| r.0);
        found
    })
    .await
}

async fn kill_all(region: &RegionHandle, ids: Vec<i32>) {
    run_on(region, move |world| {
        for id in ids {
            world.kill_entity(id);
        }
    })
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raid_by_a_bell_runs_its_waves_and_ends_after_losing_the_village() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let middle = ChunkPos::new(8, 8);
    let region = world.region(middle.region()).await;
    let (alice, mut events) = PlayerHandle::new(10_000);
    let mut chunks = Vec::new();
    for dx in -4..=4 {
        for dz in -4..=4 {
            chunks.push(ChunkPos::new(middle.x + dx, middle.z + dz));
        }
    }
    subscribe(&region, &alice, &mut events, &chunks).await;
    let ground = Generator::new(1).height(136, 136);
    let bell = BlockPos::new(136, ground + 1, 136);
    place_blocks(&world, &region, [(bell, Block::Bell.default_state())]).await;
    world.ticks(2).await;
    region.internal(RegionInternal::PoiTickets(vec![(
        bell,
        tuff_core::world::poi::Ticket::Take,
    )]));
    world.ticks(2).await;
    let (reply, answer) = oneshot::channel();
    run_on(&region, move |world| {
        debug(world, bell, DebugAction::Start(0), reply)
    })
    .await;
    let started = answer.await;
    assert_eq!(
        started.unwrap().messages,
        ["Created a raid in your local village"]
    );
    let mut log = Vec::new();
    for _ in 0..9 {
        world.ticks(100).await;
        log.push(check(&region, bell).await.join(" | "));
        let wave = raiders(&region).await;
        log.push(format!(
            "{:?}",
            wave.iter().map(|r| (r.1, r.2)).collect::<Vec<_>>()
        ));
        kill_all(&region, wave.into_iter().map(|r| r.0).collect()).await;
    }
    region.internal(RegionInternal::PoiTickets(vec![(
        bell,
        tuff_core::world::poi::Ticket::Release,
    )]));
    world.ticks(40).await;
    log.push(check(&region, bell).await.join(" | "));
    world.ticks(600).await;
    log.push(check(&region, bell).await.join(" | "));
    let expected = [
        "Found a started raid! | Num groups spawned: 0 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 0.0",
        "[]",
        "Found a started raid! | Num groups spawned: 0 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 0.0",
        "[]",
        "Found a started raid! | Num groups spawned: 1 Raid omen level: 0 Num mobs: 4 Raid health: 96.0 / 96.0",
        "[(Pillager, [75, 72, 194]), (Pillager, [75, 72, 194]), (Pillager, [75, 72, 194]), (Pillager, [75, 72, 194])]",
        "Found a started raid! | Num groups spawned: 1 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 96.0",
        "[]",
        "Found a started raid! | Num groups spawned: 1 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 96.0",
        "[]",
        "Found a started raid! | Num groups spawned: 1 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 96.0",
        "[]",
        "Found a started raid! | Num groups spawned: 2 Raid omen level: 0 Num mobs: 6 Raid health: 144.0 / 144.0",
        "[(Vindicator, [98, 73, 204]), (Vindicator, [71, 69, 201]), (Pillager, [71, 69, 201]), (Pillager, [71, 69, 201]), (Pillager, [71, 69, 201]), (Pillager, [71, 69, 201])]",
        "Found a started raid! | Num groups spawned: 2 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 144.0",
        "[]",
        "Found a started raid! | Num groups spawned: 2 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 144.0",
        "[]",
        "Found a started raid! | Num groups spawned: 2 Raid omen level: 0 Num mobs: 0 Raid health: -0.0 / 144.0",
        "Found no started raids",
    ];
    assert_eq!(log, expected);
    world.shutdown().await;
}
