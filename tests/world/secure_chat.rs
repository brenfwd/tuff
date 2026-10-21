//! Signed chat: chat session keys, the message chain, acknowledgements and
//! `enforce-secure-profile`.

use super::*;
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};
use tuff_core::protocol::chat_session::{
    ChatSession, LastSeenUpdate, MessageBody, last_seen_checksum, signed_payload,
};
use tuff_core::protocol::play::{Serverbound, SignedChat, SignedCommand};

const SESSION_ID: u128 = 0xabcd;
const FAR_FUTURE_MS: i64 = 4_000_000_000_000;
const NO_UPDATE: LastSeenUpdate = LastSeenUpdate {
    offset: 0,
    acknowledged: 0,
    checksum: 0,
};

struct Keys {
    authority: RsaPrivateKey,
    player: RsaPrivateKey,
}

fn sign(key: &RsaPrivateKey, data: &[u8]) -> Vec<u8> {
    key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(data))
        .unwrap()
}

fn keys() -> Keys {
    let mut rng = rand::thread_rng();
    Keys {
        authority: RsaPrivateKey::new(&mut rng, 2048).unwrap(),
        player: RsaPrivateKey::new(&mut rng, 2048).unwrap(),
    }
}

fn session_for(keys: &Keys, profile: u128, expires_at_ms: i64) -> ChatSession {
    let der = RsaPublicKey::from(&keys.player)
        .to_public_key_der()
        .unwrap();
    let mut payload = profile.to_be_bytes().to_vec();
    payload.extend_from_slice(&expires_at_ms.to_be_bytes());
    payload.extend_from_slice(der.as_bytes());
    ChatSession {
        session_id: SESSION_ID,
        expires_at_ms,
        public_key: der.as_bytes().to_vec(),
        key_signature: sign(&keys.authority, &payload),
    }
}

fn chat(keys: &Keys, profile: u128, index: i32, text: &str, timestamp_ms: i64) -> Serverbound {
    let body = MessageBody {
        content: text,
        timestamp_ms,
        salt: 9,
        last_seen: &[],
    };
    let signature = sign(
        &keys.player,
        &signed_payload(profile, SESSION_ID, index, &body),
    );
    Serverbound::SignedChat(Box::new(SignedChat {
        message: text.to_owned(),
        timestamp_ms,
        salt: 9,
        signature: Some(signature.try_into().unwrap()),
        update: NO_UPDATE,
    }))
}

fn unsigned(text: &str) -> Serverbound {
    Serverbound::SignedChat(Box::new(SignedChat {
        message: text.to_owned(),
        timestamp_ms: 1,
        salt: 1,
        signature: None,
        update: NO_UPDATE,
    }))
}

fn refusals(outbound: &mut tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>, key: &str) -> usize {
    let mut found = 0;
    while let Ok(frame) = outbound.try_recv() {
        if frame.windows(key.len()).any(|w| w == key.as_bytes()) {
            found += 1;
        }
    }
    found
}

