//! A far teleport reaches the client only after the chunk under the
//! destination, so the client never sinks through a floor it has not
//! loaded yet.

use super::*;
use crate::world::session::PlayerSession;
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_vanilla_data::packet::play::Clientbound as C;

#[derive(Debug, PartialEq)]
enum Out {
    Position(i32),
    Chunk(ChunkPos),
}

fn drain(frames: &mut mpsc::UnboundedReceiver<bytes::Bytes>) -> Vec<Out> {
    let mut out = Vec::new();
    while let Ok(frame) = frames.try_recv() {
        let id = framed_packet_id(&frame);
        let mut payload = framed_payload(&frame);
        payload.read_var_int().unwrap();
        if id == C::PlayerPosition.id() {
            out.push(Out::Position(payload.read_var_int().unwrap()));
        } else if id == C::LevelChunkWithLight.id() {
            let x = payload.read_int().unwrap();
            let z = payload.read_int().unwrap();
            out.push(Out::Chunk(ChunkPos::new(x, z)));
        }
    }
    out
}

async fn step(world: &TestWorld, session: &mut PlayerSession) {
    session.tick += 1;
    session.send_chunk_batch();
    session.refill_view().await;
    world.ticks(1).await;
    while let Ok(event) = session.conn.events.try_recv() {
        session.on_event(event).await;
    }
}

async fn settled(
    world: &TestWorld,
    id: i32,
) -> (PlayerSession, mpsc::UnboundedReceiver<bytes::Bytes>) {
    world.shared.live.set_view_distance(4);
    let (mut session, mut frames) = session_with_outbound(world, id);
    session.info.view_distance = 4;
    session.stand_at([8.5, 100.0, 8.5], false);
    session.update_view().await;
    for _ in 0..200 {
        step(world, &mut session).await;
        session.view.unacked_batches = 0;
        let wanted = session.chunk().view(4);
        if wanted.iter().all(|c| session.view.sent.contains(c)) {
            break;
        }
    }
    drain(&mut frames);
    (session, frames)
}

const FAR: [f64; 3] = [98.5, 100.0, 8.5];

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_far_teleport_is_sent_after_the_destination_chunk_even_with_a_slow_client() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut frames) = settled(&world, 2001).await;
    session.view.unacked_batches = session.view.max_unacked_batches;
    session.teleport_after_chunk(FAR);
    session.update_view().await;
    let mut seen = drain(&mut frames);
    for _ in 0..30 {
        step(&world, &mut session).await;
        seen.extend(drain(&mut frames));
        if seen.iter().any(|o| matches!(o, Out::Position(_))) {
            break;
        }
    }
    let destination = ChunkPos::of_position(FAR);
    let chunk_at = seen.iter().position(|o| *o == Out::Chunk(destination));
    let position_at = seen.iter().position(|o| matches!(o, Out::Position(_)));
    assert!(chunk_at.is_some(), "destination chunk never sent: {seen:?}");
    assert!(
        chunk_at < position_at,
        "teleport before its chunk: {seen:?}"
    );
    assert!(session.view.sent.contains(&destination));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_teleport_inside_the_sent_view_is_immediate() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut frames) = settled(&world, 2002).await;
    session.teleport_after_chunk([20.5, 100.0, 8.5]);
    assert!(matches!(drain(&mut frames)[..], [Out::Position(_)]));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_teleport_whose_chunk_never_arrives_is_released_at_the_deadline() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut frames) = settled(&world, 2003).await;
    session.teleport_after_chunk(FAR);
    assert!(drain(&mut frames).is_empty());
    for _ in 0..crate::world::session::TELEPORT_CHUNK_WAIT_TICKS - 1 {
        session.tick += 1;
        session.send_chunk_batch();
        assert!(drain(&mut frames).is_empty());
    }
    session.tick += 1;
    session.send_chunk_batch();
    assert!(matches!(drain(&mut frames)[..], [Out::Position(_)]));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_teleport_replaces_a_deferred_one() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut frames) = settled(&world, 2004).await;
    session.teleport_after_chunk(FAR);
    session.teleport_after_chunk([8.5, 100.0, 98.5]);
    assert!(drain(&mut frames).is_empty());
    session.tick += crate::world::session::TELEPORT_CHUNK_WAIT_TICKS;
    session.send_chunk_batch();
    let sent = drain(&mut frames);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(session.sync.teleport_target, [8.5, 100.0, 98.5]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_resend_clock_starts_when_the_deferred_teleport_is_sent() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut frames) = settled(&world, 2005).await;
    session.teleport_after_chunk(FAR);
    session.tick += 30;
    session.sync.load_deadline = None;
    let before = session.sync.teleport_sent;
    session
        .handle_packet(Serverbound::Move(Movement {
            position: None,
            rotation: Some((0.0, 0.0)),
            on_ground: false,
            horizontal_collision: false,
        }))
        .await;
    assert!(drain(&mut frames).is_empty());
    assert_eq!(session.sync.teleport_sent, before);
    world.shutdown().await;
}
