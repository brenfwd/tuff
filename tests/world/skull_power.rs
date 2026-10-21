//! Skulls and heads follow the redstone signal around them.

use super::jukebox::{set, start, wait_prop};
use super::*;
use tuff_vanilla_data::Block;

const Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_head_is_powered_while_a_neighbouring_source_is_on() {
    let (_dir, world, region, _player, _events) = start().await;
    let floor = BlockPos::new(X, Y, Z);
    let wall = BlockPos::new(X, Y, Z + 4);
    set(&region, floor, Block::PiglinHead.default_state(), None).await;
    set(
        &region,
        wall,
        "minecraft:piglin_wall_head[facing=north]".parse().unwrap(),
        None,
    )
    .await;
    wait_prop(&world, &region, floor, "powered", "false").await;
    let source = BlockPos::new(X + 1, Y, Z);
    set(&region, source, Block::RedstoneBlock.default_state(), None).await;
    set(
        &region,
        BlockPos::new(X, Y, Z + 5),
        Block::RedstoneBlock.default_state(),
        None,
    )
    .await;
    wait_prop(&world, &region, floor, "powered", "true").await;
    wait_prop(&world, &region, wall, "powered", "true").await;
    set(&region, source, Block::Air.default_state(), None).await;
    wait_prop(&world, &region, floor, "powered", "false").await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_fills_a_decorated_pot_up_to_a_stack() {
    use tuff_nbt::{Compound, Tag};
    let (_dir, world, region, _player, _events) = start().await;
    let pot = BlockPos::new(X, Y, Z + 20);
    set(
        &region,
        BlockPos::new(X, Y - 1, Z + 20),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, pot, Block::DecoratedPot.default_state(), None).await;
    let items = Compound::new().with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", 0i8)
                .with("id", "minecraft:ender_pearl")
                .with("count", 3i32),
        )]),
    );
    set(
        &region,
        BlockPos::new(X, Y + 1, Z + 20),
        Block::Hopper.default_state(),
        Some(items),
    )
    .await;
    world.ticks(60).await;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pot,
            max: pot,
            run: crate::world::region_api::RegionTask::new(move |world| {
                let count = world
                    .raw_block_entity(pot)
                    .and_then(|nbt| nbt.get_compound("item"))
                    .and_then(|item| item.get_int("count"));
                let _ = reply.send(count);
            }),
        })
        .await;
    assert_eq!(answer.await.unwrap(), Some(3));
    world.shutdown().await;
}
