//! The prediction engine end to end: a client walking by the game's own
//! physics is never set back, one a few percent too fast is; knockback
//! the client applies as sent passes, amplified knockback does not.

use super::*;
use tuff_core::protocol::play::{Movement, Serverbound};

use super::anticheat::{Walker, operator_alerts, physical_walker};

const FRICTION: f64 = (0.6f32 * 0.91f32) as f64;
/// One tick of walking forward: movement speed x 0.98 input damping.
#[expect(
    clippy::eq_op,
    reason = "the test compares a value with itself on purpose"
)]
const ACCEL: f64 =
    (0.1f32 * (0.216_000_02f32 / (0.6f32 * 0.6f32 * 0.6f32))) as f64 * 0.98f32 as f64;

async fn floor_ahead(world: &TestWorld, w: &mut Walker, length: i32) {
    let region = world.region(RegionPos::new(1, 1)).await;
    let floor: Vec<_> = (0..length)
        .map(|dz| {
            (
                BlockPos::new(w.x as i32, 100, w.z as i32 + dz),
                tuff_vanilla_data::Block::Stone.default_state(),
            )
        })
        .collect();
    place_blocks(world, &region, floor).await;
    world.ticks(2).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
}

/// Walks forward along +z (yaw 0) for `ticks`, the acceleration scaled
/// by `boost`, and returns how many moves were set back.
async fn walk(w: &mut Walker, ticks: usize, boost: f64) -> usize {
    w.session
        .handle_packet(Serverbound::PlayerInput(0x01))
        .await;
    let mut velocity = 0.0;
    let mut setbacks = 0;
    for _ in 0..ticks {
        velocity += ACCEL * boost;
        let [x, y, z] = w.session.pose.position;
        w.session
            .handle_packet(Serverbound::Move(Movement {
                position: Some([x, y, z + velocity]),
                rotation: Some((0.0, 0.0)),
                on_ground: true,
                horizontal_collision: false,
            }))
            .await;
        w.session.handle_packet(Serverbound::ClientTickEnd).await;
        velocity *= FRICTION;
        if w.session.sync.pending_teleport.is_some() {
            setbacks += 1;
            w.confirm().await;
            velocity = 0.0;
        }
    }
    setbacks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walking_by_the_rules_passes_and_a_few_percent_faster_is_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforceUncalibrated", "true")
        .unwrap();
    let mut w = physical_walker(&world, 1, ChunkPos::new(46, 46)).await;
    floor_ahead(&world, &mut w, 40).await;
    assert_eq!(walk(&mut w, 30, 1.0).await, 0, "legit walking");
    let alerts = operator_alerts(&world).await;
    assert!(alerts.is_empty(), "{alerts:?}");
    let setbacks = walk(&mut w, 20, 1.08).await;
    assert!(setbacks > 0, "speed");
    let alerts = operator_alerts(&world).await;
    assert!(alerts.iter().any(|a| a.contains("predict")), "{alerts:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_steady_few_percent_speed_cheat_climbs_the_ladder_to_a_kick() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforceUncalibrated", "true")
        .unwrap();
    let mut w = physical_walker(&world, 1, ChunkPos::new(46, 46)).await;
    floor_ahead(&world, &mut w, 40).await;
    w.session
        .handle_packet(Serverbound::PlayerInput(0x01))
        .await;
    let mut velocity = 0.0;
    let mut moves = 0;
    while !w.session.disconnected && moves < 600 {
        velocity += ACCEL * 1.05;
        let [x, y, z] = w.session.pose.position;
        w.session
            .handle_packet(Serverbound::Move(Movement {
                position: Some([x, y, z + velocity]),
                rotation: Some((0.0, 0.0)),
                on_ground: true,
                horizontal_collision: false,
            }))
            .await;
        w.session.handle_packet(Serverbound::ClientTickEnd).await;
        w.session.tick += 1;
        tuff_feature_builtin_vanilla_anticheat::clock::advance(
            &w.session,
            Duration::from_millis(50),
        );
        velocity *= FRICTION;
        if w.session.sync.pending_teleport.is_some() {
            w.confirm().await;
            velocity = 0.0;
        }
        moves += 1;
    }
    assert!(w.session.disconnected, "never kicked in {moves} moves");
    let alerts = operator_alerts(&world).await;
    assert!(
        alerts
            .iter()
            .any(|a| a.contains("kicked by the predict check")),
        "{alerts:?}"
    );
    world.shutdown().await;
}

