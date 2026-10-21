//! Per-viewer custom content pack state kept by the session.

use super::*;
use crate::protocol::pack::PackStatus;
use crate::resource_pack::{PackOffer, PackState};
use crate::world::session::PlayerSession;
use tuff_core::custom_pack::pack_id;

fn offer(hash: &str) -> PackOffer {
    PackOffer {
        id: pack_id(),
        url: "http://203.0.113.5/resourcepacks/x.zip".into(),
        sha1: hash.into(),
        required: true,
        prompt: None,
    }
}

fn loaded(session: &PlayerSession) -> bool {
    session.pack_state(pack_id()).is_some_and(|s| s.loaded)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_tracks_custom_pack_state_from_both_phases() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut session, outbound) = session_with_outbound(&world, 1);
    assert!(!loaded(&session));

    let mut states = std::collections::HashMap::new();
    let mut joined = PackState::default();
    joined.offered("aaaa");
    joined.apply(PackStatus::Loaded);
    states.insert(pack_id(), joined);
    session.joined_pack_states(states);
    assert!(loaded(&session));
    assert_eq!(session.pack_state(pack_id()).unwrap().hash, "aaaa");

    session.offer_resource_pack(offer("bbbb"));
    let state = session.pack_state(pack_id()).unwrap();
    assert_eq!((state.hash.as_str(), state.status), ("bbbb", None));
    assert!(loaded(&session), "old version stays applied");
    session.resource_pack_status(pack_id(), Some(PackStatus::Accepted));
    session.resource_pack_status(pack_id(), Some(PackStatus::Downloaded));
    session.resource_pack_status(pack_id(), Some(PackStatus::Loaded));
    assert!(loaded(&session));
    assert!(!session.disconnected);

    session.resource_pack_status(0x1234, Some(PackStatus::Loaded));
    assert!(session.pack_state(0x1234).is_none(), "never offered");

    session.pop_resource_pack(Some(pack_id()));
    assert!(session.pack_state(pack_id()).is_none());
    drop(outbound);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_a_required_repush_disconnects() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    for status in [
        PackStatus::Declined,
        PackStatus::FailedDownload,
        PackStatus::InvalidUrl,
        PackStatus::FailedReload,
        PackStatus::Discarded,
    ] {
        let (mut session, outbound) = session_with_outbound(&world, 1);
        session.offer_resource_pack(offer("cccc"));
        session.resource_pack_status(pack_id(), Some(PackStatus::Accepted));
        assert!(!session.disconnected);
        session.resource_pack_status(pack_id(), Some(status));
        assert!(session.disconnected, "{status:?} disconnects");
        assert!(!loaded(&session));
        drop(outbound);
    }
}
