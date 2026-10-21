//! The Warden: emerging when a shrieker summons it, digging back down when
//! left alone, anger from hits and smell, the sonic boom through walls,
//! and its Darkness (see `game_events` for the vibration chain).

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::PlayerLooks;
use crate::world::region_api::RegionTask;
use tuff_feature_builtin_vanilla_mobs::Mob;
use tuff_feature_builtin_vanilla_mobs::warden::Warden;
use tuff_vanilla_data::registry::{EntityType, MobEffect};

/// A pen tall enough for a Warden (2.9 blocks high), stone-roofed.
fn warden_pen(min: i32, max: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in min - 1..=max + 1 {
        for z in min - 1..=max + 1 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            blocks.push((BlockPos::new(x, SKY + 6, z), Block::Stone.default_state()));
            if x < min || x > max || z < min || z > max {
                for y in 1..=5 {
                    blocks.push((BlockPos::new(x, SKY + y, z), Block::Stone.default_state()));
                }
            }
        }
    }
    blocks
}

fn attack(player: &PlayerHandle, position: [f64; 3], damage: f32) -> EntityAttack {
    EntityAttack {
        attacker: player.id,
        attacker_uuid: 77,
        attacker_position: position,
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

fn hit(player: &PlayerHandle, target: i32, position: [f64; 3], damage: f32) -> RegionRequest {
    RegionRequest::AttackEntity {
        attacker: player.clone(),
        target,
        attack: attack(player, position, damage),
        sweep: None,
    }
}

async fn one_warden(world: &TestWorld, region: &RegionHandle) -> i32 {
    world.ticks(1).await;
    let found = mobs(region, EntityType::Warden).await;
    assert_eq!(found.len(), 1, "one warden: {found:#?}");
    found[0].id
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_warden_is_invulnerable_while_emerging_then_can_be_killed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, warden_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Warden,
        Compound::new(),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let id = one_warden(&world, &region).await;
    assert_eq!(
        with_warden(&region, id, |w| w.species.is_emerging()).await,
        Some(false),
        "a summoned warden just stands there"
    );
    with_warden(&region, id, |w| w.species.emerge(&mut w.core)).await;
    world.ticks(1).await;
    let position = [7.5, SKY as f64 + 1.0, 8.5];
    region.request(hit(&player, id, position, 1000.0)).await;
    world.ticks(100).await;
    assert_eq!(
        mobs(&region, EntityType::Warden).await.len(),
        1,
        "invulnerable while emerging"
    );
    world.ticks(40).await;
    assert_eq!(
        with_warden(&region, id, |w| w.species.is_emerging()).await,
        Some(false),
        "emerging takes 134 ticks"
    );
    region.request(hit(&player, id, position, 1000.0)).await;
    wait_for(&world, &region, 40, |found| {
        found.iter().all(|s| s.kind != EntityType::Warden)
    })
    .await;
    world.shutdown().await;
}

fn dig_cooldown(ttl: i64) -> Compound {
    let memory = Compound::new()
        .with("value", Compound::new())
        .with("ttl", ttl);
    Compound::new().with(
        "Brain",
        Compound::new().with(
            "memories",
            Compound::new().with("minecraft:dig_cooldown", memory),
        ),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_undisturbed_warden_digs_down_and_is_gone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, warden_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Warden,
        dig_cooldown(10),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Warden,
        dig_cooldown(10).with("PersistenceRequired", 1i8),
        [10.5, SKY as f64 + 1.0, 10.5],
    )
    .await;
    world.ticks(1).await;
    let found = mobs(&region, EntityType::Warden).await;
    let (digger, kept) = (found[0].id, found[1].id);
    world.ticks(40).await;
    assert_eq!(
        with_warden(&region, digger, |w| w.species.is_digging()).await,
        Some(true),
        "its dig cooldown ran out"
    );
    region
        .request(hit(&player, digger, [5.5, SKY as f64 + 1.0, 6.5], 1000.0))
        .await;
    world.ticks(100).await;
    let left = mobs(&region, EntityType::Warden).await;
    assert_eq!(left.len(), 1, "the digger is gone: {left:#?}");
    assert_eq!(left[0].id, kept, "a persistent warden never digs");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hit_makes_it_fight_and_a_walled_off_attacker_gets_a_sonic_boom() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = warden_pen(2, 13);
    for y in 1..=5 {
        for z in 2..=13 {
            blocks.push((BlockPos::new(10, SKY + y, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Warden,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let id = one_warden(&world, &region).await;
    let position = [12.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(2).await;
    region.request(hit(&player, id, position, 1.0)).await;
    world.ticks(1).await;
    let anger = with_warden(&region, id, move |w| w.species.anger_at(1000)).await;
    assert_eq!(anger, Some(100), "a hit is worth 100 anger");
    let hit = next_hit(&world, &mut events, 400).await;
    assert_eq!(
        hit.kind.name, "minecraft:sonic_boom",
        "the wall stops melee"
    );
    assert_eq!(hit.amount, 10.0);
    let push = hit.push.expect("the boom throws its victim");
    assert!(push[0] > 2.0, "pushed away from the warden: {push:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_warden_spreads_darkness_around_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, warden_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Warden,
        Compound::new().with("PersistenceRequired", 1i8),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [12.5, SKY as f64 + 1.0, 12.5],
            PlayerLooks::default(),
        ))
        .await;
    let effect = ticking_expect(&world, &mut events, 130, |event| match event {
        PlayerEvent::AddEffect(effect) if effect.effect == MobEffect::Darkness => Some(effect),
        _ => None,
    })
    .await;
    assert_eq!(effect.duration, 260);
    assert!(!effect.show_icon && !effect.show_particles);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_warden_sniffs_out_and_melees_a_nearby_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, warden_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Warden,
        Compound::new().with("PersistenceRequired", 1i8),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let position = [9.5, SKY as f64 + 1.0, 9.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 1500).await;
    assert!(
        hit.kind.name == "minecraft:mob_attack" || hit.kind.name == "minecraft:sonic_boom",
        "unexpected damage kind: {}",
        hit.kind.name
    );
    let attacker = hit.attacker.expect("the warden is to blame");
    assert_eq!(attacker.kind, EntityType::Warden);
    world.shutdown().await;
}