async fn send(w: &mut Walker, position: [f64; 3], on_ground: bool) {
    w.session
        .handle_packet(Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some(position),
            rotation: Some((0.0, 0.0)),
            on_ground,
        }))
        .await;
    w.session.handle_packet(Serverbound::ClientTickEnd).await;
    w.session.tick += 1;
}

/// Stands still, takes a knockback and follows it with the game's
/// physics, its horizontal part scaled by `scale`; returns the setbacks.
async fn knocked_back(w: &mut Walker, scale: f64) -> usize {
    knocked_with(w, scale, 1.0).await
}

async fn knocked_with(w: &mut Walker, scale: f64, up: f64) -> usize {
    let air = f64::from(0.91f32);
    let ground = f64::from(0.6f32 * 0.91f32);
    let drag = f64::from(0.98f32);
    let mut setbacks = 0;
    w.session.handle_packet(Serverbound::PlayerInput(0)).await;
    tuff_feature_builtin_vanilla_anticheat::clock::advance(&w.session, Duration::from_millis(2000));
    let start = w.session.pose.position;
    for _ in 0..8 {
        send(w, start, true).await;
    }
    let given = [0.3, 0.36, -0.2];
    w.session.on_event(PlayerEvent::Pushed(given)).await;
    let sent = tuff_core::protocol::codec::lp_vec3_received(given);
    let mut v = [sent[0] * scale, sent[1] * up, sent[2] * scale];
    let mut position = start;
    let mut on_ground = true;
    for _ in 0..40 {
        if v[0] * v[0] + v[2] * v[2] < 9.0e-6 {
            break;
        }
        let friction = if on_ground { ground } else { air };
        let mut dy = v[1];
        on_ground = position[1] + dy <= w.floor_y;
        if on_ground {
            dy = w.floor_y - position[1];
        }
        position = [position[0] + v[0], position[1] + dy, position[2] + v[2]];
        v = [
            v[0] * friction,
            if on_ground { 0.0 } else { v[1] } - 0.08,
            v[2] * friction,
        ];
        v[1] *= drag;
        send(w, position, on_ground).await;
        if w.session.sync.pending_teleport.is_some() {
            setbacks += 1;
            w.confirm().await;
            break;
        }
    }
    setbacks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn knockback_followed_as_sent_passes_and_amplified_is_set_back() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set_text("tuff.anticheat.enforceUncalibrated", "true")
        .unwrap();
    let mut w = physical_walker(&world, 1, ChunkPos::new(48, 46)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let floor: Vec<_> = (-4..=4)
        .flat_map(|dx| (-4..=4).map(move |dz| (dx, dz)))
        .map(|(dx, dz)| {
            (
                BlockPos::new(w.x as i32 + dx, 100, w.z as i32 + dz),
                tuff_vanilla_data::Block::Stone.default_state(),
            )
        })
        .collect();
    place_blocks(&world, &region, floor).await;
    world.ticks(2).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    w.session.tick += 100;
    for _ in 0..3 {
        assert_eq!(knocked_back(&mut w, 1.0).await, 0, "as sent");
    }
    let alerts = operator_alerts(&world).await;
    assert!(alerts.is_empty(), "{alerts:?}");
    assert!(knocked_back(&mut w, 1.5).await > 0, "amplified");
    let alerts = operator_alerts(&world).await;
    assert!(alerts.iter().any(|a| a.contains("predict")), "{alerts:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reduced_knockback_is_flagged_as_velocity() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = physical_walker(&world, 1, ChunkPos::new(48, 46)).await;
    let region = world.region(RegionPos::new(1, 1)).await;
    let floor: Vec<_> = (-4..=4)
        .flat_map(|dx| (-4..=4).map(move |dz| (dx, dz)))
        .map(|(dx, dz)| {
            (
                BlockPos::new(w.x as i32 + dx, 100, w.z as i32 + dz),
                tuff_vanilla_data::Block::Stone.default_state(),
            )
        })
        .collect();
    place_blocks(&world, &region, floor).await;
    world.ticks(2).await;
    while let Ok(event) = w.events.try_recv() {
        w.session.on_event(event).await;
    }
    w.session.tick += 100;
    for _ in 0..4 {
        knocked_with(&mut w, 0.7, 0.9).await;
        w.session.tick += 30;
    }
    let alerts = operator_alerts(&world).await;
    assert!(alerts.iter().any(|a| a.contains("velocity")), "{alerts:?}");
    world.shutdown().await;
}
