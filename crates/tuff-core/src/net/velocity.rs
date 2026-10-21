//! Velocity "modern" player info forwarding: the proxy authenticates the
//! player and answers a login query with an HMAC-SHA256 signed identity.

use std::net::IpAddr;

use bytes::Bytes;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tuff_wire::{PacketReader, PacketWriter};

use crate::auth::{GameProfile, ProfileProperty};

pub const CHANNEL: &str = "velocity:player_info";
pub const ENABLED_KEY: &str = "tuff.proxy.velocity.enabled";
pub const SECRET_KEY: &str = "tuff.proxy.velocity.secret";
pub const SECRET_FILE_KEY: &str = "tuff.proxy.velocity.secretFile";

pub const VERSION_DEFAULT: u8 = 1;
pub const VERSION_WITH_KEY: u8 = 2;
pub const VERSION_WITH_KEY_V2: u8 = 3;
/// The newest version: no chat key fields, used for every current client.
pub const VERSION_LAZY_SESSION: u8 = 4;
pub const MAX_VERSION: u8 = VERSION_LAZY_SESSION;

const SIGNATURE_LEN: usize = 32;
/// Upper bound on a whole signed answer; real ones are a few KiB.
pub const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_ADDRESS_BYTES: usize = 255;
const MAX_NAME_BYTES: usize = 16 * 4;
const MAX_PROPERTIES: usize = 16;
const MAX_PROPERTY_NAME_BYTES: usize = 256;
const MAX_PROPERTY_VALUE_BYTES: usize = 32 * 1024;
const MAX_KEY_BYTES: usize = 512;
const MAX_KEY_SIGNATURE_BYTES: usize = 4096;

/// The proxy's shared secret. Its `Debug` form never shows the bytes.
#[derive(Clone)]
pub struct Secret(Bytes);

impl Secret {
    pub fn new(bytes: impl Into<Bytes>) -> Option<Self> {
        let bytes = bytes.into();
        (!bytes.is_empty()).then_some(Self(bytes))
    }

    /// The secret from `tuff.proxy.velocity.secret`, or else the first line
    /// of the file `tuff.proxy.velocity.secretFile` names (relative paths
    /// resolve against `base`).
    pub fn from_config(
        config: &crate::config::ConfigStore,
        base: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let inline = config.get_string(SECRET_KEY).unwrap_or_default();
        if let Some(secret) = Self::new(inline.trim().as_bytes().to_vec()) {
            return Ok(secret);
        }
        let file = config.get_string(SECRET_FILE_KEY).unwrap_or_default();
        anyhow::ensure!(
            !file.is_empty(),
            "{ENABLED_KEY} is set but neither {SECRET_KEY} nor {SECRET_FILE_KEY} is"
        );
        let mut path = std::path::PathBuf::from(&file);
        if path.is_relative()
            && let Some(base) = base
        {
            path = base.join(path);
        }
        let text = std::fs::read_to_string(&path).map_err(|err| {
            anyhow::anyhow!("could not read {SECRET_FILE_KEY} {}: {err}", path.display())
        })?;
        let line = text.lines().next().unwrap_or_default().trim();
        Self::new(line.as_bytes().to_vec())
            .ok_or_else(|| anyhow::anyhow!("{SECRET_FILE_KEY} {} is empty", path.display()))
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC takes keys of any length")
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The identity the proxy vouches for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarded {
    pub version: u8,
    pub address: IpAddr,
    pub profile: GameProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardError {
    TooLarge,
    BadSignature,
    Malformed(String),
    UnsupportedVersion(i32),
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("forwarding data is too large"),
            Self::BadSignature => f.write_str("forwarding data has a bad signature"),
            Self::Malformed(why) => write!(f, "forwarding data is malformed: {why}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported forwarding version {v}"),
        }
    }
}

impl std::error::Error for ForwardError {}

/// The login query's data: the newest forwarding version this server reads.
pub fn request_data() -> Bytes {
    Bytes::from_static(&[MAX_VERSION])
}

/// Checks the signature in constant time, then decodes the identity.
pub fn verify(secret: &Secret, data: &[u8]) -> Result<Forwarded, ForwardError> {
    if data.len() > MAX_PAYLOAD {
        return Err(ForwardError::TooLarge);
    }
    if data.len() < SIGNATURE_LEN {
        return Err(ForwardError::Malformed("shorter than a signature".into()));
    }
    let (signature, payload) = data.split_at(SIGNATURE_LEN);
    let mut mac = secret.mac();
    mac.update(payload);
    mac.verify_slice(signature)
        .map_err(|_| ForwardError::BadSignature)?;
    decode(payload).map_err(|err| match err {
        Decode::Wire(err) => ForwardError::Malformed(err.to_string()),
        Decode::Forward(err) => err,
    })
}

enum Decode {
    Wire(tuff_wire::BufferError),
    Forward(ForwardError),
}

impl From<tuff_wire::BufferError> for Decode {
    fn from(err: tuff_wire::BufferError) -> Self {
        Self::Wire(err)
    }
}

fn malformed(why: &str) -> Decode {
    Decode::Forward(ForwardError::Malformed(why.to_owned()))
}

fn decode(payload: &[u8]) -> Result<Forwarded, Decode> {
    let mut r = PacketReader::new(Bytes::copy_from_slice(payload));
    let version = r.read_var_int()?;
    let version = match u8::try_from(version) {
        Ok(v) if (VERSION_DEFAULT..=MAX_VERSION).contains(&v) => v,
        _ => return Err(Decode::Forward(ForwardError::UnsupportedVersion(version))),
    };
    let address = r.read_string_with_max(MAX_ADDRESS_BYTES)?;
    let address = parse_address(&address).ok_or_else(|| malformed("bad client address"))?;
    let id = r.read_uuid()?;
    let name = r.read_string_with_max(MAX_NAME_BYTES)?;
    if name.is_empty() || name.chars().count() > 16 {
        return Err(malformed("bad player name"));
    }
    let count = r.read_var_int()?;
    let count = usize::try_from(count)
        .ok()
        .filter(|&n| n <= MAX_PROPERTIES)
        .ok_or_else(|| malformed("bad property count"))?;
    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.read_string_with_max(MAX_PROPERTY_NAME_BYTES)?;
        let value = r.read_string_with_max(MAX_PROPERTY_VALUE_BYTES)?;
        let signature = if r.read_bool()? {
            Some(r.read_string_with_max(MAX_PROPERTY_VALUE_BYTES)?)
        } else {
            None
        };
        properties.push(ProfileProperty {
            name,
            value,
            signature,
        });
    }
    if (VERSION_WITH_KEY..VERSION_LAZY_SESSION).contains(&version) {
        r.read_long()?;
        r.read_prefixed_bytes(MAX_KEY_BYTES)?;
        r.read_prefixed_bytes(MAX_KEY_SIGNATURE_BYTES)?;
        if version >= VERSION_WITH_KEY_V2 && r.read_bool()? {
            r.read_uuid()?;
        }
    }
    if r.remaining() != 0 {
        return Err(malformed("trailing bytes"));
    }
    Ok(Forwarded {
        version,
        address,
        profile: GameProfile {
            id,
            name,
            properties,
        },
    })
}

