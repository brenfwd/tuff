//! Trial spawners and vaults: waves, rewards, ominous mode, keys.

use super::*;
use crate::world::messages::{HandEffect, PlayerLooks};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{EntityType, MobEffect};

const SPAWNER_AT: BlockPos = BlockPos::new(8, SKY + 1, 8);
const VAULT_AT: BlockPos = BlockPos::new(8, SKY + 1, 8);

fn player_at(player: &PlayerHandle, position: [f64; 3], looks: PlayerLooks) -> RegionRequest {
    match tuff_test_support::player_at(player, position, looks) {
        RegionRequest::Player(mut update) => {
            update.uuid = u128::from(player.id as u32);
            RegionRequest::Player(update)
        }
        other => other,
    }
}

fn standing() -> PlayerLooks {
    PlayerLooks {
        invulnerable: true,
        ..Default::default()
    }
}

fn with_block_entity(mut op: Place, nbt: Compound) -> Place {
    op.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with("minecraft:block_entity_data", nbt),
        ..Default::default()
    })
    .into();
    op.game_master = true;
    op
}

fn trial_spawner_data(normal: &str, ominous: &str) -> Compound {
    Compound::new()
        .with("id", "minecraft:trial_spawner")
        .with("normal_config", normal)
        .with("ominous_config", ominous)
        .with("target_cooldown_length", 200)
}

async fn state_of(region: &RegionHandle, pos: BlockPos, property: &str) -> String {
    block_at(region, pos)
        .await
        .and_then(|state| state.get(property))
        .unwrap_or("")
        .to_owned()
}

async fn wait_state(
    world: &TestWorld,
    region: &RegionHandle,
    pos: BlockPos,
    property: &str,
    wanted: &str,
    ticks: u64,
) {
    for _ in 0..ticks {
        if state_of(region, pos, property).await == wanted {
            return;
        }
        world.ticks(1).await;
    }
    assert_eq!(
        state_of(region, pos, property).await,
        wanted,
        "{property} never became {wanted}"
    );
}

async fn kill(region: &RegionHandle, ids: Vec<i32>) {
    region
        .ask(move |world| ids.into_iter().filter(|id| world.kill_entity(*id)).count())
        .await
        .unwrap();
}

async fn items(region: &RegionHandle) -> usize {
    snapshots(region)
        .await
        .iter()
        .filter(|s| s.kind == EntityType::Item)
        .count()
}

