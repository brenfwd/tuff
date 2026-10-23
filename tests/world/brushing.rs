//! Suspicious sand and gravel: a player's brush uncovers the item they
//! hold stage by stage, brushing that stops decays back, and unsupported
//! they fall and shatter without dropping anything.

use super::*;
use crate::protocol::play::Serverbound;
use crate::world::inventory::Inventory;
use crate::world::region_api::RegionTask;
use crate::world::session::PlayerSession;
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::Face;

const SKY: i32 = 150;

async fn raw_block_entity(region: &RegionHandle, pos: BlockPos) -> Option<Compound> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                let _ = reply.send(world.raw_block_entity(pos).cloned());
            }),
        })
        .await;
    answer.await.unwrap()
}

async fn edit_block_entity(
    region: &RegionHandle,
    pos: BlockPos,
    edit: impl FnOnce(&mut Compound) + Send + 'static,
) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                if let Some(nbt) = world.raw_block_entity_mut(pos) {
                    edit(nbt);
                }
                let _ = reply.send(());
            }),
        })
        .await;
    answer.await.unwrap();
}

async fn entities_of(region: &RegionHandle, kind: EntityType) -> Vec<messages::EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn dusted(state: Option<tuff_vanilla_data::BlockState>) -> Option<String> {
    state.and_then(|s| s.get("dusted").map(str::to_owned))
}

struct Bench {
    world: TestWorld,
    region: RegionHandle,
    sand: BlockPos,
    _viewer: (PlayerHandle, mpsc::UnboundedReceiver<PlayerEvent>),
}

async fn bench(dir: &Path, block: Block) -> Bench {
    let world = TestWorld::start(dir);
    let chunk = ChunkPos::new(0, 0);
    let (viewer, mut events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, sand) = (BlockPos::new(8, SKY, 8), BlockPos::new(8, SKY + 1, 8));
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (sand, block.default_state()),
        ],
    )
    .await;
    Bench {
        world,
        region,
        sand,
        _viewer: (viewer, events),
    }
}

/// A player standing on the block at `sand`, looking straight down at it
/// with a brush in the main hand, already brushing.
async fn brusher(bench: &Bench) -> PlayerSession {
    let (mut player, outbound) = session_with_outbound(&bench.world, 1);
    std::mem::forget(outbound);
    let dimension = player.dimension;
    player
        .region_in(dimension, ChunkPos::of_block(bench.sand).region())
        .await;
    player.pose.position = [8.5, f64::from(SKY + 2), 8.5];
    player.pose.on_ground = true;
    player.pose.pitch = 90.0;
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Brush, 1, MintReason::Test)),
    );
    player
        .handle_packet(Serverbound::UseItemOn {
            hand: 0,
            position: bench.sand,
            face: Face::Up,
            cursor: [0.5, 1.0, 0.5],
            sequence: 1,
        })
        .await;
    assert!(
        player
            .handle_events_until(|event| matches!(event, PlayerEvent::ActionResult { .. }))
            .await
    );
    assert_eq!(player.actions.using.map(|u| u.item), Some(Item::Brush));
    player
}