fn parse_address(text: &str) -> Option<IpAddr> {
    let bare = text
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .unwrap_or(text);
    let bare = bare.split('%').next().unwrap_or(bare);
    bare.parse().ok()
}

/// Builds a signed answer the way a proxy does (for tests and tooling).
pub fn encode(secret: &Secret, forwarded: &Forwarded) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_var_int(i32::from(forwarded.version));
    w.write_string(&forwarded.address.to_string());
    w.write_uuid(forwarded.profile.id);
    w.write_string(&forwarded.profile.name);
    w.write_var_int(forwarded.profile.properties.len() as i32);
    for property in &forwarded.profile.properties {
        w.write_string(&property.name);
        w.write_string(&property.value);
        w.write_bool(property.signature.is_some());
        if let Some(signature) = &property.signature {
            w.write_string(signature);
        }
    }
    if (VERSION_WITH_KEY..VERSION_LAZY_SESSION).contains(&forwarded.version) {
        w.write_long(0);
        w.write_prefixed_bytes(&[]);
        w.write_prefixed_bytes(&[]);
        if forwarded.version >= VERSION_WITH_KEY_V2 {
            w.write_bool(false);
        }
    }
    let payload = w.into_bytes();
    let mut mac = secret.mac();
    mac.update(&payload);
    let mut out = mac.finalize().into_bytes().to_vec();
    out.extend_from_slice(&payload);
    out.into()
}

pub fn enabled(config: &crate::config::ConfigStore) -> bool {
    config.get_bool(ENABLED_KEY).unwrap_or(false)
}

/// The secret when forwarding is enabled; an error when it is enabled
/// without one.
pub fn configured(
    config: &crate::config::ConfigStore,
    base: Option<&std::path::Path>,
) -> anyhow::Result<Option<Secret>> {
    if !enabled(config) {
        return Ok(None);
    }
    Secret::from_config(config, base).map(Some)
}

fn is_loopback_bind(bind: &str) -> bool {
    if let Ok(addr) = bind.parse::<std::net::SocketAddr>() {
        return addr.ip().is_loopback();
    }
    bind.rsplit_once(':')
        .is_some_and(|(host, _)| host.eq_ignore_ascii_case("localhost"))
}

