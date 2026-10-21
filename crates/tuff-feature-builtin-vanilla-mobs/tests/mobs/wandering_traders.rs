//! Wandering traders: the world sending one to a player with its llamas,
//! trading with it, and its llamas defending it.

use super::*;
use crate::world::messages::PlayerLooks;
use crate::world::region_api::RegionTask;
use tuff_feature_builtin_vanilla_mobs::merchant_events::Opened;
use tuff_vanilla_data::registry::EntityType;

fn set_time(world: &TestWorld, ticks: i64) {
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wandering_trader_arrives_near_a_player_with_its_llamas() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(12, 12).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    let mut near = Vec::new();
    for x in 8..=16 {
        for z in 8..=16 {
            near.push(ChunkPos::new(x, z));
        }
    }
    subscribe(&region, &viewer, &mut events, &near).await;
    set_time(&world, 3000);
    place_blocks(&world, &region, pen(140, 260)).await;
    world.ticks(20).await;
    let player = BlockPos::new(200, SKY + 1, 200);
    let (reply, spawned) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: player,
            max: player,
            run: RegionTask::new(move |world| {
                let spawned = (0..20).any(|_| {
                    tuff_feature_builtin_vanilla_mobs::world::region::traders::spawn_trader_near(
                        world, player,
                    )
                });
                let _ = reply.send(spawned);
            }),
        })
        .await;
    assert!(spawned.await.unwrap(), "a trader came");
    let traders = mobs(&region, EntityType::WanderingTrader).await;
    assert_eq!(traders.len(), 1);
    let trader = &traders[0];
    let dx = trader.position[0] - 200.5;
    let dz = trader.position[2] - 200.5;
    assert!(
        dx.abs() <= 48.5 && dz.abs() <= 48.5,
        "{:?}",
        trader.position
    );
    let nbt = entity_nbt(&region, trader.id).await.unwrap();
    assert!(
        matches!(nbt.get("DespawnDelay"), Some(Tag::Int(d)) if *d > 47_000),
        "{nbt:?}"
    );
    assert_eq!(
        nbt.get("wander_target"),
        Some(&Tag::IntArray(vec![200, SKY + 1, 200])),
        "it heads for the player"
    );
    let llamas = mobs(&region, EntityType::TraderLlama).await;
    assert!(
        (1..=2).contains(&llamas.len()),
        "two tries at a llama: {llamas:?}"
    );
    for llama in llamas {
        let nbt = entity_nbt(&region, llama.id).await.unwrap();
        let bound = nbt
            .get("leash")
            .and_then(|t| t.as_compound())
            .and_then(|c| c.get_uuid("UUID"));
        assert_eq!(bound, Some(trader.uuid), "bound to its trader");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_trade_with_a_wandering_trader() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 10)).await;
    summon(
        &region,
        EntityType::WanderingTrader,
        Compound::new().with("NoAI", true),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [5.5, SKY as f64 + 1.0, 3.5],
            PlayerLooks::default(),
        ))
        .await;
    let trader = mobs(&region, EntityType::WanderingTrader).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: player.clone(),
            target: trader,
            interaction: interaction(&player, None),
        })
        .await;
    let view = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<Opened>().map(|opened| opened.view.clone()),
        _ => None,
    })
    .await;
    assert_eq!(view.merchant, trader);
    assert!(!view.offers.is_empty());
    assert!(!view.regular, "no level or progress bar");
    assert!(!view.can_restock);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trader_llamas_spit_at_whoever_hurts_their_trader() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 12)).await;
    summon(
        &region,
        EntityType::WanderingTrader,
        Compound::new()
            .with("NoAI", true)
            .with("DespawnDelay", 48_000),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let trader = mobs(&region, EntityType::WanderingTrader).await[0].clone();
    summon(
        &region,
        EntityType::TraderLlama,
        Compound::new().with(
            "leash",
            Compound::new().with("UUID", Tag::uuid(trader.uuid)),
        ),
        [3.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [8.5, SKY as f64 + 1.0, 3.5],
            PlayerLooks::default(),
        ))
        .await;
    world.ticks(2).await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: trader.id,
            attack: attack(1.0, None),
            sweep: None,
        })
        .await;
    let found = wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::LlamaSpit)
    })
    .await;
    assert!(found.iter().any(|s| s.kind == EntityType::LlamaSpit));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trader_llamas_on_their_traders_lead_leave_with_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 12)).await;
    summon(
        &region,
        EntityType::WanderingTrader,
        Compound::new().with("NoAI", true).with("DespawnDelay", 30),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let trader = mobs(&region, EntityType::WanderingTrader).await[0].clone();
    summon(
        &region,
        EntityType::TraderLlama,
        Compound::new().with(
            "leash",
            Compound::new().with("UUID", Tag::uuid(trader.uuid)),
        ),
        [3.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    world.ticks(10).await;
    assert_eq!(mobs(&region, EntityType::TraderLlama).await.len(), 1);
    world.ticks(30).await;
    assert!(mobs(&region, EntityType::WanderingTrader).await.is_empty());
    assert!(mobs(&region, EntityType::TraderLlama).await.is_empty());
    assert!(
        mobs(&region, EntityType::Item).await.is_empty(),
        "the lead goes with them"
    );
    world.shutdown().await;
}

fn saved_timer(backend: &Arc<dyn tuff_storage::backend::WorldBackend>) -> (i32, i32) {
    let root = backend
        .documents()
        .read(tuff_storage::document::Document::DimensionData {
            dimension: "minecraft:overworld",
            id: "minecraft:wandering_trader",
        })
        .expect("the timer was saved");
    let data = tuff_storage::document::inner(root);
    (
        data.get_int("spawn_delay").unwrap(),
        data.get_int("spawn_chance").unwrap(),
    )
}

async fn timer_survives_restart(backend: Arc<dyn tuff_storage::backend::WorldBackend>) {
    let world = TestWorld::start_with(backend.clone());
    let _region = world.region(ChunkPos::new(0, 0).region()).await;
    world.ticks(1210).await;
    world.shared.services.flush().await;
    world.shutdown().await;
    assert_eq!(saved_timer(&backend), (22_800, 25));

    let world = TestWorld::start_with(backend.clone());
    let _region = world.region(ChunkPos::new(0, 0).region()).await;
    world.ticks(1210).await;
    world.shared.services.flush().await;
    world.shutdown().await;
    assert_eq!(saved_timer(&backend), (21_600, 25), "it kept counting down");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_trader_timer_survives_a_restart_in_the_vanilla_layout() {
    let dir = TestDir::new();
    let backend = tuff_core::storage::Storage::default()
        .open(dir.path())
        .unwrap();
    timer_survives_restart(backend).await;
    assert!(
        dir.path()
            .join("dimensions/minecraft/overworld/data/minecraft/wandering_trader.dat")
            .exists()
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_trader_timer_survives_a_restart_in_sqlite() {
    let dir = TestDir::new();
    let backend: Arc<dyn tuff_storage::backend::WorldBackend> =
        Arc::new(tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.tuff")).unwrap());
    timer_survives_restart(backend).await;
}
