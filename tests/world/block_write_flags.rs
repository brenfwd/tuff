//! Plugin block writes with and without the no-updates flag: physics and
//! redstone react to the default write and stay quiet for the quiet one.

use super::*;
use tuff_plugin_wasm::world::{BlockWrite, write};
use tuff_vanilla_data::{Block, BlockState};

const Y: i32 = 200;

fn write_at(pos: BlockPos, state: BlockState, updates: bool) -> BlockWrite {
    BlockWrite {
        dimension: DimensionId::default(),
        pos,
        state,
        updates,
    }
}

async fn block_now(region: &RegionHandle, pos: BlockPos) -> Block {
    block_at(region, pos).await.expect("loaded").block()
}

async fn settle(world: &TestWorld) {
    world.ticks(30).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quiet_writes_skip_physics_and_redstone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;

    let stone = Block::Stone.default_state();
    let floor = |x: i32| BlockPos::new(x, Y, 8);
    let top = |x: i32| BlockPos::new(x, Y + 2, 8);
    let floors = vec![
        write_at(floor(2), stone, true),
        write_at(floor(4), stone, true),
    ];
    write(world.shared.clone(), floors).await;
    let sand = Block::Sand.default_state();
    write(
        world.shared.clone(),
        vec![write_at(top(2), sand, true), write_at(top(4), sand, false)],
    )
    .await;
    settle(&world).await;
    assert_eq!(
        block_now(&region, top(2)).await,
        Block::Air,
        "default falls"
    );
    assert_eq!(block_now(&region, top(4)).await, Block::Sand, "quiet stays");

    let lamp = Block::RedstoneLamp.default_state();
    let power = Block::RedstoneBlock.default_state();
    let at = |x: i32, z: i32| BlockPos::new(x, Y + 1, z);
    write(
        world.shared.clone(),
        vec![
            write_at(at(11, 10), lamp, true),
            write_at(at(11, 14), lamp, true),
        ],
    )
    .await;
    write(
        world.shared.clone(),
        vec![
            write_at(at(10, 10), power, true),
            write_at(at(10, 14), power, false),
        ],
    )
    .await;
    settle(&world).await;
    let lit = |state: BlockState| state.to_string().contains("lit=true");
    let loud = block_at(&region, at(11, 10)).await.unwrap();
    let silent = block_at(&region, at(11, 14)).await.unwrap();
    assert!(lit(loud), "{loud}");
    assert!(!lit(silent), "{silent}");
    world.shutdown().await;
}
