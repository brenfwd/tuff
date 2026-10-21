//! Teleport confirmations count as moves, one position per client tick,
//! and nothing moves the player before the client has loaded.

use super::anticheat::walker;
use super::*;
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_core::world::session::{CLIENT_LOAD_TICKS, TELEPORT_RESEND_TICKS};
use tuff_vanilla_data::damage_type;

fn to(position: [f64; 3]) -> Serverbound {
    Serverbound::Move(Movement {
        position: Some(position),
        rotation: None,
        on_ground: true,
        horizontal_collision: false,
    })
}

fn turn(yaw: f32) -> Serverbound {
    Serverbound::Move(Movement {
        position: None,
        rotation: Some((yaw, 0.0)),
        on_ground: true,
        horizontal_collision: false,
    })
}

fn accept(id: i32, position: [f64; 3], rotation: (f32, f32)) -> Serverbound {
    Serverbound::AcceptTeleportation {
        id,
        position,
        rotation,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_confirmed_teleport_moves_on_to_the_position_the_client_applied() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.teleport([4.5, 100.0, 4.5]);
    let id = player.sync.pending_teleport.expect("teleport sent");

    player.stand_at([0.5, 100.0, 0.5], true);
    player
        .handle_packet(accept(id, [4.6, 100.0, 4.5], (90.0, 10.0)))
        .await;
    assert_eq!(player.sync.pending_teleport, None);
    assert_eq!(player.pose.position, [4.6, 100.0, 4.5]);
    assert!((player.sync.known_movement[0] - 0.1).abs() < 1e-9);
    assert_eq!((player.pose.yaw, player.pose.pitch), (90.0, 10.0));
    assert!(
        !player.pose.on_ground,
        "a confirmation is never on the ground"
    );
    assert!(!player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_confirmation_is_ignored_and_a_repeat_or_a_bad_one_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.teleport([0.5, 100.0, 0.5]);
    let first = player.sync.pending_teleport.unwrap();
    player.teleport([2.5, 100.0, 0.5]);
    player
        .handle_packet(accept(first, [0.5, 100.0, 0.5], (0.0, 0.0)))
        .await;
    assert!(
        player.sync.pending_teleport.is_some(),
        "an old id confirms nothing"
    );
    assert_eq!(player.pose.position, [2.5, 100.0, 0.5]);
    assert!(!player.disconnected);

    let second = player.sync.pending_teleport.unwrap();
    player
        .handle_packet(accept(second, [2.5, 100.0, 0.5], (0.0, 0.0)))
        .await;
    assert!(!player.disconnected);
    player
        .handle_packet(accept(second, [2.5, 100.0, 0.5], (0.0, 0.0)))
        .await;
    assert!(player.disconnected, "confirming twice kicks");

    let (mut other, _out) = session_with_outbound(&world, 1001);
    other.teleport([0.5, 100.0, 0.5]);
    let id = other.sync.pending_teleport.unwrap();
    other
        .handle_packet(accept(id, [f64::NAN, 100.0, 0.5], (0.0, 0.0)))
        .await;
    assert!(other.disconnected, "a non-number position kicks");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_position_in_one_client_tick_kicks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.handle_packet(to([0.6, 100.0, 0.5])).await;
    player.handle_packet(turn(10.0)).await;
    player.handle_packet(turn(20.0)).await;
    assert!(!player.disconnected, "rotations are not counted");
    player.handle_packet(Serverbound::ClientTickEnd).await;
    player.handle_packet(to([0.7, 100.0, 0.5])).await;
    assert!(!player.disconnected, "one position per tick");
    assert_eq!(player.pose.position, [0.7, 100.0, 0.5]);

    player.teleport([0.7, 100.0, 0.5]);
    let id = player.sync.pending_teleport.unwrap();
    player
        .handle_packet(accept(id, [0.7, 100.0, 0.5], (0.0, 0.0)))
        .await;
    assert!(!player.disconnected, "a confirmation is not a tick's move");

    player.handle_packet(to([0.8, 100.0, 0.5])).await;
    assert!(player.disconnected);
    assert_eq!(player.pose.position, [0.7, 100.0, 0.5]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moves_wait_for_the_client_to_load_or_sixty_ticks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.await_client_load();
    player.teleport([0.5, 100.0, 0.5]);
    let id = player.sync.pending_teleport.unwrap();
    player
        .handle_packet(accept(id, [0.5, 100.0, 0.6], (0.0, 0.0)))
        .await;
    assert_eq!(
        player.pose.position,
        [0.5, 100.0, 0.6],
        "a confirmation counts before loading"
    );
    player.handle_packet(to([0.5, 100.0, 0.7])).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(
        player.pose.position,
        [0.5, 100.0, 0.6],
        "ignored until loaded"
    );
    player.handle_packet(Serverbound::PlayerLoaded).await;
    player.handle_packet(to([0.5, 100.0, 0.7])).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(player.pose.position, [0.5, 100.0, 0.7]);

    player.await_client_load();
    player.tick += CLIENT_LOAD_TICKS - 1;
    player.handle_packet(to([0.5, 100.0, 0.8])).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(player.pose.position, [0.5, 100.0, 0.7]);
    player.tick += 1;
    player.handle_packet(to([0.5, 100.0, 0.8])).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(
        player.pose.position,
        [0.5, 100.0, 0.8],
        "loaded after 60 ticks"
    );
    assert!(!player.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn confirmations_pass_the_anticheat_without_costing_the_timer() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut w = walker(&world, 1000, ChunkPos::new(40, 40)).await;
    for _ in 0..40 {
        let here = w.session.pose.position;
        w.session.teleport(here);
        w.confirm().await;
        assert!(w.session.sync.pending_teleport.is_none());
        w.step(0.0, w.floor_y, 0.0).await;
        assert!(
            w.session.sync.pending_teleport.is_none() && !w.session.disconnected,
            "set back after a confirmation"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actions_and_damage_wait_for_the_client_to_load() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.await_client_load();
    player.handle_packet(Serverbound::PlayerCommand(1)).await;
    player.handle_packet(Serverbound::PlayerInput(0x20)).await;
    assert!(
        !player.pose.sprinting && !player.pose.sneaking,
        "ignored until loaded"
    );
    assert_eq!(
        player.sync.input_flags, 0x20,
        "the input is still remembered"
    );
    let health = player.body.health.value;
    assert!(!player.damage(4.0, &damage_type::GENERIC, None).await);
    assert_eq!(
        player.body.health.value, health,
        "invulnerable until loaded"
    );

    player.handle_packet(Serverbound::PlayerLoaded).await;
    player.handle_packet(Serverbound::PlayerCommand(1)).await;
    player.handle_packet(Serverbound::PlayerInput(0x20)).await;
    assert!(player.pose.sprinting && player.pose.sneaking);
    assert!(player.damage(4.0, &damage_type::GENERIC, None).await);
    assert!(player.body.health.value < health);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_teleport_unconfirmed_for_twenty_ticks_is_sent_again_on_a_move() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 100.0, 0.5], true);
    player.teleport([4.5, 100.0, 4.5]);
    let first = player.sync.pending_teleport.unwrap();
    player.tick += TELEPORT_RESEND_TICKS;
    player.handle_packet(to([0.6, 100.0, 0.5])).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    assert_eq!(player.sync.pending_teleport, Some(first), "not yet");
    player.tick += 1;
    player.handle_packet(to([0.6, 100.0, 0.5])).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    let second = player.sync.pending_teleport.unwrap();
    assert_ne!(second, first, "sent again with a new id");
    assert_eq!(player.pose.position, [4.5, 100.0, 4.5]);
    player
        .handle_packet(accept(first, [4.5, 100.0, 4.5], (0.0, 0.0)))
        .await;
    assert_eq!(
        player.sync.pending_teleport,
        Some(second),
        "the old id is stale"
    );
    player
        .handle_packet(accept(second, [4.5, 100.0, 4.5], (0.0, 0.0)))
        .await;
    assert_eq!(player.sync.pending_teleport, None);
    assert!(!player.disconnected);
    world.shutdown().await;
}
