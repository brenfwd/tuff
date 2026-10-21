use std::collections::VecDeque;
use std::sync::{PoisonError, RwLock};

use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tuff_wire::{BufferError, PacketReader};

pub const SIGNATURE_BYTES: usize = 256;
pub const MAX_PUBLIC_KEY_BYTES: usize = 512;
pub const MAX_KEY_SIGNATURE_BYTES: usize = 4096;

/// A player's chat session: the id their signed messages carry and the
/// profile key those messages are signed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSession {
    pub session_id: u128,
    pub expires_at_ms: i64,
    pub public_key: Vec<u8>,
    pub key_signature: Vec<u8>,
}

impl ChatSession {
    pub fn decode(r: &mut PacketReader) -> Result<Self, BufferError> {
        let session_id = r.read_uuid()?;
        let expires_at_ms = r.read_long()?;
        let public_key = r.read_prefixed_bytes(MAX_PUBLIC_KEY_BYTES)?.to_vec();
        let key_signature = r.read_prefixed_bytes(MAX_KEY_SIGNATURE_BYTES)?.to_vec();
        Ok(Self {
            session_id,
            expires_at_ms,
            public_key,
            key_signature,
        })
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms <= now_ms
    }

    /// The profile key, if its bytes are a valid RSA public key.
    pub fn key(&self) -> Option<RsaPublicKey> {
        RsaPublicKey::from_public_key_der(&self.public_key).ok()
    }
}

/// Whether `signature` is a SHA256withRSA signature of `data` by `key`.
pub fn verify_signature(key: &RsaPublicKey, data: &[u8], signature: &[u8]) -> bool {
    signature.len() == SIGNATURE_BYTES
        && key
            .verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(data),
                signature,
            )
            .is_ok()
}

pub type Signature = [u8; SIGNATURE_BYTES];

pub const LAST_SEEN_WINDOW: usize = 20;
pub const SIGNATURE_CACHE_SIZE: usize = 128;
const BITSET_BYTES: usize = 3;

pub const MISSING_PROFILE_KEY: &str = "chat.disabled.missingProfileKey";
pub const CHAIN_BROKEN: &str = "chat.disabled.chain_broken";
pub const EXPIRED_PROFILE_KEY: &str = "chat.disabled.expiredProfileKey";
pub const INVALID_SIGNATURE: &str = "chat.disabled.invalid_signature";
pub const OUT_OF_ORDER_CHAT: &str = "chat.disabled.out_of_order_chat";
pub const CHAT_VALIDATION_FAILED: &str = "multiplayer.disconnect.chat_validation_failed";
pub const EXPIRED_PUBLIC_KEY: &str = "multiplayer.disconnect.expired_public_key";
pub const INVALID_PUBLIC_KEY_SIGNATURE: &str =
    "multiplayer.disconnect.invalid_public_key_signature";

/// How far the client says it has read: how many tracked messages scrolled
/// out of its window, which of the remaining 20 it saw, and a checksum of
/// their signatures (zero skips the check).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastSeenUpdate {
    pub offset: i32,
    pub acknowledged: u32,
    pub checksum: i8,
}

impl LastSeenUpdate {
    pub fn decode(r: &mut PacketReader) -> Result<Self, BufferError> {
        let offset = r.read_var_int()?;
        let mut acknowledged = 0u32;
        for (i, byte) in r.read_bytes(BITSET_BYTES)?.iter().enumerate() {
            acknowledged |= u32::from(*byte) << (8 * i);
        }
        let checksum = r.read_byte()?;
        Ok(Self {
            offset,
            acknowledged,
            checksum,
        })
    }

    pub fn encode_into(&self, w: &mut tuff_wire::PacketWriter) {
        w.write_var_int(self.offset);
        for i in 0..BITSET_BYTES {
            w.write_bytes(&[(self.acknowledged >> (8 * i)) as u8]);
        }
        w.write_byte(self.checksum);
    }
}

/// A message-acknowledgement the client got wrong; the player is kicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

#[derive(Debug, Clone)]
struct Tracked {
    signature: Signature,
    pending: bool,
}

/// The messages the server has sent a player and which of them the player
/// has confirmed seeing, over a window of the latest 20.
#[derive(Debug, Clone)]
pub struct LastSeenValidator {
    tracked: VecDeque<Option<Tracked>>,
    last_pending: Option<Signature>,
}

