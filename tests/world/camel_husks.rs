//! Camel husks: the undead camel that never breeds, stays once a player
//! has touched it, and carries a spear-wielding husk and a parched when a
//! husk spawns naturally.

use super::*;
use crate::world::region_api::{RegionTask, RegionWorld};
use tuff_feature_builtin_vanilla_mobs::spawning::SpawnReason;
use tuff_vanilla_data::registry::EntityType;

const Y: f64 = SKY as f64 + 1.0;

async fn start(
    chunks: &[ChunkPos],
    size: i32,
) -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(chunks[0].region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, chunks).await;
    place_blocks(&world, &region, pen(2, size)).await;
    (dir, world, region, player, events)
}

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

/// Every camel husk's riders, first (the one steering) first, with what
/// each holds in its main hand.
async fn camel_crews(region: &RegionHandle) -> Vec<Vec<(EntityType, Option<String>)>> {
    run_on(region, |world| {
        let mut crews = Vec::new();
        for id in world.entity_ids() {
            let Some(camel) = world.entity(id) else {
                continue;
            };
            if camel.entity.base().kind != EntityType::CamelHusk {
                continue;
            }
            let riders = camel.entity.base().entity_riders.clone();
            let crew = riders
                .into_iter()
                .filter_map(|rider| world.entity(rider))
                .map(|rider| {
                    let saved = crate::world::entity::save(&*rider.entity);
                    let held = saved
                        .get_compound("equipment")
                        .and_then(|e| e.get_compound("mainhand"))
                        .and_then(|m| m.get_str("id"))
                        .map(str::to_owned);
                    (rider.entity.base().kind, held)
                })
                .collect();
            crews.push(crew);
        }
        crews
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn naturally_spawned_husks_sometimes_ride_a_camel_husk_with_a_parched() {
    let chunks: Vec<ChunkPos> = (0..3)
        .flat_map(|x| (0..3).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let (_dir, world, region, _player, _events) = start(&chunks, 45).await;
    for x in 0..20 {
        let at = [3.5 + 2.0 * f64::from(x), Y, 3.5];
        summon(&region, EntityType::Husk, Compound::new(), at).await;
    }
    world.ticks(2).await;
    assert!(
        mobs(&region, EntityType::CamelHusk).await.is_empty(),
        "summoned husks come alone"
    );

    let spawned = run_on(&region, |world| {
        let mut spawned = 0;
        for x in 0..21 {
            for z in 0..20 {
                let center = [3.5 + 2.0 * f64::from(x), Y, 7.5 + 2.0 * f64::from(z)];
                let reason = SpawnReason::Natural;
                if tuff_feature_builtin_vanilla_mobs::world::region::spawning::spawn_fresh(
                    world,
                    EntityType::Husk,
                    center,
                    0.0,
                    0,
                    reason,
                ) {
                    spawned += 1;
                }
            }
        }
        spawned
    })
    .await;
    assert!(spawned > 300, "{spawned} husks spawned");
    world.ticks(2).await;

    let crews = camel_crews(&region).await;
    assert!(
        (5..spawned / 4).contains(&crews.len()),
        "{} camel husks for {spawned} husks",
        crews.len()
    );
    let spear = Some("minecraft:iron_spear".to_owned());
    for crew in &crews {
        let kinds: Vec<EntityType> = crew.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, [EntityType::Husk, EntityType::Parched], "{crew:?}");
        assert_eq!(crew[0].1, spear, "the husk wields a spear");
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camel_husk_never_breeds_and_stays_once_a_player_touches_it() {
    let (_dir, world, region, player, _events) = start(&[ChunkPos::new(0, 0)], 13).await;
    summon(
        &region,
        EntityType::CamelHusk,
        Compound::new(),
        [7.5, Y, 7.5],
    )
    .await;
    world.ticks(1).await;
    let camel = mobs(&region, EntityType::CamelHusk).await[0].id;
    let saved = entity_nbt(&region, camel).await.unwrap();
    assert_eq!(saved.get_byte("PersistenceRequired"), Some(0));
    assert_eq!(saved.get_int("Age"), None, "camel husks are never babies");

    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: camel,
            interaction: interaction(&player, Some(Item::RabbitFoot)),
        })
        .await;
    world.ticks(1).await;
    let saved = entity_nbt(&region, camel).await.unwrap();
    assert_eq!(saved.get_byte("PersistenceRequired"), Some(1));
    assert!(
        saved.get_int("InLove").is_none_or(|ticks| ticks == 0),
        "{saved:?}"
    );
}
