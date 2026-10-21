//! Wire in a world with the redstone experiments feature: the experimental
//! evaluator powers and depowers a line like the default one, and only it
//! reports each changed wire's orientation to debug viewers.

use super::*;
use crate::world::debug_feeds::{DebugSubscribers, bit};
use tuff_vanilla_data::packet::play::Clientbound as C;
use tuff_vanilla_data::registry::DebugSubscription as Feed;

const FEATURE_PACK: &str = "redstone_experiments";

/// The positions of wire orientation values sent to the bench's viewer.
fn orientation_values(b: &mut Bench) -> Vec<(BlockPos, i32)> {
    use tuff_core::protocol::codec::ReadExt;
    let mut seen = Vec::new();
    while let Ok(event) = b.events.try_recv() {
        let PlayerEvent::ChunkPackets { packets, .. } = event else {
            continue;
        };
        for frame in packets.iter() {
            let mut r = framed_payload(frame);
            if r.read_var_int().ok() != Some(C::DebugBlockValue.id()) {
                continue;
            }
            let (Ok(pos), Ok(feed), Ok(true)) =
                (r.read_position(), r.read_var_int(), r.read_bool())
            else {
                continue;
            };
            if feed == Feed::RedstoneWireOrientations as i32 {
                seen.push((pos, r.read_var_int().unwrap()));
            }
        }
    }
    seen
}

async fn powers(b: &Bench, wires: &[BlockPos]) -> Vec<u8> {
    let mut out = Vec::new();
    for pos in wires {
        out.push(state(&b.region, *pos).await.power().unwrap());
    }
    out
}

/// Powers a line of wire from a redstone block and takes the block away,
/// returning the powers with and without it and the orientations reported.
async fn run_line(experimental: bool) -> (Vec<u8>, Vec<u8>, Vec<(BlockPos, i32)>) {
    let dir = TestDir::new();
    let world = if experimental {
        TestWorld::start_with_packs(dir.path(), &[FEATURE_PACK])
    } else {
        TestWorld::start(dir.path())
    };
    let mut b = bench_in(world, &[ChunkPos::new(5, 5)], (80, 95), (84, 88)).await;
    b.world
        .debug_subscribers
        .send_replace(Arc::new(DebugSubscribers::new(vec![(
            b.player.id,
            bit(Feed::RedstoneWireOrientations),
        )])));
    let wires: Vec<BlockPos> = (82..=95).map(|x| at(x, 1, 86)).collect();
    let line: Vec<(BlockPos, &str)> = wires
        .iter()
        .map(|pos| (*pos, "minecraft:redstone_wire"))
        .collect();
    set(&b.region, &line).await;
    b.world.ticks(2).await;
    orientation_values(&mut b);

    set(&b.region, &[(at(81, 1, 86), "minecraft:redstone_block")]).await;
    b.world.ticks(2).await;
    let lit = powers(&b, &wires).await;
    let mut reported = orientation_values(&mut b);

    set(&b.region, &[(at(81, 1, 86), "minecraft:air")]).await;
    b.world.ticks(2).await;
    let dark = powers(&b, &wires).await;
    reported.extend(orientation_values(&mut b));
    b.world.shutdown().await;
    (lit, dark, reported)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_experimental_evaluator_powers_and_clears_a_line_and_reports_orientations() {
    let (lit, dark, reported) = run_line(true).await;
    assert_eq!(lit, (2..=15).rev().collect::<Vec<u8>>());
    assert_eq!(dark, vec![0; 14]);
    let line: HashSet<BlockPos> = (82..=95).map(|x| at(x, 1, 86)).collect();
    let reported_at: HashSet<BlockPos> = reported.iter().map(|(pos, _)| *pos).collect();
    assert_eq!(reported_at, line, "{reported:?}");
    assert!(reported.iter().all(|(_, id)| (0..48).contains(id)));
    let orientation = |id: i32| tuff_core::world::redstone::Orientation::from_index(id as u8);
    assert!(
        reported.iter().all(|(_, id)| orientation(*id)
            .is_some_and(|o| o.up() == tuff_vanilla_data::block::Facing::Up)),
        "{reported:?}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_evaluator_powers_the_same_and_reports_nothing() {
    let (lit, dark, reported) = run_line(false).await;
    assert_eq!(lit, (2..=15).rev().collect::<Vec<u8>>());
    assert_eq!(dark, vec![0; 14]);
    assert!(reported.is_empty(), "{reported:?}");
}
