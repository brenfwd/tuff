use std::time::{Duration, Instant};

use super::*;
use crate::chunk_generator as generator;
use tuff_core::storage::level::{DimensionSettings, OVERWORLD};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::generator::VIEWED;

/// Finished vanilla overworld chunks per second through the whole
/// pipeline (terrain of the surrounding ring, decoration, finishing).
/// Run with `cargo test --release -- --ignored --nocapture throughput`.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn vanilla_pipeline_throughput() {
    let settings = DimensionSettings::vanilla()
        .into_iter()
        .find(|d| d.key == OVERWORLD)
        .unwrap();
    let generator = Arc::new(generator::vanilla(&settings, 1).unwrap());
    let shape = ChunkShape::OVERWORLD;
    generator.terrain(ChunkPos::new(0, 0), shape, &[]);
    burst(generator, shape).await;
}

/// The same burst against the sleeping generator of
/// [`flyers_outrunning_simulated_generation`].
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn simulated_pipeline_burst() {
    burst(Arc::new(Sleeper::from_env()), ChunkShape::new(0, 16)).await;
}

/// Wants a square of `MC_PIPELINE_SIDE` (24) chunks at once, nearest
/// to its middle first as a player joining would, and prints how long
/// the chunks within four of the middle and all of them took.
async fn burst(generator: Arc<dyn StagedGenerator>, shape: ChunkShape) {
    let (_clock_tx, clock) = watch::channel(0);
    let shards = std::env::var("MC_PIPELINE_SHARDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(default_shards);
    let pipeline = GenerationActor::spawn_with(
        generator,
        PipelineContext {
            shape,
            clock,
            storage: None,
            tags: Default::default(),
        },
        shards,
        limits_from_env(),
    );
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let side = env("MC_PIPELINE_SIDE", 24.0) as i32;
    let middle = ChunkPos::new(side / 2, side / 2);
    let mut order: Vec<ChunkPos> = (0..side)
        .flat_map(|z| (0..side).map(move |x| ChunkPos::new(x, z)))
        .collect();
    order.sort_by_key(|pos| pos.distance(middle));
    let mut near = order.iter().filter(|pos| pos.distance(middle) <= 4).count();
    let started = Instant::now();
    for pos in &order {
        pipeline.want(*pos, region.clone(), VIEWED);
    }
    let mut received = 0;
    let mut near_done = 0.0;
    while received < order.len() {
        if let Ok(Some(RegionInternal::Generated { pos, .. })) =
            tokio::time::timeout(Duration::from_secs(600), inbox.recv()).await
        {
            received += 1;
            if pos.distance(middle) <= 4 {
                near -= 1;
                if near == 0 {
                    near_done = started.elapsed().as_secs_f64();
                }
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "{received} chunks in {elapsed:.2}s: {:.1} chunks/s on {} threads, {shards} shards; \
         the middle 9x9 in {near_done:.2}s",
        received as f64 / elapsed,
        rayon::current_num_threads()
    );
}

/// Flyers outrunning generation, without regions or storage: each
/// wants the chunks in its view as they come into it and withdraws
/// those it leaves undelivered. Prints chunks delivered while still in
/// view per five seconds, so overload collapse shows. Env:
/// `MC_FLYERS` (16), `MC_FLYER_SPEED` chunks/s (2.5), `MC_FLYER_SECONDS`
/// (40), `MC_FLYER_PRIORITY=distance` ranks by distance to the flyer's
/// position `MC_FLYER_LEAD` seconds ahead (0).
/// Run with `cargo test --release -p vanilla-worldgen --lib -- --ignored
/// --nocapture flyers_outrunning`.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn flyers_outrunning_generation() {
    let settings = DimensionSettings::vanilla()
        .into_iter()
        .find(|d| d.key == OVERWORLD)
        .unwrap();
    let generator = Arc::new(generator::vanilla(&settings, 1).unwrap());
    let shape = ChunkShape::OVERWORLD;
    generator.terrain(ChunkPos::new(0, 0), shape, &[]);
    fly(Counting::new(generator), shape, 16.0).await;
}

/// The same flight against a generator whose steps only sleep
/// (`MC_SIM_TERRAIN_MS` 30, `MC_SIM_DECORATE_MS` 5, `MC_SIM_FINISH_MS`
/// 1), so the pipeline's scheduling is measured apart from the
/// machine's load: sleeping workers leave the CPU to everything else.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn flyers_outrunning_simulated_generation() {
    let sleeper = Arc::new(Sleeper::from_env());
    fly(Counting::new(sleeper), ChunkShape::new(0, 16), 32.0).await;
}

/// `MC_PIPELINE_DECORATIONS` and `MC_PIPELINE_COMMITTED` per shard.
fn limits_from_env() -> Limits {
    let read = |name: &str| std::env::var(name).ok().and_then(|n| n.parse().ok());
    Limits {
        decorating: read("MC_PIPELINE_DECORATIONS"),
        committed: read("MC_PIPELINE_COMMITTED"),
    }
}

fn env(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Sleeper {
    terrain: Duration,
    decorate: Duration,
    finish: Duration,
}

impl Sleeper {
    fn from_env() -> Self {
        let millis = |name: &str, default: f64| Duration::from_secs_f64(env(name, default) / 1e3);
        Self {
            terrain: millis("MC_SIM_TERRAIN_MS", 30.0),
            decorate: millis("MC_SIM_DECORATE_MS", 5.0),
            finish: millis("MC_SIM_FINISH_MS", 1.0),
        }
    }
}

impl StagedGenerator for Sleeper {
    fn terrain(&self, pos: ChunkPos, shape: ChunkShape, _: &[Arc<StartsBlock>]) -> ProtoChunk {
        std::thread::sleep(self.terrain);
        let mut proto = ProtoChunk::new(pos, shape);
        proto.finish_terrain();
        proto
    }

    fn decorate(&self, _: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        std::thread::sleep(self.decorate);
        window
    }

    fn finish(&self, proto: ProtoChunk) -> Finished {
        std::thread::sleep(self.finish);
        Finished::new(proto.into_chunk())
    }
}

/// Counts the steps a generator runs.
struct Counting {
    inner: Arc<dyn StagedGenerator>,
    terrain: std::sync::atomic::AtomicU64,
    decorate: std::sync::atomic::AtomicU64,
}

impl Counting {
    fn new(inner: Arc<dyn StagedGenerator>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            terrain: Default::default(),
            decorate: Default::default(),
        })
    }

    fn take(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.terrain.swap(0, Relaxed),
            self.decorate.swap(0, Relaxed),
        )
    }
}

