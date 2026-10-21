//! Movement setbacks land on the last safe footing, never in mid-air.

use super::anticheat::{Walker, walker};
use super::*;

const JUMP: f64 = 0.42;

async fn jump_off(w: &mut Walker) {
    w.step(0.0, w.floor_y, 0.0).await;
    w.step(0.0, w.floor_y, 0.0).await;
    w.step(0.2, w.floor_y + JUMP, 0.0).await;
    w.step(0.2, w.floor_y + 0.75, 0.0).await;
    assert!(
        w.session.sync.pending_teleport.is_none(),
        "a plain jump passes"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mid_air_setback_returns_to_the_last_ground() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(52, 52)).await;
    let ground = [w.x, w.floor_y, w.z];
    jump_off(&mut w).await;
    w.step(4.0, w.floor_y + 0.9, 0.0).await;
    assert!(
        w.session.sync.pending_teleport.is_some(),
        "the speed is set back"
    );
    assert_eq!(w.session.pose.position, ground);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moves_before_the_setback_is_confirmed_change_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1, ChunkPos::new(53, 53)).await;
    let ground = [w.x, w.floor_y, w.z];
    jump_off(&mut w).await;
    w.step(4.0, w.floor_y + 0.9, 0.0).await;
    for n in 0..10 {
        w.step(0.3, w.floor_y + 0.9 + f64::from(n) * 0.3, 0.0).await;
        assert_eq!(
            w.session.pose.position, ground,
            "move {n} before the confirm"
        );
    }
    w.confirm().await;
    assert!(w.session.sync.pending_teleport.is_none());
    jump_off(&mut w).await;
    w.step(4.0, w.floor_y + 0.9, 0.0).await;
    assert_eq!(
        w.session.pose.position, ground,
        "still anchored on the ground"
    );
    world.shutdown().await;
}

async fn launched_then_set_back(damage: bool) -> f64 {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    if !damage {
        world
            .shared
            .config
            .set_text("tuff.anticheat.setbackFallDamage", "false")
            .unwrap();
    }
    let mut w = walker(&world, 1, ChunkPos::new(54, 54)).await;
    let ground = [w.x, w.floor_y, w.z];
    w.step(0.0, w.floor_y, 0.0).await;
    w.step(0.0, w.floor_y, 0.0).await;
    w.session.motion.pushed_at = Some(w.session.tick);
    for n in 1..=12 {
        w.step(0.0, w.floor_y + f64::from(n) * 0.5, 0.0).await;
    }
    assert!(w.session.sync.pending_teleport.is_none(), "a push may lift");
    w.session.tick += 200;
    let fallen = w.session.body.fall.distance;
    let y = w.session.pose.position[1];
    w.step(0.0, y + 0.5, 0.0).await;
    assert!(w.session.sync.pending_teleport.is_some(), "rising unpushed");
    assert_eq!(w.session.pose.position, ground);
    let gained = w.session.body.fall.distance - fallen;
    world.shutdown().await;
    gained
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_impossible_height_is_set_back_down_and_the_drop_counts_as_a_fall() {
    assert!((launched_then_set_back(true).await - 6.0).abs() < 1e-9);
    assert_eq!(launched_then_set_back(false).await, 0.0);
}