async fn spawner_world(
    dir: &TestDir,
    nbt: Compound,
) -> (
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let world = TestWorld::start(dir.path());
    allow_spawning(&world);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let op = place(&player, BlockPos::new(8, SKY, 8), Item::TrialSpawner, 1);
    region
        .request(RegionRequest::Place(with_block_entity(op, nbt)))
        .await;
    (world, region, player, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trial_spawners_send_waves_then_eject_rewards_and_cool_down() {
    let dir = TestDir::new();
    let nbt = trial_spawner_data(
        "minecraft:trial_chamber/melee/zombie/normal",
        "minecraft:trial_chamber/melee/zombie/ominous",
    );
    let (world, region, player, _events) = spawner_world(&dir, nbt).await;
    wait_state(
        &world,
        &region,
        SPAWNER_AT,
        "trial_spawner_state",
        "waiting_for_players",
        40,
    )
    .await;
    world.ticks(60).await;
    assert!(
        mobs(&region, EntityType::Zombie).await.is_empty(),
        "nothing spawns before a player is close"
    );
    region
        .request(player_at(&player, [4.5, SKY as f64 + 1.0, 4.5], standing()))
        .await;
    wait_state(
        &world,
        &region,
        SPAWNER_AT,
        "trial_spawner_state",
        "active",
        80,
    )
    .await;
    let mut killed = std::collections::HashSet::new();
    for _ in 0..1500 {
        if state_of(&region, SPAWNER_AT, "trial_spawner_state").await != "active" {
            break;
        }
        let live = mobs(&region, EntityType::Zombie).await;
        assert!(live.len() <= 2, "one player allows two at once");
        for zombie in &live {
            killed.insert(zombie.uuid);
        }
        kill(&region, live.iter().map(|z| z.id).collect()).await;
        world.ticks(2).await;
    }
    assert_eq!(killed.len(), 6, "one player is sent six zombies in all");
    wait_state(
        &world,
        &region,
        SPAWNER_AT,
        "trial_spawner_state",
        "ejecting_reward",
        120,
    )
    .await;
    wait_state(
        &world,
        &region,
        SPAWNER_AT,
        "trial_spawner_state",
        "cooldown",
        120,
    )
    .await;
    assert!(items(&region).await >= 1, "a reward was ejected");
    wait_state(
        &world,
        &region,
        SPAWNER_AT,
        "trial_spawner_state",
        "waiting_for_players",
        400,
    )
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trial_spawners_do_not_wake_for_creative_players() {
    let dir = TestDir::new();
    let nbt = trial_spawner_data(
        "minecraft:trial_chamber/melee/zombie/normal",
        "minecraft:trial_chamber/melee/zombie/ominous",
    );
    let (world, region, player, _events) = spawner_world(&dir, nbt).await;
    let creative = PlayerLooks {
        creative: true,
        invulnerable: true,
        ..Default::default()
    };
    region
        .request(player_at(&player, [4.5, SKY as f64 + 1.0, 4.5], creative))
        .await;
    world.ticks(200).await;
    assert_eq!(
        state_of(&region, SPAWNER_AT, "trial_spawner_state").await,
        "waiting_for_players"
    );
    assert!(mobs(&region, EntityType::Zombie).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_omen_makes_a_trial_spawner_ominous_and_it_rains_items() {
    let dir = TestDir::new();
    let nbt = trial_spawner_data(
        "minecraft:trial_chamber/melee/zombie/normal",
        "minecraft:trial_chamber/melee/zombie/ominous",
    );
    let (world, region, player, mut events) = spawner_world(&dir, nbt).await;
    let omen = PlayerLooks {
        invulnerable: true,
        effects: PlayerLooks::effect_bits([MobEffect::BadOmen]),
        ..Default::default()
    };
    region
        .request(player_at(&player, [4.5, SKY as f64 + 1.0, 4.5], omen))
        .await;
    let converted = ticking_expect(&world, &mut events, 200, |event| match event {
        PlayerEvent::ConvertEffect {
            from,
            to,
            ticks_per_level,
        } => Some((from, to, ticks_per_level)),
        _ => None,
    })
    .await;
    assert_eq!(
        converted,
        (MobEffect::BadOmen, MobEffect::TrialOmen, 18_000),
        "the player's Bad Omen turns into Trial Omen"
    );
    wait_state(&world, &region, SPAWNER_AT, "ominous", "true", 20).await;
    wait_state(
        &world,
        &region,
        SPAWNER_AT,
        "trial_spawner_state",
        "active",
        120,
    )
    .await;
    wait_for(&world, &region, 600, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::OminousItemSpawner)
    })
    .await;
    world.shutdown().await;
}

fn vault_data() -> Compound {
    Compound::new().with("id", "minecraft:vault").with(
        "config",
        Compound::new()
            .with("loot_table", "minecraft:chests/trial_chambers/reward")
            .with(
                "key_item",
                Compound::new()
                    .with("id", "minecraft:trial_key")
                    .with("count", 1),
            ),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vaults_wake_for_players_take_one_key_each_and_eject_loot() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let op = place(&player, BlockPos::new(8, SKY, 8), Item::Vault, 1);
    region
        .request(RegionRequest::Place(with_block_entity(op, vault_data())))
        .await;
    world.ticks(40).await;
    assert_eq!(
        state_of(&region, VAULT_AT, "vault_state").await,
        "inactive",
        "nobody is near"
    );
    region
        .request(player_at(&player, [8.5, SKY as f64 + 1.0, 6.5], standing()))
        .await;
    wait_state(&world, &region, VAULT_AT, "vault_state", "active", 60).await;

    let wrong = place(&player, VAULT_AT, Item::Stick, 2);
    region.request(RegionRequest::Place(wrong)).await;
    let outcome = ticking_expect(&world, &mut events, 20, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Interacted { .. }));
    assert_eq!(state_of(&region, VAULT_AT, "vault_state").await, "active");

    let key = place(&player, VAULT_AT, Item::TrialKey, 3);
    region.request(RegionRequest::Place(key)).await;
    let outcome = ticking_expect(&world, &mut events, 20, |event| match event {
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
    assert_eq!(
        state_of(&region, VAULT_AT, "vault_state").await,
        "unlocking"
    );
    wait_state(&world, &region, VAULT_AT, "vault_state", "ejecting", 40).await;
    for _ in 0..400 {
        if state_of(&region, VAULT_AT, "vault_state").await != "ejecting" {
            break;
        }
        world.ticks(1).await;
    }
    assert!(items(&region).await >= 1, "the vault ejected its loot");
    world.ticks(60).await;
    assert_eq!(
        state_of(&region, VAULT_AT, "vault_state").await,
        "inactive",
        "a rewarded player no longer keeps the vault awake"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_needs_as_many_keys_as_its_key_item_counts() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let mut data = vault_data();
    if let Some(Tag::Compound(config)) = data.get_mut("config") {
        config.insert(
            "key_item",
            Compound::new()
                .with("id", "minecraft:trial_key")
                .with("count", 2),
        );
    }
    let op = place(&player, BlockPos::new(8, SKY, 8), Item::Vault, 1);
    region
        .request(RegionRequest::Place(with_block_entity(op, data)))
        .await;
    region
        .request(player_at(&player, [8.5, SKY as f64 + 1.0, 6.5], standing()))
        .await;
    wait_state(&world, &region, VAULT_AT, "vault_state", "active", 60).await;

    let mut short = place(&player, VAULT_AT, Item::TrialKey, 2);
    short.held_count = 1;
    region.request(RegionRequest::Place(short)).await;
    let outcome = ticking_expect(&world, &mut events, 20, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Interacted { .. }));
    assert_eq!(state_of(&region, VAULT_AT, "vault_state").await, "active");

    let mut enough = place(&player, VAULT_AT, Item::TrialKey, 3);
    enough.held_count = 2;
    region.request(RegionRequest::Place(enough)).await;
    let outcome = ticking_expect(&world, &mut events, 20, |event| match event {
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
            effect: HandEffect::Consume(2),
            ..
        }
    ));
    world.shutdown().await;
}
