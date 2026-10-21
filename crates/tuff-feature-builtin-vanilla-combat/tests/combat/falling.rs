//! Falling blocks (sand, gravel, anvils, ...): support removal turning a
//! block into a falling entity, a stack cascading down in sequence, that
//! entity surviving a save/load round trip, that only anvils hurt a mob
//! they land on, and that a long enough fall breaks an anvil apart.
//! Scaffolding and stalactites fall through their own support rules.

use super::*;
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;

async fn falling_blocks(region: &RegionHandle) -> Vec<crate::world::messages::EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntityType::FallingBlock)
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_block_falls_when_its_support_is_removed_and_lands() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, support, gravel) = (
        BlockPos::new(8, SKY, 8),
        BlockPos::new(8, SKY + 1, 8),
        BlockPos::new(8, SKY + 2, 8),
    );
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
            (gravel, Block::Gravel.default_state()),
        ],
    )
    .await;
    assert_eq!(
        block_at(&region, gravel).await.map(|s| s.block()),
        Some(Block::Gravel)
    );

    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;

    let mut landed = false;
    for _ in 0..40 {
        world.ticks(1).await;
        if block_at(&region, support).await.map(|s| s.block()) == Some(Block::Gravel) {
            landed = true;
            break;
        }
    }
    assert!(landed, "the gravel fell into its old support's place");
    assert_eq!(
        block_at(&region, gravel).await.map(|s| s.block()),
        Some(Block::Air),
        "its old spot is empty"
    );
    assert!(falling_blocks(&region).await.is_empty(), "it settled");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_concrete_powder_solidifies_when_it_lands_in_water() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, water, support, powder) = (
        BlockPos::new(8, SKY, 8),
        BlockPos::new(8, SKY + 1, 8),
        BlockPos::new(8, SKY + 2, 8),
        BlockPos::new(8, SKY + 3, 8),
    );
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (water, Block::Water.default_state()),
            (support, Block::Stone.default_state()),
            (powder, Block::WhiteConcretePowder.default_state()),
        ],
    )
    .await;

    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;

    let mut landed = false;
    for _ in 0..40 {
        world.ticks(1).await;
        if block_at(&region, water).await.map(|s| s.block()) == Some(Block::WhiteConcrete) {
            landed = true;
            break;
        }
    }
    assert!(landed, "the powder fell into the water and solidified");
    assert!(falling_blocks(&region).await.is_empty(), "it settled");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stack_of_falling_blocks_falls_one_after_another() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, support) = (BlockPos::new(8, SKY, 8), BlockPos::new(8, SKY + 1, 8));
    let stack: Vec<BlockPos> = (2..=4).map(|dy| BlockPos::new(8, SKY + dy, 8)).collect();
    let mut initial = vec![
        (floor, Block::Stone.default_state()),
        (support, Block::Stone.default_state()),
    ];
    initial.extend(
        stack
            .iter()
            .map(|pos| (*pos, Block::Gravel.default_state())),
    );
    place_blocks(&world, &region, initial).await;

    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;

    let mut settled = false;
    for _ in 0..120 {
        world.ticks(1).await;
        if falling_blocks(&region).await.is_empty() {
            settled = true;
            break;
        }
    }
    assert!(settled, "every gravel block landed");
    // The stack packs down onto the floor with no gaps: support and the
    // three former stack positions are now gravel, the top spot is air.
    for pos in [support, stack[0], stack[1]] {
        assert_eq!(
            block_at(&region, pos).await.map(|s| s.block()),
            Some(Block::Gravel),
            "{pos:?} is packed with gravel"
        );
    }
    assert_eq!(
        block_at(&region, stack[2]).await.map(|s| s.block()),
        Some(Block::Air),
        "the top of the stack is now empty"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_block_does_not_hurt_a_mob_it_lands_on() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, support, gravel) = (
        BlockPos::new(8, SKY, 8),
        BlockPos::new(8, SKY + 1, 8),
        BlockPos::new(8, SKY + 3, 8),
    );
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
            (gravel, Block::Gravel.default_state()),
        ],
    )
    .await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true),
        [8.5, (SKY + 2) as f64, 8.5],
    )
    .await;
    let zombie_id = mobs(&region, EntityType::Zombie)
        .await
        .first()
        .expect("the zombie spawned")
        .id;

    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;
    for _ in 0..40 {
        world.ticks(1).await;
        if falling_blocks(&region).await.is_empty() {
            break;
        }
    }

    let zombies = mobs(&region, EntityType::Zombie).await;
    assert_eq!(zombies.len(), 1, "the zombie is unharmed and still there");
    assert_eq!(zombies[0].id, zombie_id);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mid_air_falling_block_survives_a_chunk_unload_and_reload() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(2, 2);
    let position = [40.5, SKY as f64, 40.5];
    {
        let world = TestWorld::start(dir.path());
        let region = world.region(chunk.region()).await;
        let (player, mut events) = PlayerHandle::new(100);
        subscribe(&region, &player, &mut events, &[chunk]).await;
        let nbt = Compound::new()
            .with("BlockState", "minecraft:gravel")
            .with("NoGravity", true)
            .with("Time", 5i32);
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt: nbt.with("id", EntityType::FallingBlock.name()),
                position,
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap(), "the falling block loads from NBT");
        assert_eq!(falling_blocks(&region).await.len(), 1);

        region
            .request(RegionRequest::Unsubscribe {
                player: player.id,
                chunks: vec![chunk],
            })
            .await;
        world.ticks(150).await;
        assert!(
            falling_blocks(&region).await.is_empty(),
            "unloaded with its chunk"
        );
        world.shutdown().await;
    }

    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(200);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let found = falling_blocks(&region).await;
    assert_eq!(found.len(), 1, "it came back with its chunk");
    assert_eq!(found[0].position, position, "no gravity kept it in place");
    let _ = &mut events;
    world.shutdown().await;
}

