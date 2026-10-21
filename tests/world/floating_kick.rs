//! Survival players hovering in the air are kicked unless allow-flight is set.

use super::*;
use tuff_core::protocol::play::{Movement, Serverbound};

fn hover(y: f64) -> Serverbound {
    Serverbound::Move(Movement {
        position: Some([0.5, y, 0.5]),
        rotation: None,
        on_ground: false,
        horizontal_collision: false,
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hovering_survival_player_is_kicked_after_eighty_ticks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 200.0, 0.5], false);
    for x in -1..=0 {
        for z in -1..=0 {
            player.client_world_mut().insert(
                ChunkPos::new(x, z),
                std::sync::Arc::new(tuff_world_model::chunk::Chunk::empty(
                    tuff_world_model::chunk::ChunkShape::OVERWORLD,
                )),
            );
        }
    }
    player.sync.load_deadline = None;
    for n in 0..80 {
        assert!(!player.disconnected, "tick {n}");
        player.handle_packet(hover(200.0)).await;
        player.handle_packet(Serverbound::ClientTickEnd).await;
        player.on_tick().await;
    }
    assert!(!player.disconnected);
    player.handle_packet(hover(200.0)).await;
    player.handle_packet(Serverbound::ClientTickEnd).await;
    player.on_tick().await;
    assert!(player.disconnected);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn falling_survival_players_are_not_kicked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _out) = session_with_outbound(&world, 1000);
    player.stand_at([0.5, 200.0, 0.5], false);
    for i in 0..100 {
        player.handle_packet(hover(200.0 - i as f64)).await;
        player.handle_packet(Serverbound::ClientTickEnd).await;
        player.on_tick().await;
    }
    assert!(!player.disconnected);
}
