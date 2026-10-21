//! Zombie extras: drowning conversion, door breaking and reinforcements.

use super::drowned::water_pool;
use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::PlayerLooks;
use crate::world::player::Difficulty;
use tuff_vanilla_data::registry::EntityType;

fn nbt_int(nbt: &Compound, key: &str) -> i64 {
    crate::world::entity::number(nbt.get(key).unwrap()).unwrap() as i64
}

pub(super) fn attack(player: &PlayerHandle, target: i32, position: [f64; 3]) -> RegionRequest {
    RegionRequest::AttackEntity {
        attacker: player.clone(),
        target,
        attack: EntityAttack {
            attacker: player.id,
            attacker_uuid: 77,
            attacker_position: position,
            attacker_yaw: 0.0,
            attacker_pitch: 0.0,
            damage: 1.0,
            knockback: 0.0,
            weapon: None,
            critical: false,
            magic: false,
            sweep: false,
            part: None,
        },
        sweep: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_submerged_long_enough_shake_then_become_drowned() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(1, 12, 6)).await;
    let start = [6.5, SKY as f64 + 3.0, 6.5];
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("InWaterTime", 599),
        start,
    )
    .await;
    world.ticks(3).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let nbt = entity_nbt(&region, zombie).await.unwrap();
    assert!(nbt_int(&nbt, "DrownedConversionTime") > 250, "shaking");
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("DrownedConversionTime", 3),
        [8.5, SKY as f64 + 3.0, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::Husk,
        Compound::new().with("DrownedConversionTime", 3),
        [4.5, SKY as f64 + 3.0, 4.5],
    )
    .await;
    wait_for(&world, &region, 60, |found| {
        found.iter().any(|s| s.kind == EntityType::Drowned)
            && found.iter().any(|s| s.kind == EntityType::Zombie)
            && !found.iter().any(|s| s.kind == EntityType::Husk)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_out_of_the_water_stop_converting() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("DrownedConversionTime", 100),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    world.ticks(3).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let nbt = entity_nbt(&region, zombie).await.unwrap();
    assert_eq!(nbt_int(&nbt, "DrownedConversionTime"), -1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_break_doors_down_on_hard_difficulty() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(Difficulty::Hard);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let door = BlockPos::new(8, SKY + 1, 7);
    let oak = Block::OakDoor
        .default_state()
        .with("facing", "east")
        .unwrap();
    let mut blocks = roofed_pen(2, 13);
    for z in 2..=13 {
        for y in 1..=3 {
            blocks.push((BlockPos::new(8, SKY + y, z), Block::Stone.default_state()));
        }
    }
    blocks.push((door, oak.with("half", "lower").unwrap()));
    blocks.push((
        BlockPos::new(8, SKY + 2, 7),
        oak.with("half", "upper").unwrap(),
    ));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("CanBreakDoors", true),
        [5.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let position = [11.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(2).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    region.request(attack(&player, zombie, position)).await;
    for _ in 0..900 {
        region
            .request(player_at(&player, position, PlayerLooks::default()))
            .await;
        let lower = block_at(&region, door).await;
        let upper = block_at(&region, BlockPos::new(8, SKY + 2, 7)).await;
        if lower.is_some_and(|s| s.block() != Block::OakDoor)
            || upper.is_some_and(|s| s.block() != Block::OakDoor)
        {
            world.shutdown().await;
            return;
        }
        world.ticks(1).await;
    }
    panic!("the door never broke");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombies_leave_doors_alone_below_hard() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let door = BlockPos::new(8, SKY + 1, 7);
    let oak = Block::OakDoor
        .default_state()
        .with("facing", "east")
        .unwrap();
    let mut blocks = roofed_pen(2, 13);
    for z in 2..=13 {
        for y in 1..=3 {
            blocks.push((BlockPos::new(8, SKY + y, z), Block::Stone.default_state()));
        }
    }
    blocks.push((door, oak.with("half", "lower").unwrap()));
    blocks.push((
        BlockPos::new(8, SKY + 2, 7),
        oak.with("half", "upper").unwrap(),
    ));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("CanBreakDoors", true),
        [5.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let position = [11.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(2).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    region.request(attack(&player, zombie, position)).await;
    for _ in 0..400 {
        region
            .request(player_at(&player, position, PlayerLooks::default()))
            .await;
        world.ticks(1).await;
    }
    let lower = block_at(&region, door).await.unwrap();
    assert_eq!(lower.block(), Block::OakDoor);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hurt_zombies_on_hard_call_reinforcements() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.difficulty.send_replace(Difficulty::Hard);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    let chunks: Vec<ChunkPos> = (0..=7)
        .flat_map(|x| (0..=7).map(move |z| ChunkPos::new(x, z)))
        .collect();
    subscribe(&region, &player, &mut events, &chunks).await;
    let mut blocks = Vec::new();
    for x in 0..=100 {
        for z in 0..=100 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
            blocks.push((BlockPos::new(x, SKY + 4, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let leader = Compound::new().with(
        "attributes",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("id", "minecraft:spawn_reinforcements")
                .with("base", 1.0f64),
        )]),
    );
    summon(
        &region,
        EntityType::Zombie,
        leader,
        [50.5, SKY as f64 + 1.0, 50.5],
    )
    .await;
    world.ticks(30).await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let position = [50.5, SKY as f64 + 1.0, 52.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: zombie,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 1.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            },
            sweep: None,
        })
        .await;
    wait_for(&world, &region, 20, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Zombie)
            .count()
            >= 2
    })
    .await;
    world.shutdown().await;
}