impl Default for LastSeenValidator {
    fn default() -> Self {
        Self {
            tracked: (0..LAST_SEEN_WINDOW).map(|_| None).collect(),
            last_pending: None,
        }
    }
}

impl LastSeenValidator {
    pub fn add_pending(&mut self, signature: Signature) {
        if self.last_pending != Some(signature) {
            self.tracked.push_back(Some(Tracked {
                signature,
                pending: true,
            }));
            self.last_pending = Some(signature);
        }
    }

    pub fn tracked_count(&self) -> usize {
        self.tracked.len()
    }

    pub fn apply_offset(&mut self, offset: i32) -> Result<(), ValidationError> {
        let max = self.tracked.len() - LAST_SEEN_WINDOW;
        if offset < 0 || offset as usize > max {
            return Err(ValidationError(format!(
                "Advanced last seen window by {offset} messages, but expected at most {max}"
            )));
        }
        self.tracked.drain(..offset as usize);
        Ok(())
    }

    /// Applies the client's update and returns the signatures it
    /// acknowledged, in window order.
    pub fn apply_update(
        &mut self,
        update: &LastSeenUpdate,
    ) -> Result<Vec<Signature>, ValidationError> {
        self.apply_offset(update.offset)?;
        let length = 32 - update.acknowledged.leading_zeros() as usize;
        if length > LAST_SEEN_WINDOW {
            return Err(ValidationError(format!(
                "Last seen update contained {length} messages, but maximum window size is {LAST_SEEN_WINDOW}"
            )));
        }
        let mut seen = Vec::with_capacity(update.acknowledged.count_ones() as usize);
        for (i, entry) in self.tracked.iter_mut().take(LAST_SEEN_WINDOW).enumerate() {
            let acknowledged = update.acknowledged >> i & 1 == 1;
            if acknowledged {
                let Some(tracked) = entry else {
                    return Err(ValidationError(format!(
                        "Last seen update acknowledged unknown or previously ignored message at index {i}"
                    )));
                };
                tracked.pending = false;
                seen.push(tracked.signature);
            } else {
                if entry.as_ref().is_some_and(|tracked| !tracked.pending) {
                    return Err(ValidationError(format!(
                        "Last seen update ignored previously acknowledged message at index {i}"
                    )));
                }
                *entry = None;
            }
        }
        if update.checksum != 0 && update.checksum != last_seen_checksum(&seen) {
            return Err(ValidationError(
                "Checksum mismatch on last seen update: the client and server must have desynced"
                    .to_owned(),
            ));
        }
        Ok(seen)
    }
}

fn signature_checksum(signature: &Signature) -> i32 {
    signature.iter().fold(1i32, |h, b| {
        h.wrapping_mul(31).wrapping_add(i32::from(*b as i8))
    })
}

pub fn last_seen_checksum(seen: &[Signature]) -> i8 {
    let checksum = seen.iter().fold(1i32, |h, signature| {
        h.wrapping_mul(31)
            .wrapping_add(signature_checksum(signature))
    });
    match checksum as i8 {
        0 => 1,
        byte => byte,
    }
}

/// The signatures the server and one client both remember, so a later
/// message can name an earlier signature by its slot instead of resending it.
#[derive(Debug, Clone)]
pub struct SignatureCache {
    entries: Vec<Option<Signature>>,
}

impl Default for SignatureCache {
    fn default() -> Self {
        Self {
            entries: vec![None; SIGNATURE_CACHE_SIZE],
        }
    }
}

impl SignatureCache {
    pub fn pack(&self, signature: &Signature) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.as_ref() == Some(signature))
    }

    pub fn unpack(&self, id: usize) -> Option<&Signature> {
        self.entries.get(id)?.as_ref()
    }

    /// Records a message's last-seen signatures and its own signature, newest
    /// first; entries pushed out of the window that were not just re-sent are
    /// kept by moving them down.
    pub fn push(&mut self, last_seen: &[Signature], signature: Option<&Signature>) {
        let mut queue: VecDeque<Signature> = last_seen.iter().copied().collect();
        queue.extend(signature.copied());
        let incoming: std::collections::HashSet<Signature> = queue.iter().copied().collect();
        for i in 0..self.entries.len() {
            if queue.is_empty() {
                break;
            }
            let evicted = self.entries[i].take();
            self.entries[i] = queue.pop_back();
            if let Some(evicted) = evicted
                && !incoming.contains(&evicted)
            {
                queue.push_front(evicted);
            }
        }
    }
}

