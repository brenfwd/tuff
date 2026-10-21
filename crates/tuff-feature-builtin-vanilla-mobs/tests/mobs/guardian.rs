//! Guardians and elder guardians: swimming, the charged beam attack, body
//! spikes stinging back a melee hit, and the elder guardian's Mining
//! Fatigue aura.

use super::*;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::registry::{EntityType, MobEffect};

/// A sealed box of water `height` blocks tall over `min..=max`, walled,
/// floored and roofed with stone so a guardian has room to swim without
/// ever leaving the water.
pub(super) fn water_pool(
    min: i32,
    max: i32,
    height: i32,
) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            let inside = (min..=max).contains(&x) && (min..=max).contains(&z);
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            for y in SKY + 1..=SKY + height {
                let state = if inside {
                    Block::Water.default_state()
                } else {
                    Block::Stone.default_state()
                };
                blocks.push((BlockPos::new(x, y, z), state));
            }
            blocks.push((
                BlockPos::new(x, SKY + height + 1, z),
                Block::Stone.default_state(),
            ));
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guardians_swim_around_their_pool() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    let start = [6.5, SKY as f64 + 3.0, 6.5];
    summon(&region, EntityType::Guardian, Compound::new(), start).await;
    wait_for(&world, &region, 300, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Guardian && crate::world::distance_sq(s.position, start) > 0.25
        })
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guardian_stings_back_a_melee_hit_while_its_spikes_are_up() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    // A dry pen: a guardian here never finds a swim target, so `moving`
    // (and so its spikes) stays deterministically at rest.
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Guardian,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let guardian = mobs(&region, EntityType::Guardian).await[0].id;
    region
        .request(player_at(
            &viewer,
            [8.5, SKY as f64 + 1.0, 3.5],
            PlayerLooks::default(),
        ))
        .await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: viewer.clone(),
            target: guardian,
            attack: attack(1.0, None),
            sweep: None,
        })
        .await;
    let sting = next_hit(&world, &mut events, 5).await;
    assert_eq!(sting.kind.name, "minecraft:thorns");
    assert_eq!(sting.amount, 2.0);
    assert_eq!(
        sting.attacker.map(|a| a.kind),
        Some(EntityType::Guardian),
        "the guardian's own spikes hit back"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guardian_charges_a_beam_and_fires_it_after_the_full_windup() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    summon(
        &region,
        EntityType::Guardian,
        Compound::new(),
        [6.5, SKY as f64 + 3.0, 6.5],
    )
    .await;
    let position = [10.5, SKY as f64 + 3.0, 6.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let mut hit = None;
    for tick in 1..=600u64 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::EntityHurt(entity_hit) = event {
                hit = Some((tick, entity_hit));
                break;
            }
        }
        if hit.is_some() {
            break;
        }
    }
    let (tick, hit) = hit.expect("the beam eventually fires");
    assert_eq!(
        hit.kind.name, "minecraft:indirect_magic",
        "the fixed beam hit fires first"
    );
    assert!(
        (1.0..=3.0).contains(&hit.amount),
        "1 damage, or 3 on hard difficulty: {}",
        hit.amount
    );
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Guardian));
    assert!(
        tick >= 85,
        "the beam has a real charge-up, not an instant hit: fired at tick {tick}"
    );
    // The fixed magic hit is immediately followed by a normal
    // melee-equivalent hit, using the guardian's own attack damage.
    let melee = next_hit(&world, &mut events, 2).await;
    assert_eq!(melee.kind.name, "minecraft:mob_attack");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_elder_guardian_curses_nearby_players_with_mining_fatigue() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    summon(
        &region,
        EntityType::ElderGuardian,
        Compound::new(),
        [6.5, SKY as f64 + 3.0, 6.5],
    )
    .await;
    let position = [10.5, SKY as f64 + 3.0, 6.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = ticking_expect(&world, &mut events, 1250, |event| match event {
        PlayerEvent::Potion(hit) => Some(hit),
        _ => None,
    })
    .await;
    let effect = hit
        .effects
        .iter()
        .find(|e| e.effect == MobEffect::MiningFatigue)
        .expect("the elder guardian's curse");
    assert_eq!(effect.amplifier, 2);
    assert_eq!(effect.duration, 6000);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guardian_leaves_a_player_within_three_blocks_alone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(5, 7, 2)).await;
    summon(
        &region,
        EntityType::Guardian,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [6.5, SKY as f64 + 1.0, 6.5],
            PlayerLooks::default(),
        ))
        .await;
    for _ in 0..300 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, PlayerEvent::EntityHurt(_)),
                "a player this close is never beamed"
            );
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guardians_hunt_squid() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    summon(
        &region,
        EntityType::Guardian,
        Compound::new(),
        [3.5, SKY as f64 + 3.0, 6.5],
    )
    .await;
    summon(
        &region,
        EntityType::Squid,
        Compound::new().with("NoAI", true),
        [10.5, SKY as f64 + 3.0, 6.5],
    )
    .await;
    let squid = mobs(&region, EntityType::Squid).await[0].id;
    let full = mob_health(&region, squid).await;
    let mut hurt = false;
    for _ in 0..400 {
        world.ticks(1).await;
        if mob_health(&region, squid).await < full {
            hurt = true;
            break;
        }
    }
    assert!(hurt, "the guardian's beam finds the squid");
    world.shutdown().await;
}
