//! Farmland trampled by landing players and mobs, and entities lifted out
//! of a dirt path or farmland that turns into full-height dirt.

use super::*;
use crate::world::game_rules::RuleValue;
use crate::world::messages::{BlockOp, PlayerUpdate};
use tuff_vanilla_data::registry::EntityType;

const FARMLAND_TOP: f64 = SKY as f64 + 0.9375;

fn quiet_farmland(world: &TestWorld) {
    world.game_rules.send_modify(|rules| {
        rules
            .set("random_tick_speed", RuleValue::Int(0))
            .expect("an integer rule");
    });
}

fn landing(
    player: &PlayerHandle,
    seq: u64,
    position: [f64; 3],
    landed: Option<f64>,
) -> RegionRequest {
    let RegionRequest::Player(update) = update(player, seq, position) else {
        unreachable!("update builds a player update");
    };
    RegionRequest::Player(PlayerUpdate {
        landed,
        grounded: true,
        ..update
    })
}

async fn farm_world() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    quiet_farmland(&world);
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, chunk).await;
    let mut blocks = pen(2, 13);
    for x in 3..=12 {
        blocks.push((BlockPos::new(x, SKY, 4), Block::Farmland.default_state()));
        blocks.push((BlockPos::new(x, SKY, 10), Block::Farmland.default_state()));
    }
    place_blocks(&world, &region, blocks).await;
    (dir, world, region, player, events)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_landing_from_a_height_tramples_farmland_but_a_step_does_not() {
    let (_dir, world, region, player, mut events) = farm_world().await;
    let high = BlockPos::new(4, SKY, 4);
    let low = BlockPos::new(8, SKY, 4);
    let at = |pos: BlockPos| [pos.x as f64 + 0.5, FARMLAND_TOP, pos.z as f64 + 0.5];

    region
        .request(landing(&player, 1, at(low), Some(0.5)))
        .await;
    region.request(landing(&player, 2, at(high), None)).await;
    region
        .request(landing(&player, 3, at(high), Some(3.0)))
        .await;
    world.ticks(1).await;

    assert_eq!(
        block_at(&region, low).await.unwrap().block(),
        Block::Farmland
    );
    assert_eq!(block_at(&region, high).await.unwrap().block(), Block::Dirt);
    let y = expect(&mut events, |event| match event {
        PlayerEvent::PushedUp { y, .. } => Some(y),
        _ => None,
    })
    .await;
    assert_eq!(y, SKY as f64 + 1.0, "the player is lifted onto the dirt");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_falling_onto_farmland_trample_it_unless_small_or_barely_falling() {
    let (_dir, world, region, _player, _events) = farm_world().await;
    let cow_drop = BlockPos::new(4, SKY, 10);
    let cow_step = BlockPos::new(8, SKY, 10);
    let chicken_drop = BlockPos::new(12, SKY, 10);
    let above = |pos: BlockPos, height: f64| {
        [
            pos.x as f64 + 0.5,
            FARMLAND_TOP + height,
            pos.z as f64 + 0.5,
        ]
    };
    let no_ai = || Compound::new().with("NoAI", Tag::Byte(1));
    summon(&region, EntityType::Cow, no_ai(), above(cow_drop, 6.0)).await;
    summon(&region, EntityType::Cow, no_ai(), above(cow_step, 0.3)).await;
    summon(
        &region,
        EntityType::Chicken,
        no_ai(),
        above(chicken_drop, 6.0),
    )
    .await;
    for _ in 0..80 {
        world.ticks(1).await;
        if mobs(&region, EntityType::Cow)
            .await
            .iter()
            .all(|c| c.position[1] < FARMLAND_TOP + 0.2)
            && block_at(&region, cow_drop).await.unwrap().block() == Block::Dirt
        {
            break;
        }
    }
    world.ticks(20).await;
    assert_eq!(
        block_at(&region, cow_drop).await.unwrap().block(),
        Block::Dirt,
        "a cow falling 6 blocks tramples"
    );
    assert_eq!(
        block_at(&region, cow_step).await.unwrap().block(),
        Block::Farmland,
        "a short drop does not"
    );
    assert_eq!(
        block_at(&region, chicken_drop).await.unwrap().block(),
        Block::Farmland,
        "a chicken is too small"
    );
    let cows = mobs(&region, EntityType::Cow).await;
    let trampler = cows
        .iter()
        .find(|c| (c.position[0] - 4.5).abs() < 0.5)
        .unwrap();
    assert_eq!(
        trampler.position[1],
        SKY as f64 + 1.0,
        "the cow stands on the dirt"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mob_griefing_off_keeps_mobs_from_trampling() {
    let (_dir, world, region, _player, _events) = farm_world().await;
    world.game_rules.send_modify(|rules| {
        rules
            .set("mob_griefing", RuleValue::Bool(false))
            .expect("a boolean rule");
    });
    let pos = BlockPos::new(4, SKY, 10);
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", Tag::Byte(1)),
        [4.5, FARMLAND_TOP + 6.0, 10.5],
    )
    .await;
    world.ticks(60).await;
    let cow = &mobs(&region, EntityType::Cow).await[0];
    assert!(cow.position[1] < FARMLAND_TOP + 0.1, "the cow landed");
    assert_eq!(
        block_at(&region, pos).await.unwrap().block(),
        Block::Farmland
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dirt_path_turning_into_dirt_lifts_the_mob_standing_in_it() {
    let (_dir, world, region, _player, _events) = farm_world().await;
    let pos = BlockPos::new(8, SKY, 7);
    place_blocks(
        &world,
        &region,
        vec![(pos, Block::DirtPath.default_state())],
    )
    .await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", Tag::Byte(1)),
        [8.5, FARMLAND_TOP, 7.5],
    )
    .await;
    world.ticks(5).await;
    let before = mobs(&region, EntityType::Cow).await[0].position[1];
    assert!(
        (before - FARMLAND_TOP).abs() < 1e-6,
        "the cow stands in the path ({before})"
    );
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: vec![(
                    BlockPos::new(pos.x, pos.y + 1, pos.z),
                    Block::Stone.default_state(),
                )],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    answer.await.unwrap();
    world.ticks(3).await;
    assert_eq!(
        block_at(&region, pos).await.unwrap().block(),
        Block::Dirt,
        "a covered path reverts"
    );
    let after = mobs(&region, EntityType::Cow).await[0].position[1];
    assert_eq!(after, SKY as f64 + 1.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setblock_turning_a_path_into_dirt_does_not_lift_the_mob() {
    let (_dir, world, region, _player, _events) = farm_world().await;
    let pos = BlockPos::new(8, SKY, 7);
    place_blocks(
        &world,
        &region,
        vec![(pos, Block::DirtPath.default_state())],
    )
    .await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", Tag::Byte(1)),
        [8.5, FARMLAND_TOP, 7.5],
    )
    .await;
    world.ticks(5).await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: vec![(pos, Block::Dirt.default_state())],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    answer.await.unwrap();
    world.ticks(3).await;
    assert_eq!(block_at(&region, pos).await.unwrap().block(), Block::Dirt);
    let after = mobs(&region, EntityType::Cow).await[0].position[1];
    assert!(after < SKY as f64 + 1.0, "{after}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_protection_keeps_players_from_trampling_unless_gamemaster() {
    use std::sync::atomic::Ordering::Relaxed;
    let (_dir, world, region, player, _events) = farm_world().await;
    world.shared.spawn_protection.store(100_000, Relaxed);
    world.shared.ops_present.store(true, Relaxed);
    let high = BlockPos::new(4, SKY, 4);
    let op = BlockPos::new(8, SKY, 4);
    let at = |pos: BlockPos| [pos.x as f64 + 0.5, FARMLAND_TOP, pos.z as f64 + 0.5];

    region
        .request(landing(&player, 1, at(high), Some(3.0)))
        .await;
    world.ticks(1).await;
    assert_eq!(
        block_at(&region, high).await.unwrap().block(),
        Block::Farmland
    );

    let RegionRequest::Player(update) = landing(&player, 2, at(op), Some(3.0)) else {
        unreachable!("landing builds a player update");
    };
    region
        .request(RegionRequest::Player(PlayerUpdate {
            gamemaster: true,
            ..update
        }))
        .await;
    world.ticks(1).await;
    assert_eq!(block_at(&region, op).await.unwrap().block(), Block::Dirt);
    world.shutdown().await;
}