/// The signed fields of one chat message.
#[derive(Debug, Clone, Copy)]
pub struct MessageBody<'a> {
    pub content: &'a str,
    pub timestamp_ms: i64,
    pub salt: i64,
    pub last_seen: &'a [Signature],
}

/// Why a signed message was refused; shown to the player as a red message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatError {
    MissingProfileKey,
    ExpiredProfileKey,
    ChainBroken,
    OutOfOrder,
    InvalidSignature,
}

impl ChatError {
    pub fn translation_key(self) -> &'static str {
        match self {
            Self::MissingProfileKey => MISSING_PROFILE_KEY,
            Self::ExpiredProfileKey => EXPIRED_PROFILE_KEY,
            Self::ChainBroken => CHAIN_BROKEN,
            Self::OutOfOrder => OUT_OF_ORDER_CHAT,
            Self::InvalidSignature => INVALID_SIGNATURE,
        }
    }
}

/// A player's run of signed messages: each is signed with the next index
/// of the chain, so a dropped or reordered message breaks everything after.
#[derive(Debug, Clone)]
pub struct ChatChain {
    sender: u128,
    session_id: u128,
    next_index: Option<i32>,
    last_timestamp_ms: i64,
}

impl ChatChain {
    pub fn new(sender: u128, session_id: u128) -> Self {
        Self {
            sender,
            session_id,
            next_index: Some(0),
            last_timestamp_ms: 0,
        }
    }

    pub fn session_id(&self) -> u128 {
        self.session_id
    }

    pub fn break_chain(&mut self) {
        self.next_index = None;
    }

    /// Checks `signature` against the next link and `body`, and advances.
    pub fn unpack(
        &mut self,
        key: &RsaPublicKey,
        key_expired: bool,
        signature: Option<&[u8]>,
        body: &MessageBody,
    ) -> Result<(), ChatError> {
        let Some(signature) = signature else {
            return Err(ChatError::MissingProfileKey);
        };
        if key_expired {
            return Err(ChatError::ExpiredProfileKey);
        }
        let Some(index) = self.next_index else {
            return Err(ChatError::ChainBroken);
        };
        if body.timestamp_ms < self.last_timestamp_ms {
            self.break_chain();
            return Err(ChatError::OutOfOrder);
        }
        self.last_timestamp_ms = body.timestamp_ms;
        let payload = signed_payload(self.sender, self.session_id, index, body);
        if !verify_signature(key, &payload, signature) {
            self.break_chain();
            return Err(ChatError::InvalidSignature);
        }
        self.next_index = index.checked_add(1);
        Ok(())
    }

    /// Moves past `count` links whose signatures cannot be checked (the
    /// arguments of a signed command), noting the command's time.
    pub fn skip(&mut self, count: usize, timestamp_ms: i64) -> Result<(), ChatError> {
        let Some(index) = self.next_index else {
            return Err(ChatError::ChainBroken);
        };
        if timestamp_ms < self.last_timestamp_ms {
            self.break_chain();
            return Err(ChatError::OutOfOrder);
        }
        self.last_timestamp_ms = timestamp_ms;
        self.next_index = i32::try_from(count).ok().and_then(|n| index.checked_add(n));
        Ok(())
    }
}

/// The bytes a client signs for one message.
pub fn signed_payload(sender: u128, session_id: u128, index: i32, body: &MessageBody) -> Vec<u8> {
    let content = body.content.as_bytes();
    let mut out = Vec::with_capacity(64 + content.len() + body.last_seen.len() * SIGNATURE_BYTES);
    out.extend_from_slice(&1i32.to_be_bytes());
    out.extend_from_slice(&sender.to_be_bytes());
    out.extend_from_slice(&session_id.to_be_bytes());
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(&body.salt.to_be_bytes());
    out.extend_from_slice(&body.timestamp_ms.div_euclid(1000).to_be_bytes());
    out.extend_from_slice(&(content.len() as i32).to_be_bytes());
    out.extend_from_slice(content);
    out.extend_from_slice(&(body.last_seen.len() as i32).to_be_bytes());
    for signature in body.last_seen {
        out.extend_from_slice(signature);
    }
    out
}