/// Offline mode lets anyone claim any name, so it stays on loopback unless
/// the owner opts in or a proxy vouches for identities.
pub fn refuse_public_offline(allowed: bool, binds: &[String]) -> anyhow::Result<()> {
    if allowed {
        return Ok(());
    }
    if let Some(public) = binds.iter().find(|bind| !is_loopback_bind(bind)) {
        anyhow::bail!(
            "refusing to start in offline mode on the non-loopback address {public}: set {} = true to allow it, enable {ENABLED_KEY} behind a Velocity proxy, or turn online-mode on",
            crate::config::ALLOW_PUBLIC_OFFLINE,
        );
    }
    Ok(())
}

const QUERY_ID: i32 = 0x7e1;

/// Asks the proxy for the player's identity; the inner error says why it
/// is missing or untrusted.
pub async fn forwarded_identity(
    secret: &Secret,
    conn: &mut crate::net::connection::Connection,
    name: &str,
) -> anyhow::Result<Result<Forwarded, tuff_item::text::TextComponent>> {
    Ok(query(secret, conn).await?.map_err(|reason| {
        log::info!("refused a login for {name}: {reason}");
        tuff_item::text::TextComponent::new(
            "This server only accepts players through its proxy (Velocity modern forwarding)",
        )
    }))
}

async fn query(
    secret: &Secret,
    conn: &mut crate::net::connection::Connection,
) -> anyhow::Result<Result<Forwarded, String>> {
    use crate::protocol::login::{LoginQuery, LoginQueryAnswer};
    conn.send(&LoginQuery {
        transaction_id: QUERY_ID,
        channel: CHANNEL.to_owned(),
        data: request_data(),
    })
    .await?;
    let answer: LoginQueryAnswer = conn.expect().await?;
    if answer.transaction_id != QUERY_ID {
        return Ok(Err(
            "the forwarding answer has the wrong transaction id".into()
        ));
    }
    let Some(data) = answer.data else {
        return Ok(Err(
            "no forwarding data (not connected through a Velocity proxy with modern forwarding)"
                .into(),
        ));
    };
    Ok(verify(secret, &data).map_err(|err| err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(version: u8) -> Forwarded {
        Forwarded {
            version,
            address: "2001:db8::7".parse().unwrap(),
            profile: GameProfile {
                id: 0x069a79f444e94726a5befca90e38aaf5,
                name: "Notch".into(),
                properties: vec![ProfileProperty {
                    name: "textures".into(),
                    value: "dGV4dHVyZXM=".into(),
                    signature: Some("c2ln".into()),
                }],
            },
        }
    }

    fn secret() -> Secret {
        Secret::new(&b"hunter2"[..]).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_version_roundtrips() {
        for version in VERSION_DEFAULT..=MAX_VERSION {
            let forwarded = sample(version);
            assert_eq!(
                verify(&secret(), &encode(&secret(), &forwarded)),
                Ok(forwarded)
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wrong_secret_or_flipped_byte_fails_the_signature() {
        let data = encode(&secret(), &sample(MAX_VERSION));
        let other = Secret::new(&b"hunter3"[..]).unwrap();
        assert_eq!(verify(&other, &data), Err(ForwardError::BadSignature));
        let mut flipped = data.to_vec();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(verify(&secret(), &flipped), Err(ForwardError::BadSignature));
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_oversized_and_unknown_versions_are_refused() {
        let data = encode(&secret(), &sample(MAX_VERSION));
        assert!(matches!(
            verify(&secret(), &data[..10]),
            Err(ForwardError::Malformed(_))
        ));
        assert_eq!(
            verify(&secret(), &vec![0; MAX_PAYLOAD + 1]),
            Err(ForwardError::TooLarge)
        );
        let resign = |payload: &[u8]| {
            let mut mac = secret().mac();
            mac.update(payload);
            let mut out = mac.finalize().into_bytes().to_vec();
            out.extend_from_slice(payload);
            out
        };
        let payload = &data[SIGNATURE_LEN..];
        assert!(matches!(
            verify(&secret(), &resign(&payload[..payload.len() - 3])),
            Err(ForwardError::Malformed(_))
        ));
        let mut huge_count = PacketWriter::new();
        huge_count.write_var_int(4);
        huge_count.write_string("1.2.3.4");
        huge_count.write_uuid(1);
        huge_count.write_string("a");
        huge_count.write_var_int(i32::MAX);
        assert!(matches!(
            verify(&secret(), &resign(&huge_count.into_bytes())),
            Err(ForwardError::Malformed(_))
        ));
        let mut future = PacketWriter::new();
        future.write_var_int(9);
        assert_eq!(
            verify(&secret(), &resign(&future.into_bytes())),
            Err(ForwardError::UnsupportedVersion(9))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_secret_never_prints() {
        assert!(!format!("{:?}", secret()).contains("hunter"));
    }
}
