//! A player's undelivered chunk requests stay inside the per-player
//! budget while they teleport or fly across ungenerated terrain, and
//! every chunk in view still arrives.

use super::*;
use crate::world::session::PlayerSession;

fn pending(session: &PlayerSession) -> usize {
    session
        .view
        .subscribed
        .iter()
        .filter(|c| !session.view.sent.contains(c))
        .count()
}

fn complete(session: &PlayerSession) -> bool {
    let wanted = session.chunk().view(session.view.view_distance);
    wanted.iter().all(|c| session.view.sent.contains(c))
}

async fn step(world: &TestWorld, session: &mut PlayerSession) {
    session.send_chunk_batch();
    session.refill_view().await;
    world.ticks(1).await;
    while let Ok(event) = session.conn.events.try_recv() {
        session.on_event(event).await;
    }
    session.view.unacked_batches = 0;
}

struct Run {
    ticks: u32,
    peak_pending: usize,
}

async fn fly(world: &TestWorld, session: &mut PlayerSession, legs: &[[f64; 3]]) -> Run {
    let mut run = Run {
        ticks: 0,
        peak_pending: 0,
    };
    for leg in legs {
        session.stand_at(*leg, false);
        session.update_view().await;
        for _ in 0..4 {
            step(world, session).await;
            run.ticks += 1;
            run.peak_pending = run.peak_pending.max(pending(session));
        }
    }
    while !complete(session) {
        step(world, session).await;
        run.ticks += 1;
        run.peak_pending = run.peak_pending.max(pending(session));
        assert!(run.ticks < 4000, "the view never completed");
    }
    run
}

const LEGS: [[f64; 3]; 4] = [
    [8.5, 100.0, 8.5],
    [4000.5, 100.0, 8.5],
    [4400.5, 100.0, 408.5],
    [8.5, 100.0, 8.5],
];

async fn run_with(budget: u32) -> Run {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.shared.live.set_view_distance(10);
    world.shared.live.set_chunk_budget(budget);
    let (mut session, _frames) = session_with_outbound(&world, 1000);
    session.info.view_distance = 10;
    session.view.max_unacked_batches = u32::MAX;
    let run = fly(&world, &mut session, &LEGS).await;
    world.shutdown().await;
    run
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_budget_bounds_the_window_while_the_whole_view_still_arrives() {
    let run = run_with(32).await;
    assert!(run.peak_pending <= 32, "peak pending {}", run.peak_pending);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_budget_a_b_against_unlimited() {
    let off = run_with(0).await;
    let on = run_with(64).await;
    eprintln!(
        "chunk_budget A/B: off ticks={} peak_pending={}; on ticks={} peak_pending={}",
        off.ticks, off.peak_pending, on.ticks, on.peak_pending
    );
    assert!(on.peak_pending <= 64);
    assert!(off.peak_pending > on.peak_pending);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_budget_does_not_starve_a_second_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.shared.live.set_view_distance(8);
    world.shared.live.set_chunk_budget(24);
    let (mut far, _far_frames) = session_with_outbound(&world, 1001);
    let (mut near, _near_frames) = session_with_outbound(&world, 1002);
    for session in [&mut far, &mut near] {
        session.info.view_distance = 8;
        session.view.max_unacked_batches = u32::MAX;
    }
    far.stand_at([6000.5, 100.0, 6000.5], false);
    near.stand_at([8.5, 100.0, 8.5], false);
    far.update_view().await;
    near.update_view().await;
    let mut ticks = 0;
    while !complete(&near) || !complete(&far) {
        step(&world, &mut far).await;
        step(&world, &mut near).await;
        assert!(pending(&far) <= 24 && pending(&near) <= 24);
        ticks += 1;
        assert!(ticks < 4000, "a view never completed");
    }
    world.shutdown().await;
}
