//! Lightning strikes driven through the real per-region scheduler: the
//! weather gate (nothing strikes while clear, something eventually does
//! during a storm over a large enough loaded area).

use tuff_vanilla_data::registry::EntityType;

use super::*;
use crate::world::messages::ChunkTicket;

/// `/forceload`'s own limit (16x16 chunks), maximizing the strike chance
/// per tick without a real player's view distance.
const AREA: i32 = 15;

async fn force_load_area(world: &TestWorld) -> Vec<RegionHandle> {
    let dimension = world.dimension(OVERWORLD);
    let mut chunks = Vec::new();
    for z in 0..=AREA {
        for x in 0..=AREA {
            chunks.push(ChunkPos::new(x, z));
        }
    }
    let (reply, answer) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension,
            chunks,
            ticket: ChunkTicket::Forced,
            on: true,
            reply,
        })
        .await
        .unwrap();
    answer.await.unwrap();
    let mut regions = Vec::new();
    for region in chunks_regions() {
        regions.push(world.region(region).await);
    }
    regions
}

fn chunks_regions() -> Vec<RegionPos> {
    let mut regions: Vec<RegionPos> = (0..=AREA)
        .flat_map(|x| (0..=AREA).map(move |z| ChunkPos::new(x, z).region()))
        .collect();
    regions.sort();
    regions.dedup();
    regions
}

fn thundering(world: &TestWorld) {
    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.thundering = true;
        weather.rain_level = 1.0;
        weather.thunder_level = 1.0;
    });
}

async fn any_bolt(regions: &[RegionHandle]) -> bool {
    for region in regions {
        if snapshots(region)
            .await
            .iter()
            .any(|s| s.kind == EntityType::LightningBolt)
        {
            return true;
        }
    }
    false
}

#[tuff_core::watchdog]
#[tokio::test]
async fn no_lightning_strikes_a_clear_sky() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = force_load_area(&world).await;
    // Let the area finish loading and lighting before the window that
    // actually counts.
    world.ticks(30).await;
    for _ in 0..150 {
        assert!(
            !any_bolt(&region).await,
            "no strike should ever roll while the sky is clear"
        );
        world.ticks(1).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_thunderstorm_eventually_strikes_the_loaded_area() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = force_load_area(&world).await;
    world.ticks(30).await;
    thundering(&world);
    // 256 chunks x 1/100_000 per tick each: well over 99.9999% odds of at
    // least one hit within this budget (failure probability ~ e^-20).
    const BUDGET: u64 = 8_000;
    const STEP: u64 = 2;
    for _ in 0..BUDGET / STEP {
        if any_bolt(&region).await {
            return;
        }
        world.ticks(STEP).await;
    }
    assert!(
        any_bolt(&region).await,
        "no lightning bolt appeared within {BUDGET} ticks of a storm over {} chunks",
        (AREA + 1) * (AREA + 1)
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn lightning_rods_draw_strikes_and_lightning_scrapes_their_copper() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let dimension = world.dimension(OVERWORLD);
    let chunks: Vec<ChunkPos> = (0..8)
        .flat_map(|x| (0..8).map(move |z| ChunkPos::new(x, z)))
        .collect();
    let (reply, answer) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension,
            chunks,
            ticket: ChunkTicket::Forced,
            on: true,
            reply,
        })
        .await
        .unwrap();
    answer.await.unwrap();
    let region = world.region(RegionPos::new(0, 0)).await;
    world.ticks(30).await;
    let rod = BlockPos::new(64, 250, 64);
    place_blocks(
        &world,
        &region,
        vec![
            (BlockPos::new(64, 249, 64), Block::Stone.default_state()),
            (rod, Block::OxidizedLightningRod.default_state()),
        ],
    )
    .await;
    world.ticks(30).await;
    thundering(&world);
    const BUDGET: u64 = 30_000;
    const STEP: u64 = 2;
    for _ in 0..BUDGET / STEP {
        world.ticks(STEP).await;
        let state = block_at(&region, rod).await.unwrap();
        if state.block() != Block::OxidizedLightningRod {
            assert_eq!(
                state.block(),
                Block::LightningRod,
                "a strike scrapes the rod back to bare copper"
            );
            return;
        }
    }
    panic!("no strike reached the rod within {BUDGET} ticks");
}

async fn force_load(world: &TestWorld, chunks: Vec<ChunkPos>) {
    let dimension = world.dimension(OVERWORLD);
    let (reply, answer) = oneshot::channel();
    world
        .manager
        .send(ManagerMessage::ForceLoad {
            dimension,
            chunks,
            ticket: ChunkTicket::Forced,
            on: true,
            reply,
        })
        .await
        .unwrap();
    answer.await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_strike_goes_to_the_closest_rod_even_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    force_load(
        &world,
        (24..=34)
            .flat_map(|x| (-2..=2).map(move |z| ChunkPos::new(x, z)))
            .collect(),
    )
    .await;
    let west = world.region(RegionPos::new(0, 0)).await;
    let east = world.region(RegionPos::new(1, 0)).await;
    world.ticks(30).await;
    let (far, near) = (BlockPos::new(430, 250, 8), BlockPos::new(520, 250, 8));
    for (region, rod) in [(&west, far), (&east, near)] {
        place_blocks(
            &world,
            region,
            vec![
                (
                    BlockPos::new(rod.x, rod.y - 1, rod.z),
                    Block::Stone.default_state(),
                ),
                (rod, Block::OxidizedLightningRod.default_state()),
            ],
        )
        .await;
    }
    world.ticks(5).await;
    thundering(&world);
    world.ticks(1).await;

    let center = BlockPos::new(500, 251, 8);
    west.request(RegionRequest::WhenLoaded {
        min: center,
        max: center,
        run: crate::world::region_api::RegionTask::new(move |region| {
            tuff_feature_builtin_vanilla_entities::world::region::lightning::strike_rolled(
                region, center,
            );
        }),
    })
    .await;
    for _ in 0..10 {
        world.ticks(1).await;
        if block_at(&east, near).await.map(|s| s.block()) == Some(Block::LightningRod) {
            assert_eq!(
                block_at(&west, far).await.map(|s| s.block()),
                Some(Block::OxidizedLightningRod),
                "the farther rod in the rolling region is passed over"
            );
            return;
        }
    }
    panic!("the strike never reached the nearer rod in the next region");
}
