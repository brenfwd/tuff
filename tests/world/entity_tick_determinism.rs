use super::*;
use std::sync::atomic::Ordering;
use tuff_nbt::Compound;

const KINDS: [&str; 8] = [
    "minecraft:zombie",
    "minecraft:pig",
    "minecraft:sheep",
    "minecraft:chicken",
    "minecraft:cow",
    "minecraft:skeleton",
    "minecraft:wolf",
    "minecraft:rabbit",
];

/// Runs a seeded crowd of mobs for `ticks` ticks and returns what a viewer
/// was sent and every entity's saved state, both rendered for comparison.
async fn run(shuffle: u64, min_batch: usize, ticks: u64) -> (Vec<String>, String) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .entity_compute_shuffle
        .store(shuffle, Ordering::Relaxed);
    world
        .shared
        .entity_compute_min_batch
        .store(min_batch, Ordering::Relaxed);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(900);
    let chunks: Vec<ChunkPos> = (0..9).map(|i| ChunkPos::new(i % 3, i / 3)).collect();
    for chunk in &chunks {
        subscribe(&region, &viewer, &mut events, &[*chunk]).await;
    }
    let height = Generator::new(1).height(20, 20);
    for i in 0..48 {
        let (x, z) = (f64::from(i % 8) * 1.7 + 12.0, f64::from(i / 8) * 1.7 + 12.0);
        let nbt = Compound::new()
            .with("id", KINDS[i as usize % KINDS.len()])
            .with("UUID", tuff_nbt::Tag::IntArray(vec![0x7e57, 0, i, 1]));
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt,
                position: [x, f64::from(height + 1), z],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    while events.try_recv().is_ok() {}
    let mut stream = Vec::new();
    for _ in 0..ticks {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            stream.push(format!("{event:?}"));
        }
    }
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntitiesWithData { reply })
        .await;
    let mut entities = answer.await.unwrap();
    entities.sort_by_key(|e| e.id);
    let state = format!("{entities:?}");
    world.shutdown().await;
    (stream, state)
}

fn first_difference(a: &str, b: &str) -> String {
    let at = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let from = a[..at].rfind("EntitySnapshot").unwrap_or(0);
    let end = |s: &str| (at + 200).min(s.len());
    format!("{}\n---\n{}", &a[from..end(a)], &b[from..end(b)])
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entity_ticks_do_not_depend_on_compute_order() {
    let ticks = 100;
    let (serial_stream, serial_state) = run(0, usize::MAX, ticks).await;
    assert!(!serial_stream.is_empty());
    for shuffle in [0, 0x5eed, 0xdead_beef] {
        let (stream, state) = run(shuffle, usize::MAX, ticks).await;
        assert!(
            state == serial_state,
            "entity state differs for {shuffle:#x}: {}",
            first_difference(&state, &serial_state)
        );
        assert_eq!(stream.len(), serial_stream.len());
        for (i, (a, b)) in stream.iter().zip(&serial_stream).enumerate() {
            assert_eq!(a, b, "packet {i} differs for {shuffle:#x}");
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entity_ticks_on_many_threads_match_one_thread() {
    let ticks = 100;
    let (serial_stream, serial_state) = run(0, usize::MAX, ticks).await;
    for min_batch in [1, 3] {
        let (stream, state) = run(0, min_batch, ticks).await;
        assert!(
            state == serial_state,
            "entity state differs in batches of {min_batch}: {}",
            first_difference(&state, &serial_state)
        );
        assert_eq!(
            stream, serial_stream,
            "packets differ in batches of {min_batch}"
        );
    }
}
