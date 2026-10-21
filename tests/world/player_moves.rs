//! Players publish relative moves to the viewers that saw their previous
//! report, and full state otherwise.

use super::*;
use tuff_vanilla_data::packet::play::Clientbound as C;

async fn tracks(
    world: &TestWorld,
    watcher: &mut crate::world::session::PlayerSession,
) -> Vec<crate::world::messages::Track> {
    world.ticks(2).await;
    let mut out = Vec::new();
    while let Ok(event) = watcher.conn.events.try_recv() {
        if let PlayerEvent::Track(track) = &event {
            out.push(track.clone());
        }
        watcher.on_event(event).await;
    }
    out
}

fn ids(packets: &[bytes::Bytes]) -> Vec<i32> {
    packets
        .iter()
        .map(|p| framed_payload(p).read_var_int().expect("id"))
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walking_publishes_deltas_and_turning_the_head() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (mut mover, frames) = session_with_outbound(&world, 100);
    std::mem::forget(frames);
    let (mut watcher, frames) = session_with_outbound(&world, 101);
    std::mem::forget(frames);
    watcher.view.subscribed.insert(chunk);
    region
        .request(RegionRequest::Subscribe {
            player: watcher.conn.handle.clone(),
            chunks: vec![chunk],
        })
        .await;
    mover.stand_at([4.5, 70.0, 4.5], true);
    mover.publish(true).await;
    tracks(&world, &mut watcher).await;

    mover.stand_at([4.75, 70.0, 4.5], true);
    mover.publish(false).await;
    let seen = tracks(&world, &mut watcher).await;
    let moves = seen
        .iter()
        .rev()
        .find_map(|t| t.moves.clone())
        .expect("a relative move");
    assert_eq!(ids(&moves), [C::MoveEntityPos.id()]);

    mover.pose.yaw += 90.0;
    mover.publish(false).await;
    let seen = tracks(&world, &mut watcher).await;
    let moves = seen
        .iter()
        .rev()
        .find_map(|t| t.moves.clone())
        .expect("a relative turn");
    assert_eq!(ids(&moves), [C::MoveEntityRot.id(), C::RotateHead.id()]);

    mover.publish(true).await;
    let seen = tracks(&world, &mut watcher).await;
    assert!(seen.iter().all(|t| t.moves.is_none()), "forced is absolute");
}
