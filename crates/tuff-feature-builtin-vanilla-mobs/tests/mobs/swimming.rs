//! Water mobs swimming as the game moves them: along paths through the
//! water at their own stroke speed, never leaving it, and flopping about
//! when stranded.

use super::guardian::water_pool;
use super::*;
use tuff_vanilla_data::registry::EntityType;

const POOL_MIN: i32 = 1;
const POOL_MAX: i32 = 12;
const POOL_HEIGHT: i32 = 6;

struct Swim {
    positions: Vec<[f64; 3]>,
}

impl Swim {
    fn fastest_horizontal_step(&self) -> f64 {
        self.positions
            .windows(2)
            .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][2] - w[0][2]).powi(2)).sqrt())
            .fold(0.0, f64::max)
    }

    fn farthest_from_start(&self) -> f64 {
        let start = self.positions[0];
        self.positions
            .iter()
            .map(|p| crate::world::distance_sq(*p, start).sqrt())
            .fold(0.0, f64::max)
    }

    fn stays_in_pool(&self) -> bool {
        let (min, max) = (f64::from(POOL_MIN), f64::from(POOL_MAX + 1));
        let (bottom, top) = (f64::from(SKY + 1), f64::from(SKY + 1 + POOL_HEIGHT));
        self.positions.iter().all(|[x, y, z]| {
            (min..=max).contains(x) && (min..=max).contains(z) && (bottom..=top).contains(y)
        })
    }
}

/// Puts one `kind` in the middle of a sealed pool and records where it is
/// after each of `ticks` ticks.
async fn swim(kind: EntityType, ticks: usize) -> Swim {
    swim_with(kind, Compound::new(), ticks).await
}

/// [`swim`] for a `kind` summoned with `nbt`.
async fn swim_with(kind: EntityType, nbt: Compound, ticks: usize) -> Swim {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, water_pool(POOL_MIN, POOL_MAX, POOL_HEIGHT)).await;
    let start = [6.5, f64::from(SKY) + 3.0, 6.5];
    summon(&region, kind, nbt, start).await;
    let mut positions = Vec::with_capacity(ticks + 1);
    for _ in 0..=ticks {
        let found = mobs(&region, kind).await;
        assert_eq!(found.len(), 1, "{found:#?}");
        positions.push(found[0].position);
        world.ticks(1).await;
    }
    world.shutdown().await;
    Swim { positions }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fish_swim_about_their_pool_at_a_fish_pace() {
    for kind in [EntityType::Cod, EntityType::TropicalFish] {
        let swim = swim(kind, 400).await;
        assert!(swim.farthest_from_start() > 1.5, "{kind:?} swims somewhere");
        assert!(
            swim.fastest_horizontal_step() < 0.1,
            "{kind:?} tops out near its 0.063 blocks a tick stroke, not {}",
            swim.fastest_horizontal_step()
        );
        assert!(swim.stays_in_pool(), "{kind:?}: {:?}", swim.positions);
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dolphins_swim_quickly_but_within_their_stroke() {
    let rested = Compound::new().with("Air", 4800i16);
    let swim = swim_with(EntityType::Dolphin, rested, 300).await;
    assert!(swim.farthest_from_start() > 1.5);
    assert!(
        swim.fastest_horizontal_step() < 0.25,
        "{}",
        swim.fastest_horizontal_step()
    );
    assert!(swim.stays_in_pool(), "{:?}", swim.positions);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn squids_swim_in_pulses() {
    let swim = swim(EntityType::Squid, 300).await;
    assert!(swim.farthest_from_start() > 1.0);
    assert!(
        swim.fastest_horizontal_step() <= 0.2 + 1.0e-6,
        "a stroke pushes at most 0.2 blocks a tick, not {}",
        swim.fastest_horizontal_step()
    );
    let steps: Vec<f64> = swim
        .positions
        .windows(2)
        .map(|w| crate::world::distance_sq(w[0], w[1]).sqrt())
        .collect();
    assert!(
        steps.windows(2).any(|w| w[1] > w[0] * 1.5 + 0.01),
        "each stroke kicks it forward again"
    );
    assert!(swim.stays_in_pool(), "{:?}", swim.positions);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn axolotls_swim_their_pool() {
    let swim = swim(EntityType::Axolotl, 300).await;
    assert!(swim.farthest_from_start() > 1.5);
    assert!(
        swim.fastest_horizontal_step() < 0.25,
        "{}",
        swim.fastest_horizontal_step()
    );
    assert!(swim.stays_in_pool(), "{:?}", swim.positions);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stranded_fish_flop_about() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let floor = f64::from(SKY) + 1.0;
    summon(&region, EntityType::Cod, Compound::new(), [7.5, floor, 7.5]).await;
    wait_for(&world, &region, 60, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Cod && s.position[1] > floor + 0.5)
    })
    .await;
    world.shutdown().await;
}
