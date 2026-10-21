//! Cramming and pushing in tight spots: climbers are not pushable, so a
//! crowd standing in a ladder, vine or scaffolding cell never crams.

use super::*;
use tuff_vanilla_data::registry::EntityType;

async fn pigs_left_in_hole(climbable: Option<Block>, count: usize) -> usize {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    let mut blocks = pen(24, 24);
    if let Some(block) = climbable {
        blocks.push((BlockPos::new(24, SKY + 1, 24), block.default_state()));
    }
    place_blocks(&world, &region, blocks).await;
    for _ in 0..count {
        summon(
            &region,
            EntityType::Pig,
            Compound::new().with("NoAI", true),
            [24.5, SKY as f64 + 1.0, 24.5],
        )
        .await;
    }
    world.ticks(100).await;
    let left = mobs(&region, EntityType::Pig).await.len();
    world.shutdown().await;
    left
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crowd_on_a_ladder_in_a_one_block_hole_never_crams() {
    assert_eq!(pigs_left_in_hole(Some(Block::Ladder), 40).await, 40);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vines_and_scaffolding_stop_cramming_like_ladders() {
    for block in [Block::Vine, Block::Scaffolding] {
        assert_eq!(pigs_left_in_hole(Some(block), 30).await, 30, "{block:?}");
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_crowd_without_a_ladder_crams_down_to_the_rule() {
    let left = pigs_left_in_hole(None, 30).await;
    assert!((23..30).contains(&left), "{left} left");
}
