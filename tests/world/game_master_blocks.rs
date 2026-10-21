//! Game master blocks: the jigsaw and test block editors' saves, the test
//! block's redstone trigger and the signal a powered start block gives.

use super::jukebox::{prop, saved, set, start, wait_prop};
use super::*;
use crate::protocol::play::JigsawEdit;
use crate::world::block_entity::editors;
use crate::world::region_api::RegionTask;
use tuff_vanilla_data::BlockState;

const X: i32 = 84;
const Y: i32 = 200;
const Z: i32 = 84;

fn state(text: &str) -> BlockState {
    text.parse().unwrap()
}

async fn run<T: Send + 'static>(
    region: &RegionHandle,
    pos: BlockPos,
    job: impl FnOnce(&mut dyn crate::world::region_api::RegionWorld) -> T + Send + 'static,
) -> T {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                let _ = reply.send(job(world));
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_jigsaw_editor_saves_its_fields_into_the_block() {
    let (_dir, world, region, _player, _events) = start().await;
    let jigsaw = BlockPos::new(X, Y, Z + 8);
    set(
        &region,
        jigsaw,
        state("minecraft:jigsaw[orientation=north_up]"),
        None,
    )
    .await;
    let edit = JigsawEdit {
        position: jigsaw,
        name: "test:start".to_owned(),
        target: "test:end".to_owned(),
        pool: "test:pool".to_owned(),
        final_state: "minecraft:stone".to_owned(),
        joint: "aligned",
        selection_priority: 3,
        placement_priority: 7,
    };
    assert!(
        run(&region, jigsaw, move |w| editors::apply_jigsaw_edit(
            w, &edit
        ))
        .await
    );
    let nbt = saved(&region, jigsaw).await;
    assert_eq!(nbt.get_str("name"), Some("test:start"));
    assert_eq!(nbt.get_str("target"), Some("test:end"));
    assert_eq!(nbt.get_str("pool"), Some("test:pool"));
    assert_eq!(nbt.get_str("final_state"), Some("minecraft:stone"));
    assert_eq!(nbt.get_str("joint"), Some("aligned"));
    assert_eq!(nbt.get_int("selection_priority"), Some(3));
    assert_eq!(nbt.get_int("placement_priority"), Some(7));

    let elsewhere = BlockPos::new(X, Y, Z + 10);
    let stone = JigsawEdit {
        position: elsewhere,
        name: String::new(),
        target: String::new(),
        pool: String::new(),
        final_state: String::new(),
        joint: "aligned",
        selection_priority: 0,
        placement_priority: 0,
    };
    assert!(
        !run(&region, elsewhere, move |w| editors::apply_jigsaw_edit(
            w, &stone
        ))
        .await
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_test_block_editor_sets_the_mode_and_message() {
    let (_dir, world, region, _player, _events) = start().await;
    let block = BlockPos::new(X, Y, Z + 12);
    set(
        &region,
        block,
        state("minecraft:test_block[mode=fail]"),
        None,
    )
    .await;
    assert!(
        run(&region, block, move |w| editors::apply_test_block_edit(
            w, block, 1, "checked"
        ))
        .await
    );
    assert_eq!(prop(&region, block, "mode").await.unwrap(), "log");
    let nbt = saved(&region, block).await;
    assert_eq!(nbt.get_str("mode"), Some("log"));
    assert_eq!(nbt.get_str("message"), Some("checked"));
    assert!(
        run(&region, block, move |w| editors::apply_test_block_edit(
            w, block, 99, ""
        ))
        .await
    );
    assert_eq!(prop(&region, block, "mode").await.unwrap(), "start");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redstone_triggers_a_test_block_and_a_powered_start_block_powers_a_lamp() {
    let (_dir, world, region, _player, _events) = start().await;
    let log = BlockPos::new(X, Y, Z);
    set(&region, log, state("minecraft:test_block[mode=log]"), None).await;
    set(
        &region,
        BlockPos::new(X + 1, Y, Z),
        Block::RedstoneBlock.default_state(),
        None,
    )
    .await;
    world.ticks(2).await;
    assert_eq!(saved(&region, log).await.get_bool("powered"), Some(true));
    set(
        &region,
        BlockPos::new(X + 1, Y, Z),
        Block::Air.default_state(),
        None,
    )
    .await;
    world.ticks(2).await;
    assert_eq!(saved(&region, log).await.get_bool("powered"), Some(false));

    let start_block = BlockPos::new(X, Y, Z + 4);
    let lamp = BlockPos::new(X + 1, Y, Z + 4);
    set(
        &region,
        start_block,
        state("minecraft:test_block[mode=start]"),
        None,
    )
    .await;
    set(&region, lamp, Block::RedstoneLamp.default_state(), None).await;
    world.ticks(2).await;
    assert_eq!(prop(&region, lamp, "lit").await.unwrap(), "false");

    run(&region, start_block, move |w| {
        w.raw_block_entity_mut(start_block)
            .unwrap()
            .insert("powered", true);
        w.update_neighbors_at(start_block, Block::TestBlock);
    })
    .await;
    wait_prop(&world, &region, lamp, "lit", "true").await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_game_masters_use_the_editor_blocks() {
    let (_dir, world, region, player, mut events) = start().await;
    let block = BlockPos::new(X, Y, Z + 16);
    set(
        &region,
        block,
        state("minecraft:test_block[mode=fail]"),
        None,
    )
    .await;
    for (sequence, game_master) in [(1, false), (2, true)] {
        let mut click = place(&player, block, Item::Stick, sequence);
        click.item = None;
        click.held = None;
        click.held_count = 0;
        click.game_master = game_master;
        region.request(RegionRequest::Place(click)).await;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::ActionResult {
                sequence: seen,
                outcome,
            } if seen == sequence => Some(outcome),
            _ => None,
        })
        .await;
        assert_eq!(
            matches!(outcome, ActionOutcome::Interacted { .. }),
            game_master,
            "{outcome:?}"
        );
    }
    world.shutdown().await;
}
