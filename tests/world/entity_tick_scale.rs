use super::*;
use tuff_nbt::Compound;

async fn summon(region: &RegionHandle, nbt: Compound, position: [f64; 3]) -> bool {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position,
            fresh: false,
            reply,
        })
        .await;
    summoned.await.unwrap()
}

async fn ms_per_tick(count: usize) -> f64 {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let row = (count as f64).sqrt().ceil() as usize;
    let side = ((row as f64 * 1.2 + 4.0) / 16.0).ceil() as i32;
    let chunks: Vec<ChunkPos> = (0..side * side)
        .map(|i| ChunkPos::new(i % side, i / side))
        .collect();
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(900);
    for chunk in &chunks {
        subscribe(&region, &viewer, &mut events, &[*chunk]).await;
    }
    let height = Generator::new(1).height(40, 40);
    for i in 0..count {
        let (x, z) = ((i % row) as f64 * 1.2 + 2.0, (i / row) as f64 * 1.2 + 2.0);
        let pig = Compound::new().with("id", "minecraft:pig");
        assert!(summon(&region, pig, [x, f64::from(height + 1), z]).await);
    }
    world.ticks(20).await;
    while events.try_recv().is_ok() {}
    let ticks = 40;
    let started = std::time::Instant::now();
    for _ in 0..ticks {
        world.ticks(1).await;
        while events.try_recv().is_ok() {}
    }
    let ms = started.elapsed().as_secs_f64() * 1000.0 / f64::from(ticks);
    world.shutdown().await;
    ms
}

/// Region tick cost with many entities; run with
/// `cargo test --release --test world entity_tick_scale -- --ignored --nocapture`.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing measurement, run by hand"]
async fn entity_tick_scale() {
    for count in [1000, 2000, 4000, 8000] {
        println!(
            "entities={count} ms_per_tick={:.2}",
            ms_per_tick(count).await
        );
    }
}
