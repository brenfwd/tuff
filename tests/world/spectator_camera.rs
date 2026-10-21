//! A spectator watches through a nearby entity: Set Camera on attack or the
//! spectate packet, the view follows the target, sneaking lets go.

use super::*;
use crate::protocol::play::{GameMode, Serverbound};
use tuff_vanilla_data::packet::play::Clientbound as C;

fn camera_packets(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>) -> Vec<i32> {
    let mut targets = Vec::new();
    while let Ok(frame) = outbound.try_recv() {
        if framed_packet_id(&frame) == C::SetCamera.id() {
            let mut payload = framed_payload(&frame);
            payload.read_var_int().unwrap();
            targets.push(payload.read_var_int().unwrap());
        }
    }
    targets
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attacking_a_near_player_in_spectator_sets_the_camera_and_follows() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 1000);
    let own = player.id();
    player.mode.game_mode = GameMode::Spectator;
    player.stand_at([4.5, 151.0, 6.5], false);
    let (target, _events) = PlayerHandle::new(2000);
    player.see_player(&target, [4.5, 151.0, 8.5]);
    player.handle_packet(Serverbound::PlayerLoaded).await;
    player.run_tick().await;
    camera_packets(&mut outbound);

    player.handle_packet(Serverbound::Attack(2000)).await;
    assert_eq!(player.camera(), Some(2000));
    assert_eq!(camera_packets(&mut outbound), vec![2000]);

    player.run_tick().await;
    assert_eq!(player.position(), [4.5, 151.0, 8.5]);

    player.handle_packet(Serverbound::PlayerInput(0x20)).await;
    assert_eq!(player.camera(), None);
    assert_eq!(camera_packets(&mut outbound), vec![own]);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_camera_ignores_far_targets_survival_players_and_self() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 1000);
    let own = player.id();
    player.stand_at([4.5, 151.0, 6.5], false);
    let (far, _far_events) = PlayerHandle::new(2000);
    player.see_player(&far, [4.5, 151.0, 12.5]);
    player.run_tick().await;
    camera_packets(&mut outbound);

    player
        .handle_packet(Serverbound::Spectate(Some(2000)))
        .await;
    assert_eq!(player.camera(), None, "not in spectator mode");
    player.mode.game_mode = GameMode::Spectator;
    player
        .handle_packet(Serverbound::Spectate(Some(2000)))
        .await;
    assert_eq!(player.camera(), None, "out of range");
    player.handle_packet(Serverbound::Spectate(Some(own))).await;
    assert_eq!(player.camera(), None, "itself");
    assert!(camera_packets(&mut outbound).is_empty());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_camera_resets_when_the_target_leaves_view() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = session_with_outbound(&world, 1000);
    let own = player.id();
    player.mode.game_mode = GameMode::Spectator;
    player.stand_at([4.5, 151.0, 6.5], false);
    let (target, _events) = PlayerHandle::new(2000);
    player.see_player(&target, [4.5, 151.0, 8.5]);
    player.run_tick().await;
    player
        .handle_packet(Serverbound::Spectate(Some(2000)))
        .await;
    assert_eq!(player.camera(), Some(2000));
    camera_packets(&mut outbound);

    player.mode.game_mode = GameMode::Survival;
    player.run_tick().await;
    assert_eq!(player.camera(), None);
    assert_eq!(camera_packets(&mut outbound), vec![own]);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_spectator_menu_teleports_to_a_far_player_and_only_in_spectator() {
    use crate::world::nearby::{IndexedPlayer, PlayerIndex};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _outbound) = session_with_outbound(&world, 1000);
    player.stand_at([4.5, 151.0, 6.5], false);
    let (target, _events) = PlayerHandle::new(2000);
    let far = [4000.5, 151.0, -3000.5];
    let index = PlayerIndex::build([(
        player.dimension_id(),
        IndexedPlayer {
            handle: target,
            uuid: 77,
            name: "far".into(),
            position: far,
            looks: Default::default(),
            time_since_rest: 0,
        },
    )]);
    player.teleport_to_player(&index, 77).await;
    assert_eq!(player.position(), [4.5, 151.0, 6.5]);
    player.mode.game_mode = GameMode::Spectator;
    player.teleport_to_player(&index, 99).await;
    assert_eq!(player.position(), [4.5, 151.0, 6.5]);
    player.teleport_to_player(&index, 77).await;
    assert_eq!(player.position(), far);
}
