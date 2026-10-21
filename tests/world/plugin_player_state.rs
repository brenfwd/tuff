//! Plugin-driven vitals, experience and effects reach the player's
//! published state.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::{Directive, DirectiveSource};
use crate::world::session::PlayerSession;
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;

fn session(shared: &Arc<Shared>) -> PlayerSession {
    let (outbound, frames) = mpsc::unbounded_channel();
    std::mem::forget(frames);
    let (handle, _events) = PlayerHandle::new(1);
    let (_unused, session_events) = PlayerHandle::new(1);
    let (_packets, packets) = mpsc::channel(8);
    PlayerSession::new(
        shared.clone(),
        1,
        GameProfile {
            id: 1,
            name: "Alpha".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle,
        session_events,
        packets,
        PermissionLevel::Owners,
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_directives_change_published_vitals() {
    let (coordinator, mut requests) = mpsc::channel(64);
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    let shared = Arc::new(shared);
    let mut session = session(&shared);
    let source = || DirectiveSource {
        name: TextComponent::new("Plugin"),
        player: None,
    };
    session
        .apply_directive(
            Directive::SetVitals {
                health: Some(7.0),
                food: Some(3),
            },
            source(),
        )
        .await;
    session
        .apply_directive(
            Directive::Experience {
                amount: 5,
                levels: true,
                set: true,
            },
            source(),
        )
        .await;
    session
        .apply_directive(
            Directive::EffectGive {
                effect: tuff_vanilla_data::registry::MobEffect::Speed,
                duration: 200,
                amplifier: 1,
                particles: false,
            },
            source(),
        )
        .await;
    session.report_state().await;
    let state = loop {
        if let CoordinatorMessage::PlayerState { state, .. } = requests.recv().await.unwrap() {
            break state;
        }
    };
    assert_eq!(state.health, 7.0);
    assert_eq!(state.food, 3);
    assert_eq!(state.level, 5);
    assert_eq!(
        state.effects,
        [(tuff_vanilla_data::registry::MobEffect::Speed, 1)]
    );
}