async fn world_items(region: &RegionHandle, item: &str) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntityType::Item && e.name.to_lowercase().contains(item))
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsupported_scaffolding_falls_and_settles_with_its_new_distance() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, landing, start) = (
        BlockPos::new(8, SKY, 8),
        BlockPos::new(8, SKY + 1, 8),
        BlockPos::new(8, SKY + 5, 8),
    );
    place_blocks(&world, &region, [(floor, Block::Stone.default_state())]).await;
    place_blocks(
        &world,
        &region,
        [(start, Block::Scaffolding.default_state())],
    )
    .await;

    let mut settled = None;
    for _ in 0..60 {
        world.ticks(1).await;
        let state = block_at(&region, landing).await;
        if state.and_then(|s| s.distance()) == Some(0) {
            settled = state;
            break;
        }
    }
    let settled = settled.expect("the scaffolding fell and found its support");
    assert_eq!(settled.block(), Block::Scaffolding);
    assert_eq!(settled.bottom(), Some(false));
    assert_eq!(
        block_at(&region, start).await,
        Some(Block::Air.default_state())
    );
    assert!(falling_blocks(&region).await.is_empty(), "it settled");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scaffolding_that_just_lost_its_support_breaks_instead_of_falling() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (ground, support, scaffold) = (
        BlockPos::new(8, SKY, 8),
        BlockPos::new(8, SKY + 3, 8),
        BlockPos::new(8, SKY + 4, 8),
    );
    place_blocks(
        &world,
        &region,
        [
            (ground, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
        ],
    )
    .await;
    place_blocks(
        &world,
        &region,
        [(scaffold, Block::Scaffolding.default_state())],
    )
    .await;
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, scaffold).await.and_then(|s| s.distance()),
        Some(0)
    );

    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;
    world.ticks(3).await;
    assert_eq!(
        block_at(&region, scaffold).await,
        Some(Block::Air.default_state())
    );
    assert!(
        falling_blocks(&region).await.is_empty(),
        "it broke, not fell"
    );
    assert_eq!(
        world_items(&region, "scaffolding").await,
        1,
        "it dropped itself"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalactite_losing_its_ceiling_falls_breaks_and_hurts_what_it_hits() {
    use tuff_vanilla_data::block::{Thickness, VerticalDirection};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let drip = |thickness| {
        Block::PointedDripstone
            .default_state()
            .with_vertical_direction(VerticalDirection::Down)
            .unwrap()
            .with_thickness(thickness)
            .unwrap()
    };
    let floor = BlockPos::new(8, SKY, 8);
    let ceiling = BlockPos::new(8, SKY + 10, 8);
    let column = [BlockPos::new(8, SKY + 9, 8), BlockPos::new(8, SKY + 8, 8)];
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (ceiling, Block::Stone.default_state()),
            (column[0], drip(Thickness::Frustum)),
            (column[1], drip(Thickness::Tip)),
        ],
    )
    .await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new().with("NoAI", true),
        [8.5, (SKY + 1) as f64, 8.5],
    )
    .await;
    let zombie = mobs(&region, EntityType::Zombie)
        .await
        .first()
        .expect("the zombie spawned")
        .id;
    world.ticks(3).await;
    assert!(
        falling_blocks(&region).await.is_empty(),
        "held by its ceiling"
    );

    place_blocks(&world, &region, [(ceiling, Block::Air.default_state())]).await;
    let mut fell = false;
    for _ in 0..5 {
        world.ticks(1).await;
        if falling_blocks(&region).await.len() == 2 {
            fell = true;
            break;
        }
    }
    assert!(fell, "the whole column fell together");
    for pos in column {
        assert_eq!(
            block_at(&region, pos).await,
            Some(Block::Air.default_state())
        );
    }
    for _ in 0..60 {
        world.ticks(1).await;
        if falling_blocks(&region).await.is_empty() {
            break;
        }
    }
    assert!(falling_blocks(&region).await.is_empty(), "both landed");
    assert_eq!(
        block_at(&region, BlockPos::new(8, SKY + 1, 8)).await,
        Some(Block::Air.default_state()),
        "a stalactite cannot stand on the floor, so it broke"
    );
    assert_eq!(
        world_items(&region, "dripstone").await,
        2,
        "each piece dropped itself"
    );

    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id: zombie, reply })
        .await;
    let health = answer
        .await
        .unwrap()
        .expect("the zombie is still there")
        .get("Health")
        .and_then(|t| t.as_float())
        .expect("zombies save their health");
    assert!(
        health < 20.0,
        "the falling tip hurt the zombie (health {health})"
    );
    world.shutdown().await;
}