impl StagedGenerator for Counting {
    fn starts(&self, block: (i32, i32), shape: ChunkShape) -> StartsBlock {
        self.inner.starts(block, shape)
    }

    fn terrain(&self, pos: ChunkPos, shape: ChunkShape, starts: &[Arc<StartsBlock>]) -> ProtoChunk {
        self.terrain
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.terrain(pos, shape, starts)
    }

    fn decorate(&self, center: ChunkPos, window: Vec<ProtoChunk>) -> Vec<ProtoChunk> {
        self.decorate
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.decorate(center, window)
    }

    fn finish(&self, proto: ProtoChunk) -> Finished {
        self.inner.finish(proto)
    }
}

async fn fly(generator: Arc<Counting>, shape: ChunkShape, default_flyers: f64) {
    let flyers = env("MC_FLYERS", default_flyers) as usize;
    let speed = env("MC_FLYER_SPEED", 2.5);
    let seconds = env("MC_FLYER_SECONDS", 40.0);
    let by_distance = std::env::var("MC_FLYER_PRIORITY").is_ok_and(|v| v == "distance");
    let radius = 10;
    let (_clock_tx, clock) = watch::channel(0);
    let pipeline = GenerationActor::spawn_with(
        generator.clone(),
        PipelineContext {
            shape,
            clock,
            storage: None,
            tags: Default::default(),
        },
        default_shards(),
        limits_from_env(),
    );
    let (region, _requests, mut inbox) =
        RegionHandle::new(DimensionId::default(), RegionPos::new(0, 0), 8);
    let started = Instant::now();
    let mut wanted: HashMap<ChunkPos, u32> = HashMap::new();
    let mut done: HashSet<ChunkPos> = HashSet::new();
    let slots = (seconds / 5.0).ceil() as usize;
    let mut buckets = vec![(0u32, 0u32); slots];
    let mut work = vec![(0u64, 0u64); slots];
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    while started.elapsed().as_secs_f64() < seconds {
        interval.tick().await;
        let t = started.elapsed().as_secs_f64();
        let (terrain, decorate) = generator.take();
        let slot = &mut work[((t / 5.0) as usize).min(slots - 1)];
        slot.0 += terrain;
        slot.1 += decorate;
        let centers: Vec<ChunkPos> = (0..flyers)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / flyers as f64;
                let d = 4.0 + speed * t;
                ChunkPos::new(
                    (d * angle.cos()).floor() as i32,
                    (d * angle.sin()).floor() as i32,
                )
            })
            .collect();
        let lead = env("MC_FLYER_LEAD", 0.0);
        let ahead: Vec<ChunkPos> = (0..flyers)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / flyers as f64;
                let d = 4.0 + speed * (t + lead);
                ChunkPos::new(
                    (d * angle.cos()).floor() as i32,
                    (d * angle.sin()).floor() as i32,
                )
            })
            .collect();
        let mut view: HashMap<ChunkPos, u32> = HashMap::new();
        for (center, ahead) in centers.iter().zip(&ahead) {
            for pos in within(*center, radius) {
                let priority = if by_distance {
                    1 + pos.distance(*ahead) as u32
                } else {
                    1
                };
                let best = view.entry(pos).or_insert(priority);
                *best = (*best).min(priority);
            }
        }
        for (pos, priority) in &view {
            if !done.contains(pos) && wanted.get(pos) != Some(priority) {
                pipeline.want(*pos, region.clone(), *priority);
                wanted.insert(*pos, *priority);
            }
        }
        wanted.retain(|pos, _| {
            let keep = view.contains_key(pos);
            if !keep {
                pipeline.unwant(*pos, region.pos);
            }
            keep
        });
        while let Ok(message) = inbox.try_recv() {
            if let RegionInternal::Generated { pos, .. } = message {
                let last = buckets.len() - 1;
                let bucket = &mut buckets[((t / 5.0) as usize).min(last)];
                done.insert(pos);
                if wanted.remove(&pos).is_some() {
                    bucket.0 += 1;
                } else {
                    bucket.1 += 1;
                }
            }
        }
    }
    let mut total = 0;
    for (i, ((useful, late), (terrain, decorate))) in buckets.iter().zip(&work).enumerate() {
        total += useful;
        eprintln!(
            "{:>3}s: {useful} in view, {late} late, {terrain} terrain, {decorate} decorations",
            i * 5
        );
    }
    eprintln!("total in view: {total}");
}
