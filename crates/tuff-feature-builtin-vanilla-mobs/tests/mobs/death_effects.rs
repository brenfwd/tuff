//! Effects that act when their bearer is killed and removed.

use super::wither::lethal_attack;
use super::*;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;

fn bearing(effect: &str) -> Compound {
    let active = Compound::new()
        .with("id", effect)
        .with("duration", 10_000i32)
        .with("amplifier", 0i8);
    Compound::new()
        .with("NoAI", true)
        .with("active_effects", Tag::List(vec![Tag::Compound(active)]))
}

async fn kill_bearing(effect: &str) -> (TestWorld, RegionHandle, TestDir) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(&region, EntityType::Zombie, bearing(effect), [5.5, y, 5.5]).await;
    world.ticks(2).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].clone();
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: zombie.id,
            attack: lethal_attack(&player, zombie.position),
            sweep: None,
        })
        .await;
    world.ticks(30).await;
    (world, region, dir)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oozing_mob_leaves_two_slimes_when_killed() {
    let (world, region, _dir) = kill_bearing("minecraft:oozing").await;
    assert!(mobs(&region, EntityType::Zombie).await.is_empty());
    assert_eq!(mobs(&region, EntityType::Slime).await.len(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_weaving_mob_leaves_cobwebs_when_killed() {
    let (world, region, _dir) = kill_bearing("minecraft:weaving").await;
    let mut webs = 0;
    for x in 4..=7 {
        for y in SKY..=SKY + 2 {
            for z in 4..=7 {
                let state = block_at(&region, BlockPos::new(x, y, z)).await;
                webs += usize::from(state.map(|s| s.block()) == Some(Block::Cobweb));
            }
        }
    }
    assert!((1..=3).contains(&webs), "{webs}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_without_the_effects_leaves_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true),
        [5.5, y, 5.5],
    )
    .await;
    world.ticks(2).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].clone();
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: zombie.id,
            attack: lethal_attack(&player, zombie.position),
            sweep: None,
        })
        .await;
    world.ticks(30).await;
    assert!(mobs(&region, EntityType::Slime).await.is_empty());
    world.shutdown().await;
}

fn pen_region_setup() -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = roofed_pen(2, 13);
    blocks.push((BlockPos::new(6, SKY + 1, 6), Block::Stone.default_state()));
    blocks.push((BlockPos::new(6, SKY + 1, 5), Block::Lever.default_state()));
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wind_charged_mob_bursts_when_killed_and_flips_a_nearby_lever() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen_region_setup()).await;
    let lever = BlockPos::new(6, SKY + 1, 5);
    assert_eq!(
        block_at(&region, lever).await.unwrap().powered(),
        Some(false)
    );
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Zombie,
        bearing("minecraft:wind_charged"),
        [5.5, y, 5.5],
    )
    .await;
    world.ticks(2).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].clone();
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: zombie.id,
            attack: lethal_attack(&player, zombie.position),
            sweep: None,
        })
        .await;
    world.ticks(30).await;
    assert_eq!(
        block_at(&region, lever).await.unwrap().powered(),
        Some(true)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurt_infested_mob_eventually_releases_silverfish() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Giant,
        bearing("minecraft:infested"),
        [5.5, y, 5.5],
    )
    .await;
    world.ticks(2).await;
    let giant = mobs(&region, EntityType::Giant).await[0].clone();
    let mut released = false;
    for _ in 0..150 {
        let mut hit = lethal_attack(&player, giant.position);
        hit.damage = 0.5;
        region
            .request(RegionRequest::AttackEntity {
                attacker: player.clone(),
                target: giant.id,
                attack: hit,
                sweep: None,
            })
            .await;
        world.ticks(21).await;
        if !mobs(&region, EntityType::Silverfish).await.is_empty() {
            released = true;
            break;
        }
    }
    assert!(released);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oozing_sulfur_cube_leaves_slimes_when_its_explosion_removes_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    let mut nbt = bearing("minecraft:oozing");
    nbt.insert("Size", 1i32);
    nbt.insert("fuse", 5i32);
    nbt.insert(
        "equipment",
        Compound::new().with(
            "body",
            ItemStack::new(tuff_vanilla_data::Item::Tnt, 1).to_nbt(),
        ),
    );
    summon(&region, EntityType::SulfurCube, nbt, [5.5, y, 5.5]).await;
    world.ticks(40).await;
    assert!(mobs(&region, EntityType::SulfurCube).await.is_empty());
    assert_eq!(mobs(&region, EntityType::Slime).await.len(), 2);
    world.shutdown().await;
}

async fn react(
    effect: tuff_vanilla_data::registry::MobEffect,
    trigger: crate::world::region_api::PlayerEffectTrigger,
) -> (TestWorld, RegionHandle, TestDir) {
    use crate::world::region_api::{PlayerEffectEvent, RegionTask};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let event = PlayerEffectEvent {
        trigger,
        effects: vec![effect],
        culprit: crate::world::entity::Culprit::player(1000, 77, "Tester"),
        position: [5.5, SKY as f64 + 1.0, 5.5],
        height: 1.8,
        yaw: 0.0,
        pitch: 0.0,
    };
    region
        .request(RegionRequest::Run(RegionTask::new(move |world| {
            world.player_effect(event);
        })))
        .await;
    world.ticks(2).await;
    (world, region, dir)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oozing_player_leaves_two_slimes_when_killed() {
    use crate::world::region_api::PlayerEffectTrigger;
    let (world, region, _dir) = react(
        tuff_vanilla_data::registry::MobEffect::Oozing,
        PlayerEffectTrigger::Died,
    )
    .await;
    assert_eq!(mobs(&region, EntityType::Slime).await.len(), 2);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_weaving_player_leaves_cobwebs_when_killed() {
    use crate::world::region_api::PlayerEffectTrigger;
    let (world, region, _dir) = react(
        tuff_vanilla_data::registry::MobEffect::Weaving,
        PlayerEffectTrigger::Died,
    )
    .await;
    let mut webs = 0;
    for x in 4..=7 {
        for y in SKY..=SKY + 2 {
            for z in 4..=7 {
                let state = block_at(&region, BlockPos::new(x, y, z)).await;
                webs += usize::from(state.map(|s| s.block()) == Some(Block::Cobweb));
            }
        }
    }
    assert!((1..=3).contains(&webs), "{webs}");
    world.shutdown().await;
}
