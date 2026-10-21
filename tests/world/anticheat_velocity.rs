//! Server-given velocity through the ledger: an applied push is allowed,
//! an ignored one counts against the player.

use super::anticheat::{Walker, walker};
use super::*;

async fn floored(world: &TestWorld, chunk: ChunkPos) -> Walker {
    let mut w = walker(world, 1, chunk).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let floor: Vec<_> = (-2..24)
        .map(|dx| {
            (
                BlockPos::new(w.x as i32 + dx, 100, w.z as i32),
                tuff_vanilla_data::Block::Stone.default_state(),
            )
        })
        .collect();
    place_blocks(world, &region, floor).await;
    world.ticks(2).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    w
}

async fn knocked(w: &mut Walker, follow: bool) {
    tuff_feature_builtin_vanilla_anticheat::clock::advance(&w.session, Duration::from_millis(1500));
    w.session
        .on_event(PlayerEvent::Pushed([0.5, 0.4, 0.0]))
        .await;
    let (mut vx, mut vy) = (0.5, 0.4);
    let mut y = w.floor_y;
    for _ in 0..if follow { 30 } else { 12 } {
        w.session.tick += 1;
        if follow {
            y = (y + vy).max(w.floor_y);
            w.step(vx, y, 0.0).await;
            vx *= 0.91;
            vy = (vy - 0.08) * 0.98;
        } else {
            w.step(0.0, w.floor_y, 0.0).await;
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn following_knockback_is_never_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = floored(&world, ChunkPos::new(56, 56)).await;
    w.session.tick += 100;
    for _ in 0..3 {
        knocked(&mut w, true).await;
        assert!(w.session.sync.pending_teleport.is_none());
    }
    assert!(!w.session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ignoring_knockback_again_and_again_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.check.velocity.kickAbove", "2")
        .unwrap();
    let mut w = floored(&world, ChunkPos::new(57, 57)).await;
    w.session.tick += 100;
    for _ in 0..8 {
        knocked(&mut w, false).await;
        if w.session.disconnected {
            break;
        }
    }
    assert!(w.session.disconnected);
    let alerts = super::anticheat::operator_alerts(&world).await;
    let last = alerts.last().unwrap();
    assert!(last.contains("kicked by the velocity check"), "{last}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unused_push_cannot_be_spent_on_speed_later() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = floored(&world, ChunkPos::new(58, 58)).await;
    w.session.tick += 100;
    w.session
        .on_event(PlayerEvent::Pushed([3.0, 0.0, 0.0]))
        .await;
    for _ in 0..40 {
        w.session.tick += 1;
        w.step(0.0, w.floor_y, 0.0).await;
    }
    w.step(3.0, w.floor_y, 0.0).await;
    assert!(w.session.sync.pending_teleport.is_some());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cart_nudge_is_recorded_without_a_motion_packet() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = floored(&world, ChunkPos::new(59, 59)).await;
    let before = w.session.motion.velocity_sent.total;
    w.session
        .on_event(PlayerEvent::Nudged([0.02, 0.0, 0.0]))
        .await;
    assert_eq!(w.session.motion.velocity_sent.total, before + 1);
    let given = w.session.motion.velocity_sent.since(before).next().unwrap();
    assert!(given.added);
    world.shutdown().await;
}
