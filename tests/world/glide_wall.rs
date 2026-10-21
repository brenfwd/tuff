//! Gliding into a wall hurts by the lost horizontal speed.

use super::*;
use crate::world::session::PlayerSession;
use tuff_core::protocol::play::{Movement, Serverbound};

fn glide_move(x: f64, collided: bool) -> Serverbound {
    Serverbound::Move(Movement {
        position: Some([x, 200.0, 0.5]),
        rotation: None,
        on_ground: false,
        horizontal_collision: collided,
    })
}

async fn step(player: &mut PlayerSession, packet: Serverbound) {
    player.motion.glide.active = true;
    player.handle_packet(packet).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    player.on_tick().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fast_glide_into_a_wall_hurts_and_a_graze_does_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 200.0, 0.5], false);
    player.sync.load_deadline = None;
    player.motion.glide.active = true;
    step(&mut player, glide_move(1.5, false)).await;
    step(&mut player, glide_move(2.5, false)).await;
    assert_eq!(player.pose.position[0], 2.5);
    assert_eq!(player.health(), 20.0);
    step(&mut player, glide_move(2.5, true)).await;
    assert_eq!(player.health(), 13.0);
    step(&mut player, glide_move(3.0, false)).await;
    step(&mut player, glide_move(3.4, true)).await;
    assert_eq!(player.health(), 13.0, "a graze loses too little speed");
}