/// One tick of both the player and the world, then the region's answers.
async fn tick_both(world: &TestWorld, player: &mut PlayerSession) {
    player.run_tick().await;
    world.ticks(1).await;
    while let Ok(event) = player.conn.events.try_recv() {
        player.handle_event(event).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn brushing_suspicious_sand_uncovers_its_loot_and_leaves_sand() {
    let dir = TestDir::new();
    let bench = bench(dir.path(), Block::SuspiciousSand).await;
    let sand = bench.sand;
    edit_block_entity(&bench.region, sand, |nbt| {
        nbt.insert("LootTable", "minecraft:archaeology/desert_pyramid");
        nbt.insert("LootTableSeed", 12345i64);
    })
    .await;
    let mut player = brusher(&bench).await;

    let mut stages = Vec::new();
    for _ in 0..5 {
        tick_both(&bench.world, &mut player).await;
    }
    let uncovered = raw_block_entity(&bench.region, sand).await.unwrap();
    assert!(
        !uncovered.contains_key("LootTable"),
        "the first stroke rolls the loot"
    );
    let item =
        tuff_vanilla_common::brushable::held_item(&uncovered).expect("the loot is uncovered");
    assert_eq!(
        uncovered.get_byte("hit_direction"),
        Some(1),
        "brushed from above"
    );

    let mut completed = false;
    for _ in 0..100 {
        tick_both(&bench.world, &mut player).await;
        let state = block_at(&bench.region, sand).await;
        if let Some(stage) = dusted(state)
            && stages.last() != Some(&stage)
        {
            stages.push(stage);
        }
        if state.map(|s| s.block()) == Some(Block::Sand) {
            completed = true;
            break;
        }
    }
    assert!(completed, "ten strokes turn it into sand");
    assert_eq!(stages, ["1", "2", "3"]);
    let dropped = entities_of(&bench.region, EntityType::Item).await;
    if let Some(dropped) = dropped.first() {
        assert_eq!(dropped.name, item.item().name());
        assert!(
            dropped.position[1] > f64::from(SKY + 2),
            "out of the brushed top face"
        );
    }
    let mut picked_up = false;
    for _ in 0..5 {
        tick_both(&bench.world, &mut player).await;
        picked_up |= player
            .inventory()
            .slots()
            .iter()
            .flatten()
            .any(|stack| stack.item() == item.item());
    }
    assert!(
        picked_up,
        "the loot pops out to the brusher, with no pickup delay"
    );
    assert_eq!(
        player.inventory().hand(false).map(|s| s.damage()),
        Some(1),
        "completing the brushing wears the brush"
    );
    bench.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn brushing_that_stops_decays_back_to_undusted() {
    let dir = TestDir::new();
    let bench = bench(dir.path(), Block::SuspiciousGravel).await;
    let sand = bench.sand;
    let mut player = brusher(&bench).await;
    for _ in 0..40 {
        tick_both(&bench.world, &mut player).await;
    }
    assert_eq!(
        dusted(block_at(&bench.region, sand).await).as_deref(),
        Some("2"),
        "four strokes"
    );
    player.stop_using();

    let mut stages = Vec::new();
    for _ in 0..80 {
        bench.world.ticks(1).await;
        let stage = dusted(block_at(&bench.region, sand).await).unwrap();
        if stages.last() != Some(&stage) {
            stages.push(stage);
        }
    }
    assert_eq!(
        stages,
        ["2", "1", "0"],
        "two strokes are lost every 4 ticks"
    );
    let nbt = raw_block_entity(&bench.region, sand).await.unwrap();
    assert!(
        !nbt.contains_key("hit_direction"),
        "undone brushing forgets its side"
    );
    assert_eq!(
        block_at(&bench.region, sand).await.map(|s| s.block()),
        Some(Block::SuspiciousGravel)
    );
    bench.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsupported_suspicious_sand_falls_and_shatters_without_a_drop() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (viewer, mut events) = PlayerHandle::new(100);
    view_around(&world, &viewer, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, support, sand) = (
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
            (sand, Block::SuspiciousSand.default_state()),
        ],
    )
    .await;
    edit_block_entity(&region, sand, |nbt| {
        nbt.insert(
            "item",
            tuff_item::item::ItemPicture::new(Item::Diamond, 1).to_nbt(),
        );
    })
    .await;
    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;

    let mut fell = false;
    let mut shattered = false;
    for _ in 0..60 {
        world.ticks(1).await;
        let falling = entities_of(&region, EntityType::FallingBlock).await;
        fell |= !falling.is_empty();
        if fell && falling.is_empty() {
            shattered = true;
            break;
        }
    }
    assert!(fell && shattered, "it fell, then broke on landing");
    assert_eq!(
        block_at(&region, sand).await.map(|s| s.block()),
        Some(Block::Air)
    );
    assert_eq!(
        block_at(&region, support).await.map(|s| s.block()),
        Some(Block::Air),
        "it does not settle"
    );
    assert!(
        entities_of(&region, EntityType::Item).await.is_empty(),
        "neither the block nor its item drop"
    );
    world.shutdown().await;
}
