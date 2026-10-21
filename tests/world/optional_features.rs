use super::*;
use crate::world::messages::{BlockOp, BlockOpResult};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::BlockState;

const Y: i32 = 200;

fn shipped_beyond_vanilla(enabled: &[&str]) -> Vec<Box<dyn crate::feature::Feature>> {
    let enabled: Vec<String> = enabled.iter().map(|id| (*id).to_owned()).collect();
    let mut features = tuff::features::shipped(&enabled).unwrap();
    let vanilla = tuff::vanilla_features::all();
    let extra = features.split_off(vanilla.len());
    let ids = |list: &[Box<dyn crate::feature::Feature>]| -> Vec<&'static str> {
        list.iter().map(|feature| feature.id()).collect()
    };
    assert_eq!(ids(&features), ids(&vanilla));
    assert_eq!(extra.len(), enabled.len() + 2);
    extra
}

async fn blocks(region: &RegionHandle, op: BlockOp) -> BlockOpResult {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap()
}

async fn paste(region: &RegionHandle, x: i32, z: i32, state: &str, nbt: Option<Compound>) {
    let state: BlockState = state.parse().unwrap();
    let changes = vec![(BlockPos::new(x, Y, z), state, nbt)];
    blocks(
        region,
        BlockOp::Paste {
            changes,
            strict: false,
        },
    )
    .await;
}

async fn copy(region: &RegionHandle, x: i32, z: i32) -> (BlockState, Option<Compound>) {
    let positions = vec![BlockPos::new(x, Y, z)];
    match blocks(region, BlockOp::Copy { positions }).await {
        BlockOpResult::Copied(mut found) => found.remove(0).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn diamonds(nbt: Option<&Compound>) -> Option<i32> {
    let Some(Tag::List(slots)) = nbt?.get("Items") else {
        return None;
    };
    let slot = slots.first()?.as_compound()?;
    if slot.get_str("id")? != "minecraft:diamond" {
        return None;
    }
    slot.get_int("count")
}

/// Pushes a chest holding five diamonds east with a piston in a world
/// running the shipped feature list; returns what ends up at the chest's
/// old and new positions.
async fn push_chest(enabled: &[&str]) -> [(BlockState, Option<Compound>); 2] {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), shipped_beyond_vanilla(enabled));
    let (player, mut events) = PlayerHandle::new(100);
    view_around(&world, &player, &mut events, ChunkPos::new(5, 5)).await;
    let region = world.region(ChunkPos::new(5, 5).region()).await;
    paste(&region, 84, 86, "minecraft:piston[facing=east]", None).await;
    let chest = Compound::new().with("id", "minecraft:chest").with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", Tag::Byte(0))
                .with("id", "minecraft:diamond")
                .with("count", Tag::Int(5)),
        )]),
    );
    paste(
        &region,
        85,
        86,
        "minecraft:chest[facing=north]",
        Some(chest),
    )
    .await;
    paste(&region, 84, 87, "minecraft:redstone_block", None).await;
    world.ticks(4).await;
    let result = [copy(&region, 85, 86).await, copy(&region, 86, 86).await];
    world.shutdown().await;
    result
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shipped_server_keeps_chests_immovable_by_default() {
    let [origin, beyond] = push_chest(&[]).await;
    assert_eq!(origin.0.block(), Block::Chest);
    assert_eq!(diamonds(origin.1.as_ref()), Some(5));
    assert_eq!(beyond.0.block(), Block::Air);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enabling_the_optional_feature_lets_a_pushed_chest_keep_its_items() {
    let [origin, beyond] = push_chest(&[tuff_feature_builtin_moving_block_entities::ID]).await;
    assert_eq!(origin.0.block(), Block::PistonHead);
    assert_eq!(beyond.0.block(), Block::Chest);
    assert_eq!(diamonds(beyond.1.as_ref()), Some(5));
}
