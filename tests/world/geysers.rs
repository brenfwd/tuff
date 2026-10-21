//! Potent sulfur geysers: a continuous one lifts what floats over it, a
//! periodic one counts down between dormant and erupting, and a quiet
//! one gives nausea to whoever breathes its gas at the surface.

use super::*;
use crate::world::messages::PlayerUpdate;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::{EntityType, MobEffect};

const X: i32 = 8;
const Z: i32 = 8;

/// Potent sulfur at `SKY` over `under`, with `water` source blocks above
/// it in a glass shaft, then open air.
async fn geyser(
    under: Block,
    water: i32,
    state: &str,
) -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(200);
    view_around(&world, &viewer, &mut events, chunk).await;
    let mut blocks: Vec<(BlockPos, BlockState)> = vec![
        (BlockPos::new(X, SKY - 2, Z), Block::Stone.default_state()),
        (BlockPos::new(X, SKY - 1, Z), under.default_state()),
    ];
    for y in SKY - 1..=SKY + water {
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            blocks.push((
                BlockPos::new(X + dx, y, Z + dz),
                Block::Glass.default_state(),
            ));
        }
    }
    for y in 1..=water {
        blocks.push((BlockPos::new(X, SKY + y, Z), Block::Water.default_state()));
    }
    let sulfur = format!("minecraft:potent_sulfur[potent_sulfur_state={state}]");
    blocks.push((BlockPos::new(X, SKY, Z), sulfur.parse().unwrap()));
    place_blocks(&world, &region, blocks).await;
    (dir, world, region, viewer, events)
}

async fn sulfur_state(region: &RegionHandle) -> String {
    block_at(region, BlockPos::new(X, SKY, Z))
        .await
        .and_then(|s| s.get("potent_sulfur_state").map(str::to_owned))
        .unwrap_or_default()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_continuous_geyser_lifts_an_item_out_of_its_water() {
    let (_dir, world, region, _viewer, _events) = geyser(Block::Lava, 2, "continuous").await;
    assert_eq!(sulfur_state(&region).await, "continuous");
    world.ticks(21).await;
    let start = f64::from(SKY) + 1.2;
    summon(
        &region,
        EntityType::Item,
        Compound::new().with(
            "Item",
            Compound::new()
                .with("id", "minecraft:stone")
                .with("count", 1i32),
        ),
        [f64::from(X) + 0.5, start, f64::from(Z) + 0.5],
    )
    .await;
    let mut highest = start;
    for _ in 0..30 {
        world.ticks(1).await;
        if let Some(item) = mobs(&region, EntityType::Item).await.first() {
            highest = highest.max(item.position[1]);
        }
    }
    assert!(
        highest > start + 3.0,
        "the plume throws it clear of the water: reached {highest}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_periodic_geyser_erupts_then_goes_dormant_again() {
    let (_dir, world, region, _viewer, _events) = geyser(Block::MagmaBlock, 1, "dormant").await;
    assert_eq!(sulfur_state(&region).await, "dormant");
    let mut seen = vec!["dormant".to_owned()];
    for _ in 0..80 {
        world.ticks(10).await;
        let state = sulfur_state(&region).await;
        if seen.last() != Some(&state) {
            seen.push(state);
        }
        if seen.len() == 3 {
            break;
        }
    }
    assert_eq!(seen, ["dormant", "erupting", "dormant"]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breathing_a_quiet_geysers_gas_gives_nausea() {
    let (_dir, world, region, _viewer, _events) = geyser(Block::Stone, 1, "wet").await;
    assert_eq!(sulfur_state(&region).await, "wet");
    let (player, mut events) = PlayerHandle::new(100);
    let RegionRequest::Player(mut update) = update(
        &player,
        1,
        [
            f64::from(X) + 0.5,
            f64::from(SKY) + 2.5 - 1.62,
            f64::from(Z) + 0.5,
        ],
    ) else {
        unreachable!()
    };
    update.looks.height = 1.8;
    update.looks.eye_height = 1.62;
    region
        .request(RegionRequest::Player(PlayerUpdate { ..update }))
        .await;
    world.ticks(21).await;
    let effect = expect(&mut events, |event| match event {
        PlayerEvent::GiveEffect {
            effect, duration, ..
        } => Some((effect, duration)),
        _ => None,
    })
    .await;
    assert_eq!(effect, (MobEffect::Nausea, 80));
    world.shutdown().await;
}