async fn zombie_health(region: &RegionHandle, id: i32) -> Option<f32> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    let nbt = answer.await.unwrap()?;
    match nbt.get("Health") {
        Some(Tag::Float(health)) => Some(*health),
        _ => None,
    }
}

/// Drops `anvil` from `height` blocks above a stone floor, onto a zombie
/// standing there if `zombie`; returns the region, the zombie's id and the
/// landing cell once nothing is falling any more.
async fn drop_anvil(
    world: &TestWorld,
    anvil: Block,
    height: i32,
    zombie: bool,
) -> (RegionHandle, Option<i32>, BlockPos) {
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let floor = BlockPos::new(8, SKY, 8);
    let support = BlockPos::new(8, SKY + height, 8);
    let top = BlockPos::new(8, SKY + height + 1, 8);
    place_blocks(
        world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
            (top, anvil.default_state()),
        ],
    )
    .await;
    let zombie = if zombie {
        summon(
            &region,
            EntityType::Zombie,
            Compound::new().with("NoAI", true),
            [8.5, (SKY + 1) as f64, 8.5],
        )
        .await;
        let mobs = mobs(&region, EntityType::Zombie).await;
        Some(mobs.first().expect("the zombie spawned").id)
    } else {
        None
    };
    place_blocks(world, &region, [(support, Block::Air.default_state())]).await;
    let mut settled = false;
    for _ in 0..100 {
        world.ticks(1).await;
        if falling_blocks(&region).await.is_empty() {
            settled = true;
            break;
        }
    }
    assert!(settled, "the anvil stopped falling");
    std::mem::drop(events);
    (region, zombie, BlockPos::new(8, SKY + 1, 8))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_anvil_hurts_a_mob_it_lands_on() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (region, zombie, landing) = drop_anvil(&world, Block::Anvil, 7, true).await;
    let zombie = zombie.unwrap();
    // Seven blocks fallen: ceil(7 - 1) * 2 = 12 damage, less a little for
    // the zombie's 2 armor points.
    let health = zombie_health(&region, zombie)
        .await
        .expect("the zombie lives");
    assert!(
        (7.0..9.0).contains(&health),
        "the anvil took about 12 health, left {health}"
    );
    let block = block_at(&region, landing).await.map(|s| s.block());
    assert!(
        matches!(block, Some(Block::Anvil | Block::ChippedAnvil)),
        "it landed, at worst chipped: {block:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_anvil_breaks_apart_after_a_long_fall() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    // 24 blocks fallen: the degrade chance is 0.05 + 23 * 0.05, a
    // certainty. The roll does not need anything under the anvil.
    let (region, _, landing) = drop_anvil(&world, Block::DamagedAnvil, 24, false).await;
    assert_eq!(
        block_at(&region, landing).await.map(|s| s.block()),
        Some(Block::Air),
        "the anvil broke apart instead of landing"
    );
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let items = answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntityType::Item)
        .count();
    assert_eq!(items, 0, "nothing dropped");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_anvil_hurts_a_player_under_it_but_not_a_spectator() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (viewer, mut viewer_events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut viewer_events, chunk).await;
    let region = world.region(chunk.region()).await;
    let floor = BlockPos::new(8, SKY, 8);
    let support = BlockPos::new(8, SKY + 6, 8);
    let top = BlockPos::new(8, SKY + 7, 8);
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
            (top, Block::Anvil.default_state()),
        ],
    )
    .await;
    let feet = [8.5, (SKY + 1) as f64, 8.5];
    let (player, mut player_events) = PlayerHandle::new(200);
    region.request(update(&player, 1, feet)).await;
    let (spectator, mut spectator_events) = PlayerHandle::new(300);
    let RegionRequest::Player(mut ghost) = update(&spectator, 1, feet) else {
        unreachable!()
    };
    ghost.spectator = true;
    region.request(RegionRequest::Player(ghost)).await;
    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;
    let mut hits = (0, 0);
    for _ in 0..60 {
        world.ticks(1).await;
        while let Ok(event) = player_events.try_recv() {
            hits.0 += matches!(event, PlayerEvent::EntityHurt(_)) as u32;
        }
        while let Ok(event) = spectator_events.try_recv() {
            hits.1 += matches!(event, PlayerEvent::EntityHurt(_)) as u32;
        }
        if falling_blocks(&region).await.is_empty() {
            break;
        }
    }
    assert_eq!(hits, (1, 0), "(player, spectator) hits");
    std::mem::drop(viewer_events);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_falling_block_hands_its_block_entity_data_to_the_landed_block() {
    use tuff_nbt::{Compound, Tag};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let floor = BlockPos::new(8, SKY, 8);
    place_blocks(&world, &region, [(floor, Block::Stone.default_state())]).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:falling_block")
                .with("BlockState", "minecraft:chest")
                .with(
                    "TileEntityData",
                    Compound::new().with("CustomName", Tag::String("\"Fallen\"".into())),
                ),
            position: [8.5, f64::from(SKY) + 3.0, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let landed = BlockPos::new(8, SKY + 1, 8);
    for _ in 0..40 {
        world.ticks(1).await;
        if block_at(&region, landed).await.map(|s| s.block()) == Some(Block::Chest) {
            break;
        }
    }
    world.ticks(2).await;
    let (reply, copied) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: crate::world::messages::BlockOp::Copy {
                positions: vec![landed],
            },
            reply,
        })
        .await;
    let crate::world::messages::BlockOpResult::Copied(mut found) = copied.await.unwrap() else {
        panic!("copied");
    };
    let (state, data) = found.remove(0).expect("loaded");
    assert_eq!(state.block(), Block::Chest);
    assert!(
        data.is_some_and(|nbt| nbt.get_str("CustomName").is_some()),
        "the data reached the chest"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalactite_due_to_fall_keeps_its_thickness() {
    use crate::world::messages::{BlockUpdate, RegionInternal};
    use tuff_vanilla_data::block::{Thickness, VerticalDirection};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let drip = |thickness| {
        Block::PointedDripstone
            .default_state()
            .with_vertical_direction(VerticalDirection::Down)
            .unwrap()
            .with_thickness(thickness)
            .unwrap()
    };
    let ceiling = BlockPos::new(8, SKY + 10, 8);
    let (top, tip) = (BlockPos::new(8, SKY + 9, 8), BlockPos::new(8, SKY + 8, 8));
    place_blocks(
        &world,
        &region,
        [
            (ceiling, Block::Stone.default_state()),
            (top, drip(Thickness::Frustum)),
            (tip, drip(Thickness::Tip)),
        ],
    )
    .await;
    let air = Block::Air.default_state();
    let lose = |pos| BlockUpdate::Place {
        pos,
        state: air,
        cause: Default::default(),
    };
    region.internal(RegionInternal::BlockUpdates {
        tick: None,
        updates: vec![lose(ceiling)],
    });
    region.internal(RegionInternal::BlockUpdates {
        tick: None,
        updates: vec![lose(tip)],
    });
    world.ticks(1).await;
    assert_eq!(
        block_at(&region, top).await.and_then(|s| s.thickness()),
        Some(Thickness::Frustum),
        "already falling, it does not reshape into a tip"
    );
    world.shutdown().await;
}
