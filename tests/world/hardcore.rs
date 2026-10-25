//! Hardcore toggled under connected players: a fresh Login for the living,
//! the next respawn for the dead, and spectator after a hardcore death.

use super::*;
use crate::protocol::play::{GameMode, Serverbound};
use crate::world::session::PlayerSession;
use std::sync::atomic::Ordering;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::packet::play::Clientbound;

fn set_hardcore(world: &TestWorld, on: bool) {
    world.shared.hardcore.store(on, Ordering::Relaxed);
}

fn login_flags(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>) -> Vec<bool> {
    let mut flags = Vec::new();
    while let Ok(frame) = outbound.try_recv() {
        let mut r = framed_payload(&frame);
        if r.read_var_int().unwrap() == Clientbound::Login.id() {
            r.read_int().unwrap();
            flags.push(r.read_bool().unwrap());
        }
    }
    flags
}

async fn kill(session: &mut PlayerSession) {
    session
        .damage(1000.0, &damage_type::GENERIC_KILL, None)
        .await;
    assert!(session.is_dead());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_living_player_gets_a_fresh_login_when_hardcore_toggles() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut outbound) = session_with_outbound(&world, 1);
    login_flags(&mut outbound);
    session.sync_hardcore().await;
    assert!(login_flags(&mut outbound).is_empty());
    set_hardcore(&world, true);
    session.sync_hardcore().await;
    assert_eq!(login_flags(&mut outbound), vec![true]);
    set_hardcore(&world, false);
    session.sync_hardcore().await;
    assert_eq!(login_flags(&mut outbound), vec![false]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hardcore_death_respawns_into_spectator_with_the_flag() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, mut outbound) = session_with_outbound(&world, 1);
    kill(&mut session).await;
    set_hardcore(&world, true);
    session.sync_hardcore().await;
    login_flags(&mut outbound);
    session.handle_packet(Serverbound::ClientCommand(0)).await;
    assert_eq!(login_flags(&mut outbound), vec![true]);
    assert!(!session.is_dead());
    assert_eq!(session.mode.game_mode, GameMode::Spectator);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_normal_death_still_respawns_in_the_same_mode() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session_with_outbound(&world, 1);
    let mode = session.mode.game_mode;
    kill(&mut session).await;
    session.respawn().await;
    assert_eq!(session.mode.game_mode, mode);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entering_spectator_mode_drops_the_shoulder_entities() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, _outbound) = session_with_outbound(&world, 1);
    session.tick = 100;
    session.body.shoulders.entities[0] =
        Some(tuff_core::world::stored_entity::StoredEntity::from_storage(
            tuff_nbt::Compound::new().with("id", "minecraft:parrot"),
        ));
    session.set_game_mode(GameMode::Spectator).await;
    assert!(session.body.shoulders.entities.iter().all(Option::is_none));
    world.shutdown().await;
}