/// Mojang's public keys for player certificates, which sign every profile
/// key. Empty until fetched; secure chat is not enforced while it is.
#[derive(Default)]
pub struct ProfileKeyAuthority {
    keys: RwLock<Vec<RsaPublicKey>>,
    enforce: std::sync::atomic::AtomicBool,
}

impl ProfileKeyAuthority {
    pub fn set_keys(&self, keys: Vec<RsaPublicKey>) {
        *self.keys.write().unwrap_or_else(PoisonError::into_inner) = keys;
    }

    /// Whether `enforce-secure-profile` applies: set when the server
    /// authenticates players and the property is on.
    pub fn set_enforcing(&self, enforce: bool) {
        self.enforce
            .store(enforce, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether players without a valid chat key may not chat: vanilla only
    /// enforces what it can check.
    pub fn enforcing(&self) -> bool {
        self.enforce.load(std::sync::atomic::Ordering::Relaxed) && self.can_validate()
    }

    pub fn can_validate(&self) -> bool {
        !self
            .keys
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Whether Mojang signed `session`'s key for the account `profile_id`.
    pub fn validate(&self, profile_id: u128, session: &ChatSession) -> bool {
        let mut payload = Vec::with_capacity(24 + session.public_key.len());
        payload.extend_from_slice(&profile_id.to_be_bytes());
        payload.extend_from_slice(&session.expires_at_ms.to_be_bytes());
        payload.extend_from_slice(&session.public_key);
        let keys = self.keys.read().unwrap_or_else(PoisonError::into_inner);
        keys.iter().any(|key| {
            let signature = session.key_signature.as_slice();
            key.verify(
                Pkcs1v15Sign::new::<Sha1>(),
                &Sha1::digest(&payload),
                signature,
            )
            .is_ok()
                || key
                    .verify(
                        Pkcs1v15Sign::new::<Sha256>(),
                        &Sha256::digest(&payload),
                        signature,
                    )
                    .is_ok()
        })
    }
}

/// Parses the key list of the profile API's `publickeys` response.
pub fn parse_authority_keys(body: &[u8]) -> Vec<RsaPublicKey> {
    use base64::Engine;
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    json["playerCertificateKeys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["publicKey"].as_str())
        .filter_map(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .filter_map(|der| RsaPublicKey::from_public_key_der(&der).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePublicKey;
    use tuff_wire::PacketWriter;

    fn pair() -> (RsaPrivateKey, RsaPublicKey) {
        let private = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let public = RsaPublicKey::from(&private);
        (private, public)
    }

    fn sign(private: &RsaPrivateKey, data: &[u8]) -> Vec<u8> {
        private
            .sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(data))
            .unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn verifies_good_signatures_and_rejects_bad_ones() {
        let (private, public) = pair();
        let signature = sign(&private, b"hello");
        assert_eq!(signature.len(), SIGNATURE_BYTES);
        assert!(verify_signature(&public, b"hello", &signature));
        assert!(!verify_signature(&public, b"hullo", &signature));
        assert!(!verify_signature(&public, b"hello", &signature[1..]));
        let mut padded = signature.clone();
        padded.push(0);
        assert!(!verify_signature(&public, b"hello", &padded));
        assert!(!verify_signature(&public, b"hello", &[]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn decodes_a_session_and_bounds_its_fields() {
        let (_, public) = pair();
        let der = public.to_public_key_der().unwrap();
        let mut w = PacketWriter::new();
        w.write_uuid(7);
        w.write_long(1_000);
        w.write_var_int(der.as_bytes().len() as i32);
        w.write_bytes(der.as_bytes());
        w.write_var_int(3);
        w.write_bytes(&[1, 2, 3]);
        let session = ChatSession::decode(&mut PacketReader::new(w.into_bytes())).unwrap();
        assert_eq!(session.session_id, 7);
        assert!(session.key().is_some());
        assert!(session.is_expired(1_000));
        assert!(!session.is_expired(999));

        let mut w = PacketWriter::new();
        w.write_uuid(7);
        w.write_long(1);
        w.write_var_int(MAX_PUBLIC_KEY_BYTES as i32 + 1);
        w.write_bytes(&vec![0; MAX_PUBLIC_KEY_BYTES + 1]);
        assert!(ChatSession::decode(&mut PacketReader::new(w.into_bytes())).is_err());

        let truncated = PacketWriter::new();
        assert!(ChatSession::decode(&mut PacketReader::new(truncated.into_bytes())).is_err());
    }

    fn sig(n: u8) -> Signature {
        [n; SIGNATURE_BYTES]
    }

    fn update(offset: i32, acknowledged: u32, seen: &[Signature]) -> LastSeenUpdate {
        LastSeenUpdate {
            offset,
            acknowledged,
            checksum: last_seen_checksum(seen),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn checksum_is_never_zero_and_depends_on_the_signatures() {
        assert_eq!(last_seen_checksum(&[]), 1);
        assert_ne!(last_seen_checksum(&[sig(1), sig(2)]), 0);
        let mut varied = sig(0);
        for (i, byte) in varied.iter_mut().enumerate() {
            *byte = i as u8;
        }
        assert_ne!(last_seen_checksum(&[varied]), last_seen_checksum(&[sig(0)]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn last_seen_tracks_pending_messages_and_acknowledgements() {
        let mut v = LastSeenValidator::default();
        assert_eq!(v.tracked_count(), 20);
        v.add_pending(sig(1));
        v.add_pending(sig(1));
        v.add_pending(sig(2));
        assert_eq!(v.tracked_count(), 22);

        assert!(v.apply_offset(3).is_err(), "only two can scroll out");
        assert!(v.apply_offset(-1).is_err());
        v.apply_offset(2).unwrap();

        let seen = v.apply_update(&update(0, 0, &[])).unwrap();
        assert!(seen.is_empty());

        let mut v = LastSeenValidator::default();
        v.add_pending(sig(1));
        v.add_pending(sig(2));
        let both = [sig(1), sig(2)];
        let seen = v.apply_update(&update(2, 3 << 18, &both)).unwrap();
        assert_eq!(seen, both.to_vec());
        assert!(
            v.apply_update(&update(0, 0, &[])).is_err(),
            "ignoring an acknowledged message"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn last_seen_rejects_unknown_acknowledgements_and_bad_checksums() {
        let mut v = LastSeenValidator::default();
        assert!(v.apply_update(&update(0, 1, &[])).is_err());
        let mut v = LastSeenValidator::default();
        assert!(v.apply_update(&update(0, 1 << 20, &[])).is_err());
        let mut v = LastSeenValidator::default();
        v.add_pending(sig(4));
        v.apply_offset(1).unwrap();
        let wrong = LastSeenUpdate {
            offset: 0,
            acknowledged: 1 << 19,
            checksum: last_seen_checksum(&[sig(4)]).wrapping_add(1),
        };
        assert!(v.apply_update(&wrong).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn signature_cache_packs_recent_signatures_and_evicts_the_oldest() {
        let mut cache = SignatureCache::default();
        assert_eq!(cache.pack(&sig(1)), None);
        cache.push(&[], Some(&sig(1)));
        cache.push(&[sig(1)], Some(&sig(2)));
        assert_eq!(cache.unpack(cache.pack(&sig(2)).unwrap()), Some(&sig(2)));
        assert!(cache.pack(&sig(1)).is_some());
        for n in 10..(10 + SIGNATURE_CACHE_SIZE as u8) {
            cache.push(&[], Some(&sig(n)));
        }
        assert_eq!(cache.pack(&sig(1)), None);
        assert!(
            cache
                .pack(&sig(10 + SIGNATURE_CACHE_SIZE as u8 - 1))
                .is_some()
        );
        assert_eq!(cache.unpack(SIGNATURE_CACHE_SIZE), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_chain_checks_signatures_order_and_expiry() {
        let (private, public) = pair();
        let sender = 5u128;
        let session = 6u128;
        let sign_next = |index: i32, text: &str, timestamp_ms: i64| {
            let body = MessageBody {
                content: text,
                timestamp_ms,
                salt: 3,
                last_seen: &[],
            };
            (
                sign(&private, &signed_payload(sender, session, index, &body)),
                timestamp_ms,
            )
        };
        let body = |text, timestamp_ms| MessageBody {
            content: text,
            timestamp_ms,
            salt: 3,
            last_seen: &[],
        };

        let mut chain = ChatChain::new(sender, session);
        let (s0, t0) = sign_next(0, "a", 5_000);
        assert_eq!(
            chain.unpack(&public, false, Some(&s0), &body("a", t0)),
            Ok(())
        );
        let (s1, t1) = sign_next(1, "b", 5_500);
        assert_eq!(
            chain.unpack(&public, false, None, &body("b", t1)),
            Err(ChatError::MissingProfileKey)
        );
        assert_eq!(
            chain.unpack(&public, true, Some(&s1), &body("b", t1)),
            Err(ChatError::ExpiredProfileKey)
        );
        assert_eq!(
            chain.unpack(&public, false, Some(&s1), &body("b", t1)),
            Ok(())
        );
        assert_eq!(
            chain.unpack(&public, false, Some(&s1), &body("b", t1)),
            Err(ChatError::InvalidSignature),
            "replay of an old link"
        );
        assert_eq!(
            chain.unpack(&public, false, Some(&s1), &body("b", t1)),
            Err(ChatError::ChainBroken)
        );

        let mut chain = ChatChain::new(sender, session);
        let (s0, _) = sign_next(0, "a", 5_000);
        assert_eq!(
            chain.unpack(&public, false, Some(&s0), &body("a", 5_000)),
            Ok(())
        );
        let (s1, _) = sign_next(1, "b", 4_000);
        assert_eq!(
            chain.unpack(&public, false, Some(&s1), &body("b", 4_000)),
            Err(ChatError::OutOfOrder)
        );
        assert_eq!(chain.skip(1, 6_000), Err(ChatError::ChainBroken));
    }

    #[tuff_core::watchdog]
    #[test]
    fn signed_payload_matches_the_wire_layout() {
        let body = MessageBody {
            content: "hi",
            timestamp_ms: 12_999,
            salt: -1,
            last_seen: &[sig(7)],
        };
        let payload = signed_payload(1, 2, 3, &body);
        assert_eq!(payload.len(), 4 + 16 + 16 + 4 + 8 + 8 + 4 + 2 + 4 + 256);
        assert_eq!(&payload[..4], &1i32.to_be_bytes());
        assert_eq!(&payload[36..40], &3i32.to_be_bytes());
        assert_eq!(&payload[48..56], &12i64.to_be_bytes());
        assert_eq!(&payload[56..60], &2i32.to_be_bytes());
        assert_eq!(&payload[60..62], b"hi");
        assert_eq!(&payload[62..66], &1i32.to_be_bytes());
    }

    #[tuff_core::watchdog]
    #[test]
    fn authority_keys_parse_and_validate_profile_keys() {
        use base64::Engine;
        let (authority, authority_public) = pair();
        let (_, player) = pair();
        let der = player.to_public_key_der().unwrap();
        let expires = 1_800_000_000_000i64;
        let mut payload = 9u128.to_be_bytes().to_vec();
        payload.extend_from_slice(&expires.to_be_bytes());
        payload.extend_from_slice(der.as_bytes());
        let session = ChatSession {
            session_id: 1,
            expires_at_ms: expires,
            public_key: der.as_bytes().to_vec(),
            key_signature: sign(&authority, &payload),
        };

        let authority_keys = ProfileKeyAuthority::default();
        assert!(!authority_keys.can_validate());
        assert!(!authority_keys.validate(9, &session));
        authority_keys.set_enforcing(true);
        assert!(!authority_keys.enforcing(), "nothing to check against");

        let listing = serde_json::json!({
            "profilePropertyKeys": [{"publicKey": "AAAA"}],
            "playerCertificateKeys": [
                {"publicKey": base64::engine::general_purpose::STANDARD
                    .encode(authority_public.to_public_key_der().unwrap().as_bytes())},
                {"publicKey": "not base64!"},
            ],
        });
        let keys = parse_authority_keys(listing.to_string().as_bytes());
        assert_eq!(keys.len(), 1);
        assert!(parse_authority_keys(b"{").is_empty());

        authority_keys.set_keys(keys);
        assert!(authority_keys.enforcing());
        assert!(authority_keys.validate(9, &session));
        assert!(
            !authority_keys.validate(10, &session),
            "bound to the account"
        );
    }
}