fn authority_knows(world: &TestWorld, keys: &Keys, enforcing: bool) {
    world
        .shared
        .profile_keys
        .set_keys(vec![RsaPublicKey::from(&keys.authority)]);
    world.shared.profile_keys.set_enforcing(enforcing);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_key_is_accepted_only_when_the_authority_signed_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let keys = keys();
    let (mut session, _outbound) = session_with_outbound(&world, 1);
    let profile = session.profile.id;

    session
        .handle_packet(Serverbound::ChatSessionUpdate(session_for(
            &keys,
            profile,
            FAR_FUTURE_MS,
        )))
        .await;
    assert!(!session.disconnected, "keys not loaded: ignored");

    authority_knows(&world, &keys, false);
    let mut forged = session_for(&keys, profile, FAR_FUTURE_MS);
    forged.expires_at_ms += 1;
    session
        .handle_packet(Serverbound::ChatSessionUpdate(forged))
        .await;
    assert!(session.disconnected, "signature does not cover the key");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_chat_key_disconnects() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let keys = keys();
    authority_knows(&world, &keys, false);
    let (mut session, _outbound) = session_with_outbound(&world, 1);
    let profile = session.profile.id;
    session
        .handle_packet(Serverbound::ChatSessionUpdate(session_for(
            &keys, profile, 1,
        )))
        .await;
    assert!(session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_messages_chain_and_a_bad_one_breaks_the_rest() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let keys = keys();
    authority_knows(&world, &keys, true);
    let (mut session, mut outbound) = session_with_outbound(&world, 1);
    let profile = session.profile.id;
    session
        .handle_packet(Serverbound::ChatSessionUpdate(session_for(
            &keys,
            profile,
            FAR_FUTURE_MS,
        )))
        .await;
    assert!(!session.disconnected);
    refusals(&mut outbound, "");

    session
        .handle_packet(chat(&keys, profile, 0, "one", 10_000))
        .await;
    session
        .handle_packet(chat(&keys, profile, 1, "two", 11_000))
        .await;
    assert_eq!(refusals(&mut outbound, "chat.disabled"), 0);

    session
        .handle_packet(chat(&keys, profile, 5, "skipped", 12_000))
        .await;
    assert_eq!(
        refusals(&mut outbound, "chat.disabled.invalid_signature"),
        1
    );
    session
        .handle_packet(chat(&keys, profile, 2, "late", 13_000))
        .await;
    assert_eq!(refusals(&mut outbound, "chat.disabled.chain_broken"), 1);
    assert!(!session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_may_not_go_back_in_time() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let keys = keys();
    authority_knows(&world, &keys, true);
    let (mut session, mut outbound) = session_with_outbound(&world, 1);
    let profile = session.profile.id;
    session
        .handle_packet(Serverbound::ChatSessionUpdate(session_for(
            &keys,
            profile,
            FAR_FUTURE_MS,
        )))
        .await;
    session
        .handle_packet(chat(&keys, profile, 0, "one", 10_000))
        .await;
    session
        .handle_packet(chat(&keys, profile, 1, "two", 9_000))
        .await;
    assert_eq!(
        refusals(&mut outbound, "chat.disabled.out_of_order_chat"),
        1
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsigned_chat_is_refused_only_while_enforcing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let keys = keys();
    let (mut session, mut outbound) = session_with_outbound(&world, 1);

    session.handle_packet(unsigned("open")).await;
    assert_eq!(refusals(&mut outbound, "chat.disabled"), 0);

    world.shared.profile_keys.set_enforcing(true);
    session.handle_packet(unsigned("still open")).await;
    assert_eq!(
        refusals(&mut outbound, "chat.disabled"),
        0,
        "nothing to check keys against"
    );

    authority_knows(&world, &keys, true);
    session.handle_packet(unsigned("closed")).await;
    assert_eq!(
        refusals(&mut outbound, "chat.disabled.missingProfileKey"),
        1
    );
    assert!(!session.disconnected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_session_needs_signed_messages() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let keys = keys();
    authority_knows(&world, &keys, false);
    let (mut session, mut outbound) = session_with_outbound(&world, 1);
    let profile = session.profile.id;
    session
        .handle_packet(Serverbound::ChatSessionUpdate(session_for(
            &keys,
            profile,
            FAR_FUTURE_MS,
        )))
        .await;
    session.handle_packet(unsigned("plain")).await;
    assert_eq!(
        refusals(&mut outbound, "chat.disabled.missingProfileKey"),
        1
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_acknowledgements_disconnect_and_good_ones_do_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());

    let (mut session, _outbound) = session_with_outbound(&world, 1);
    session.handle_packet(Serverbound::ChatAck(0)).await;
    session
        .handle_packet(Serverbound::SignedChat(Box::new(SignedChat {
            message: "hi".into(),
            timestamp_ms: 1,
            salt: 1,
            signature: None,
            update: LastSeenUpdate {
                offset: 0,
                acknowledged: 0,
                checksum: last_seen_checksum(&[]),
            },
        })))
        .await;
    assert!(!session.disconnected);

    session.handle_packet(Serverbound::ChatAck(1)).await;
    assert!(session.disconnected, "nothing was sent to scroll out");

    let (mut session, _outbound) = session_with_outbound(&world, 2);
    let mut update = NO_UPDATE;
    update.acknowledged = 1;
    session
        .handle_packet(Serverbound::SignedCommand(Box::new(SignedCommand {
            command: "list".into(),
            timestamp_ms: 1,
            salt: 1,
            signatures: Vec::new(),
            update,
        })))
        .await;
    assert!(session.disconnected, "acknowledged a message never sent");

    let (mut session, _outbound) = session_with_outbound(&world, 3);
    let mut update = NO_UPDATE;
    update.checksum = 77;
    session
        .handle_packet(Serverbound::SignedCommand(Box::new(SignedCommand {
            command: "list".into(),
            timestamp_ms: 1,
            salt: 1,
            signatures: Vec::new(),
            update,
        })))
        .await;
    assert!(session.disconnected, "checksum mismatch");
    world.shutdown().await;
}
